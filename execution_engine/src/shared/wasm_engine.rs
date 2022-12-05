//! Preprocessing of Wasm modules.
use casper_types::{
    account::{self, AccountHash},
    api_error,
    bytesrepr::{self, Bytes, FromBytes, ToBytes},
    contracts::ContractPackageStatus,
    AccessRights, ApiError, CLValue, ContractPackageHash, ContractVersion, Gas, Key,
    ProtocolVersion, StoredValue, URef, U512,
};
use itertools::Itertools;
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use once_cell::sync::Lazy;
use parity_wasm::elements::{
    self, External, Instruction, Internal, MemorySection, Section, TableType, Type,
};
use pwasm_utils::{self, stack_height};
use rand::{distributions::Standard, prelude::*, Rng};
use serde::{Deserialize, Serialize};
use std::{
    borrow::{BorrowMut, Cow},
    cell::{Cell, RefCell},
    collections::{hash_map::Entry, HashMap},
    error::Error,
    fmt::{self, Display, Formatter},
    fs::{self, File, OpenOptions},
    io::Write,
    ops::DerefMut,
    path::Path,
    rc::Rc,
    sync::{Arc, Mutex, Once, RwLock},
    time::{Duration, Instant},
};
use thiserror::Error;
use tracing::Subscriber;
use wasmer::{imports, AsStoreMut, AsStoreRef, FunctionEnv, FunctionEnvMut, TypedFunction};
use wasmer_compiler_singlepass::Singlepass;

const DEFAULT_GAS_MODULE_NAME: &str = "env";
/// Name of the internal gas function injected by [`pwasm_utils::inject_gas_counter`].
const INTERNAL_GAS_FUNCTION_NAME: &str = "gas";

/// We only allow maximum of 4k function pointers in a table section.
pub const DEFAULT_MAX_TABLE_SIZE: u32 = 4096;
/// Maximum number of elements that can appear as immediate value to the br_table instruction.
pub const DEFAULT_BR_TABLE_MAX_SIZE: u32 = 256;
/// Maximum number of global a module is allowed to declare.
pub const DEFAULT_MAX_GLOBALS: u32 = 256;
/// Maximum number of parameters a function can have.
pub const DEFAULT_MAX_PARAMETER_COUNT: u32 = 256;

use parity_wasm::elements::Module as WasmiModule;
use wasmi::{ImportsBuilder, MemoryRef, ModuleInstance, ModuleRef};
use wasmtime::{
    AsContext, AsContextMut, Caller, Extern, ExternType, InstanceAllocationStrategy, Memory,
    MemoryType, StoreContextMut, Trap,
};

use crate::{
    core::{
        execution,
        resolvers::{
            create_module_resolver, memory_resolver::MemoryResolver,
            v1_function_index::FunctionIndex,
        },
        runtime::{externals::WasmiExternals, Runtime, RuntimeStack},
    },
    shared::host_function_costs,
    storage::global_state::StateReader,
};

use super::wasm_config::WasmConfig;

/// Mode of execution for smart contracts.
#[derive(Debug, Copy, Clone, PartialEq, Eq, FromPrimitive, ToPrimitive, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Runs Wasm modules in interpreted mode.
    Interpreted = 1,
    /// Runs Wasm modules in a compiled mode.
    Compiled = 2,
    /// Runs Wasm modules in a JIT mode.
    JustInTime = 3,
    /// Runs Wasm modules in a compiled mode via fast ahead of time compiler.
    Singlepass,
}

impl ToBytes for ExecutionMode {
    fn to_bytes(&self) -> Result<Vec<u8>, bytesrepr::Error> {
        self.to_u32().unwrap().to_bytes()
    }

    fn serialized_length(&self) -> usize {
        self.to_u32().unwrap().serialized_length()
    }
}

impl FromBytes for ExecutionMode {
    fn from_bytes(bytes: &[u8]) -> Result<(Self, &[u8]), bytesrepr::Error> {
        let (execution_mode, rem) = u32::from_bytes(bytes)?;
        let execution_mode =
            ExecutionMode::from_u32(execution_mode).ok_or(bytesrepr::Error::Formatting)?;
        Ok((execution_mode, rem))
    }
}

impl Distribution<ExecutionMode> for Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> ExecutionMode {
        match rng.gen::<bool>() {
            false => ExecutionMode::Interpreted,
            true => ExecutionMode::Compiled,
        }
    }
}

fn deserialize_interpreted(wasm_bytes: &[u8]) -> Result<WasmiModule, PreprocessingError> {
    parity_wasm::elements::deserialize_buffer(wasm_bytes).map_err(PreprocessingError::from)
}

/// Statically dispatched Wasm module wrapper for different implementations
/// NOTE: Probably at this stage it can hold raw wasm bytes without being an enum, and the decision
/// can be made later
#[derive(Clone)]
pub enum Module {
    /// TODO: We might not need it anymore. We use "do nothing" bytes for replacing wasm modules
    Noop,
    Interpreted {
        original_bytes: Vec<u8>,
        wasmi_module: WasmiModule,
    },
    Compiled {
        original_bytes: Vec<u8>,
        /// Used to carry on original Wasm module for easy further processing.
        wasmi_module: WasmiModule,
        precompile_time: Option<Duration>,
        /// Ahead of time compiled artifact.
        compiled_artifact: Vec<u8>,
    },
    Jitted {
        original_bytes: Vec<u8>,
        wasmi_module: WasmiModule,
        precompile_time: Option<Duration>,
        module: wasmtime::Module,
    },
    Singlepass {
        original_bytes: Vec<u8>,
        wasmi_module: WasmiModule,
        // store: wasmer::Store,
        wasmer_module: wasmer::Module,
    },
}

impl fmt::Debug for Module {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Noop => f.debug_tuple("Noop").finish(),
            Self::Interpreted { wasmi_module, .. } => {
                f.debug_tuple("Interpreted").field(wasmi_module).finish()
            }
            Self::Compiled { .. } => f.debug_tuple("Compiled").finish(),
            Self::Jitted { .. } => f.debug_tuple("Jitted").finish(),
            Module::Singlepass { .. } => f.debug_tuple("Singlepass").finish(),
        }
    }
}

impl Module {
    pub fn try_into_interpreted(self) -> Result<WasmiModule, Self> {
        if let Self::Interpreted { wasmi_module, .. } = self {
            Ok(wasmi_module)
        } else {
            Err(self)
        }
    }

    /// Consumes self, and returns an [`WasmiModule`] regardless of an implementation.
    ///
    /// Such module can be useful for performing optimizations. To create a [`Module`] back use
    /// appropriate [`WasmEngine`] method.
    pub fn into_interpreted(self) -> WasmiModule {
        match self {
            Module::Noop => unimplemented!("Attempting to get interpreted module from noop"),
            Module::Interpreted {
                original_bytes,
                wasmi_module,
            } => wasmi_module,
            Module::Compiled { wasmi_module, .. } => wasmi_module,
            Module::Jitted { wasmi_module, .. } => wasmi_module,
            Module::Singlepass { wasmi_module, .. } => wasmi_module,
        }
    }
}

#[derive(Debug)]
pub enum LogProperty {
    Size(usize),
    PrecompileTime(Duration),
    Runtime(Duration),
    EntryPoint(String),
    Finish,
}

impl Display for LogProperty {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LogProperty::Size(value) => write!(f, "size={}", value),
            LogProperty::PrecompileTime(value) => {
                write!(f, "precompile_time_us={}", value.as_micros())
            }
            LogProperty::Runtime(value) => write!(f, "runtime_us={}", value.as_micros()),
            LogProperty::EntryPoint(value) => write!(f, "entrypoint={}", value),
            LogProperty::Finish => write!(f, "finish"),
        }
    }
}

pub fn record_performance_log(wasm_bytes: &[u8], log_property: Vec<LogProperty>) {
    let mut f = OpenOptions::new()
        .read(false)
        .append(true)
        .create(true)
        .open("performance_log.txt")
        .expect("should open file");

    let wasm_id = account::blake2b(wasm_bytes);

    let properties = log_property
        .into_iter()
        .map(|prop| prop.to_string())
        .join(",");

    let data = format!("{},{}\n", base16::encode_lower(&wasm_id), properties);
    f.write(data.as_bytes()).expect("should write");
    f.sync_all().expect("should sync");
}

/// Common error type adapter for all Wasm engines supported.
#[derive(Error, Clone, Debug)]
pub enum RuntimeError {
    #[error(transparent)]
    WasmiError(Arc<wasmi::Error>),
    #[error(transparent)]
    WasmtimeError(#[from] wasmtime::Trap),
    #[error(transparent)]
    WasmerError(#[from] wasmer::RuntimeError),
    #[error(transparent)]
    MemoryAccessError(#[from] wasmer::MemoryAccessError),
    #[error("{0}")]
    Other(String),
}

impl From<wasmi::Error> for RuntimeError {
    fn from(error: wasmi::Error) -> Self {
        RuntimeError::WasmiError(Arc::new(error))
    }
}

impl From<String> for RuntimeError {
    fn from(string: String) -> Self {
        Self::Other(string)
    }
}

impl RuntimeError {
    /// Extracts the executor error from a runtime Wasm trap.
    pub fn into_execution_error(self) -> Result<execution::Error, Self> {
        match &self {
            RuntimeError::WasmiError(wasmi_error) => {
                match wasmi_error
                    .as_host_error()
                    .and_then(|host_error| host_error.downcast_ref::<execution::Error>())
                {
                    Some(execution_error) => Ok(execution_error.clone()),
                    None => Err(self),
                }
                // Ok(error)
            }
            RuntimeError::WasmtimeError(wasmtime_trap) => {
                match wasmtime_trap
                    .source()
                    .and_then(|src| src.downcast_ref::<execution::Error>())
                {
                    Some(execution_error) => Ok(execution_error.clone()),
                    None => Err(self),
                }
            }
            RuntimeError::WasmerError(wasmer_runtime_error) => {
                match wasmer_runtime_error
                    // .clone()
                    .source()
                    .and_then(|src| src.downcast_ref::<execution::Error>())
                {
                    Some(execution_error) => Ok(execution_error.clone()),
                    None => Err(self),
                }
            }
            RuntimeError::Other(_) => Err(self),
            RuntimeError::MemoryAccessError(_) => Err(self),
        }
    }
}

impl Into<String> for RuntimeError {
    fn into(self) -> String {
        self.to_string()
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum RuntimeValue {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl From<wasmi::RuntimeValue> for RuntimeValue {
    fn from(runtime_value: wasmi::RuntimeValue) -> Self {
        match runtime_value {
            wasmi::RuntimeValue::I32(value) => RuntimeValue::I32(value),
            wasmi::RuntimeValue::I64(value) => RuntimeValue::I64(value),
            // NOTE: Why wasmi implements F32/F64 newtypes? Is F64->f64 conversion safe even though
            // they claim they're compatible with rust's IEEE 754-2008?
            wasmi::RuntimeValue::F32(value) => RuntimeValue::F32(value.to_float()),
            wasmi::RuntimeValue::F64(value) => RuntimeValue::F64(value.to_float()),
        }
    }
}

impl Into<wasmi::RuntimeValue> for RuntimeValue {
    fn into(self) -> wasmi::RuntimeValue {
        match self {
            RuntimeValue::I32(value) => wasmi::RuntimeValue::I32(value),
            RuntimeValue::I64(value) => wasmi::RuntimeValue::I64(value),
            RuntimeValue::F32(value) => wasmi::RuntimeValue::F32(value.into()),
            RuntimeValue::F64(value) => wasmi::RuntimeValue::F64(value.into()),
        }
    }
}

/// Warmed up instance of a wasm module used for execution.
#[derive(Clone)]
pub enum Instance<R>
where
    R: Send + Sync + 'static + Clone + StateReader<Key, StoredValue>,
    R::Error: Into<execution::Error>,
{
    // TODO: We might not need it. Actually trying to execute a noop is a programming error.
    Noop,
    Interpreted {
        original_bytes: Vec<u8>,
        module: ModuleRef,
        memory: MemoryRef,
        runtime: Runtime<R>,
    },
    // NOTE: Instance should contain wasmtime::Instance instead but we need to hold Store that has
    // a lifetime and a generic R
    Compiled {
        /// For metrics
        original_bytes: Vec<u8>,
        precompile_time: Option<Duration>,
        /// Raw Wasmi module used only for further processing.
        module: WasmiModule,
        /// This is compiled module used for execution.
        compiled_module: wasmtime::Module,
        runtime: Runtime<R>,
    },
    Singlepass {
        original_bytes: Vec<u8>,
        module: WasmiModule,
        /// There is no separate "compile" step that turns a wasm bytes into a wasmer module
        wasmer_module: wasmer::Module,
        runtime: Runtime<R>,
    },
}

unsafe impl<R> Send for Instance<R>
where
    R: Send + Sync + 'static + Clone + StateReader<Key, StoredValue>,
    R::Error: Into<execution::Error>,
{
}
// unsafe impl<R> Sync for Instance<R> where R: Sync {}

impl<R> fmt::Debug for Instance<R>
where
    R: Send + Sync + 'static + Clone + StateReader<Key, StoredValue>,
    R::Error: Into<execution::Error>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Noop => f.debug_tuple("Noop").finish(),
            Self::Interpreted { module, memory, .. } => f
                .debug_tuple("Interpreted")
                .field(module)
                .field(memory)
                .finish(),
            Self::Compiled { .. } => f.debug_tuple("Compiled").finish(),
            Self::Singlepass { .. } => f.debug_tuple("Singlepass").finish(),
        }
    }
}

// #[derive(Debug, Clone)]
// pub struct InstanceRef<R>(Rc<Instance>);

// impl From<Instance> for InstanceRef {
//     fn from(instance: Instance) -> Self {
//         InstanceRef(Rc::new(instance))
//     }
// }

pub trait FunctionContext {
    fn memory_read(&self, offset: u32, size: usize) -> Result<Vec<u8>, RuntimeError>;
    fn memory_write(&mut self, offset: u32, data: &[u8]) -> Result<(), RuntimeError>;
}

pub struct WasmiAdapter {
    memory: wasmi::MemoryRef,
}

impl WasmiAdapter {
    pub fn new(memory: wasmi::MemoryRef) -> Self {
        Self { memory }
    }
}

impl FunctionContext for WasmiAdapter {
    fn memory_read(&self, offset: u32, size: usize) -> Result<Vec<u8>, RuntimeError> {
        Ok(self.memory.get(offset, size)?)
    }

    fn memory_write(&mut self, offset: u32, data: &[u8]) -> Result<(), RuntimeError> {
        self.memory.set(offset, data)?;
        Ok(())
    }
}

/// Wasm caller object passed as an argument for each.
pub struct WasmtimeAdapter<'a> {
    data: &'a mut [u8],
}

impl<'a> FunctionContext for WasmtimeAdapter<'a> {
    fn memory_read(&self, offset: u32, size: usize) -> Result<Vec<u8>, RuntimeError> {
        let mut buffer = vec![0; size];

        let slice = self
            .data
            .get(offset as usize..)
            .and_then(|s| s.get(..buffer.len()))
            .ok_or_else(|| Trap::new("memory access"))?;
        buffer.copy_from_slice(slice);
        Ok(buffer)
    }

    fn memory_write(&mut self, offset: u32, buffer: &[u8]) -> Result<(), RuntimeError> {
        self.data
            .get_mut(offset as usize..)
            .and_then(|s| s.get_mut(..buffer.len()))
            .ok_or_else(|| Trap::new("memory access"))?
            .copy_from_slice(buffer);
        Ok(())
    }
}

struct WasmerAdapter<'a>(wasmer::MemoryView<'a>);
impl<'a> WasmerAdapter<'a> {
    fn new(memory_view: wasmer::MemoryView<'a>) -> Self {
        Self(memory_view)
    }
}

impl<'a> FunctionContext for WasmerAdapter<'a> {
    fn memory_read(&self, offset: u32, size: usize) -> Result<Vec<u8>, RuntimeError> {
        let mut vec = vec![0; size];
        self.0.read(offset as u64, &mut vec)?;
        Ok(vec)
    }

    fn memory_write(&mut self, offset: u32, data: &[u8]) -> Result<(), RuntimeError> {
        self.0.write(offset as u64, data)?;
        Ok(())
    }
}

fn caller_adapter_and_runtime<'b, 'c, 'd: 'c, R: Clone>(
    caller: &'c mut Caller<'d, Runtime<R>>,
) -> (WasmtimeAdapter<'c>, &'c mut Runtime<R>) {
    let mem = caller.data().wasmtime_memory;
    let (data, runtime) = mem
        .expect("Memory should have been initialized.")
        .data_and_store_mut(caller);
    (WasmtimeAdapter { data }, runtime)
}

struct WasmerEnv<R: Clone> {
    runtime: Box<Runtime<R>>,
    // memory: wasmer::Memory,
}

impl<R> Instance<R>
where
    R: Send + Sync + 'static + Clone + StateReader<Key, StoredValue>,
    R::Error: Into<execution::Error>,
{
    pub fn interpreted_memory(&self) -> MemoryRef {
        match self {
            Instance::Noop => unimplemented!("Unable to get interpreted memory from noop module"),
            Instance::Interpreted { memory, .. } => memory.clone(),
            Instance::Compiled { .. } => unreachable!("available only from wasmi externals"),
            Instance::Singlepass { .. } => unreachable!("available only from wasmi externals"),
        }
    }
    /// Invokes exported function
    pub fn invoke_export(
        self,
        wasm_engine: &WasmEngine,
        func_name: &str,
        args: Vec<RuntimeValue>,
    ) -> Result<Option<RuntimeValue>, RuntimeError> {
        match self {
            Instance::Noop => {
                unimplemented!("Unable to execute export {} on noop instance", func_name)
            }
            Instance::Interpreted {
                original_bytes,
                module,
                memory,
                mut runtime,
            } => {
                let wasmi_args: Vec<wasmi::RuntimeValue> =
                    args.into_iter().map(|value| value.into()).collect();

                // record_performance_log(&original_bytes,
                // LogProperty::EntryPoint(func_name.to_string()));

                // get the runtime value

                let start = Instant::now();

                let mut wasmi_externals = WasmiExternals {
                    runtime: &mut runtime,
                    module: module.clone(),
                    memory: memory.clone(),
                };

                let wasmi_result =
                    module.invoke_export(func_name, wasmi_args.as_slice(), &mut wasmi_externals);

                let stop = start.elapsed();
                record_performance_log(
                    &original_bytes,
                    vec![
                        LogProperty::Size(original_bytes.len()),
                        LogProperty::EntryPoint(func_name.to_string()),
                        LogProperty::PrecompileTime(Duration::default()),
                        LogProperty::Runtime(stop),
                    ],
                );

                let wasmi_result = wasmi_result?;

                // wasmi's runtime value into our runtime value
                let result = wasmi_result.map(RuntimeValue::from);
                Ok(result)
            }
            Instance::Compiled {
                original_bytes,
                module,
                mut compiled_module,
                precompile_time,
                runtime,
            } => {
                // record_performance_log(&original_bytes,
                // LogProperty::EntryPoint(func_name.to_string()));

                let mut store = wasmtime::Store::new(&wasm_engine.compiled_engine, runtime);

                let mut linker = wasmtime::Linker::new(&wasm_engine.compiled_engine);

                let memory_import = compiled_module.imports().find_map(|import| {
                    if (import.module(), import.name()) == ("env", Some("memory")) {
                        Some(import.ty())
                    } else {
                        None
                    }
                });

                let memory_type = match memory_import {
                    Some(ExternType::Memory(memory)) => memory,
                    Some(unknown_extern) => panic!("unexpected extern {:?}", unknown_extern),
                    None => MemoryType::new(1, Some(wasm_engine.wasm_config().max_memory)),
                };

                debug_assert_eq!(
                    memory_type.maximum(),
                    Some(wasm_engine.wasm_config().max_memory as u64)
                );

                let memory = wasmtime::Memory::new(&mut store, memory_type).unwrap();
                store.data_mut().wasmtime_memory = Some(memory);

                linker
                    .define("env", "memory", wasmtime::Extern::from(memory))
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_read_value",
                        |mut caller: Caller<Runtime<R>>,
                         key_ptr: u32,
                         key_size: u32,
                         output_size_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.read(
                                function_context,
                                key_ptr,
                                key_size,
                                output_size_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_add",
                        |mut caller: Caller<Runtime<R>>,
                         key_ptr: u32,
                         key_size: u32,
                         value_ptr: u32,
                         value_size: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            runtime.casper_add(
                                function_context,
                                key_ptr,
                                key_size,
                                value_ptr,
                                value_size,
                            )?;
                            Ok(())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_revert",
                        |mut caller: Caller<Runtime<R>>, param: u32| {
                            caller.data_mut().casper_revert(param)?;
                            Ok(()) //unreachable
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_ret",
                        |mut caller: Caller<Runtime<R>>, value_ptr: u32, value_size: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();
                            let error = runtime.ret(function_context, value_ptr, value_size);
                            Result::<(), _>::Err(error.into())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_get_phase",
                        |mut caller: Caller<Runtime<R>>, dest_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();
                            runtime.charge_host_function_call(
                                &host_function_costs.get_phase,
                                [dest_ptr],
                            )?;
                            runtime.get_phase(function_context, dest_ptr)?;
                            Ok(())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_is_valid_uref",
                        |mut caller: Caller<Runtime<R>>, uref_ptr: u32, uref_size: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret =
                                runtime.is_valid_uref(function_context, uref_ptr, uref_size)?;
                            Ok(i32::from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_add_associated_key",
                        |mut caller: Caller<Runtime<R>>,
                         account_hash_ptr: u32,
                         account_hash_size: u32,
                         weight: i32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.add_associated_key(
                                function_context,
                                account_hash_ptr,
                                account_hash_size as usize,
                                weight as u8,
                            )?;
                            Ok(ret)
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_remove_associated_key",
                        |mut caller: Caller<Runtime<R>>,
                         account_hash_ptr: u32,
                         account_hash_size: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.remove_associated_key(
                                function_context,
                                account_hash_ptr,
                                account_hash_size as usize,
                            )?;
                            Ok(ret)
                            // Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_update_associated_key",
                        |mut caller: Caller<Runtime<R>>,
                         account_hash_ptr: u32,
                         account_hash_size: u32,
                         weight: i32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.update_associated_key(
                                function_context,
                                account_hash_ptr,
                                account_hash_size as usize,
                                weight as u8,
                            )?;
                            Ok(ret)
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_set_action_threshold",
                        |mut caller: Caller<Runtime<R>>,
                         permission_level: u32,
                         permission_threshold: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.set_action_threshold(
                                function_context,
                                permission_level,
                                permission_threshold as u8,
                            )?;
                            Ok(ret)
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_get_caller",
                        |mut caller: Caller<Runtime<R>>, output_size_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.get_caller(function_context, output_size_ptr)?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_get_blocktime",
                        |mut caller: Caller<Runtime<R>>, dest_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            runtime.get_blocktime(function_context, dest_ptr)?;
                            Ok(())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "gas",
                        |mut caller: Caller<Runtime<R>>, param: u32| {
                            caller.data_mut().gas(Gas::new(U512::from(param)))?;
                            Ok(())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_new_uref",
                        |mut caller: Caller<Runtime<R>>, uref_ptr, value_ptr, value_size| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            runtime.casper_new_uref(
                                function_context,
                                uref_ptr,
                                value_ptr,
                                value_size,
                            )?;
                            Ok(())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_create_purse",
                        |mut caller: Caller<Runtime<R>>, dest_ptr: u32, dest_size: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.casper_create_purse(
                                function_context,
                                dest_ptr,
                                dest_size,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_write",
                        |mut caller: Caller<Runtime<R>>,
                         key_ptr: u32,
                         key_size: u32,
                         value_ptr: u32,
                         value_size: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            runtime.casper_write(
                                function_context,
                                key_ptr,
                                key_size,
                                value_ptr,
                                value_size,
                            )?;
                            Ok(())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_get_main_purse",
                        |mut caller: Caller<Runtime<R>>, dest_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            runtime.casper_get_main_purse(function_context, dest_ptr)?;
                            Ok(())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_get_named_arg_size",
                        |mut caller: Caller<Runtime<R>>,
                         name_ptr: u32,
                         name_size: u32,
                         size_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.casper_get_named_arg_size(
                                function_context,
                                name_ptr,
                                name_size,
                                size_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_get_named_arg",
                        |mut caller: Caller<Runtime<R>>,
                         name_ptr: u32,
                         name_size: u32,
                         dest_ptr: u32,
                         dest_size: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.casper_get_named_arg(
                                function_context,
                                name_ptr,
                                name_size,
                                dest_ptr,
                                dest_size,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_transfer_to_account",
                        |mut caller: Caller<Runtime<R>>,
                         key_ptr: u32,
                         key_size: u32,
                         amount_ptr: u32,
                         amount_size: u32,
                         id_ptr: u32,
                         id_size: u32,
                         result_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.casper_transfer_to_account(
                                function_context,
                                key_ptr,
                                key_size,
                                amount_ptr,
                                amount_size,
                                id_ptr,
                                id_size,
                                result_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_has_key",
                        |mut caller: Caller<Runtime<R>>, name_ptr: u32, name_size: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.has_key(function_context, name_ptr, name_size)?;
                            Ok(ret)
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_get_key",
                        |mut caller: Caller<Runtime<R>>,
                         name_ptr: u32,
                         name_size: u32,
                         output_ptr: u32,
                         output_size: u32,
                         bytes_written: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.load_key(
                                function_context,
                                name_ptr,
                                name_size,
                                output_ptr,
                                output_size as usize,
                                bytes_written,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_put_key",
                        |mut caller: Caller<Runtime<R>>, name_ptr, name_size, key_ptr, key_size| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            runtime.casper_put_key(
                                function_context,
                                name_ptr,
                                name_size,
                                key_ptr,
                                key_size,
                            )?;
                            Ok(())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_remove_key",
                        |mut caller: Caller<Runtime<R>>, name_ptr: u32, name_size: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            runtime.remove_key(function_context, name_ptr, name_size)?;
                            Ok(())
                        },
                    )
                    .unwrap();

                #[cfg(feature = "test-support")]
                linker
                    .func_wrap(
                        "env",
                        "casper_print",
                        |mut caller: Caller<Runtime<R>>, text_ptr: u32, text_size: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            runtime.casper_print(function_context, text_ptr, text_size)?;
                            Ok(())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_transfer_from_purse_to_purse",
                        |mut caller: Caller<Runtime<R>>,
                         source_ptr,
                         source_size,
                         target_ptr,
                         target_size,
                         amount_ptr,
                         amount_size,
                         id_ptr,
                         id_size| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.casper_transfer_from_purse_to_purse(
                                function_context,
                                source_ptr,
                                source_size,
                                target_ptr,
                                target_size,
                                amount_ptr,
                                amount_size,
                                id_ptr,
                                id_size,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_transfer_from_purse_to_account",
                        |mut caller: Caller<Runtime<R>>,
                         source_ptr,
                         source_size,
                         key_ptr,
                         key_size,
                         amount_ptr,
                         amount_size,
                         id_ptr,
                         id_size,
                         result_ptr| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.casper_transfer_from_purse_to_account(
                                function_context,
                                source_ptr,
                                source_size,
                                key_ptr,
                                key_size,
                                amount_ptr,
                                amount_size,
                                id_ptr,
                                id_size,
                                result_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_get_balance",
                        |mut caller: Caller<Runtime<R>>,
                         ptr: u32,
                         ptr_size: u32,
                         output_size_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();

                            runtime.charge_host_function_call(
                                &host_function_costs.get_balance,
                                [ptr, ptr_size, output_size_ptr],
                            )?;
                            let ret = runtime.get_balance_host_buffer(
                                function_context,
                                ptr,
                                ptr_size as usize,
                                output_size_ptr,
                            )?;

                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_read_host_buffer",
                        |mut caller: Caller<Runtime<R>>, dest_ptr, dest_size, bytes_written_ptr| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();

                            runtime.charge_host_function_call(
                                &host_function_costs.read_host_buffer,
                                [dest_ptr, dest_size, bytes_written_ptr],
                            )?;

                            let ret = runtime.read_host_buffer(
                                function_context,
                                dest_ptr,
                                dest_size as usize,
                                bytes_written_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_get_system_contract",
                        |mut caller: Caller<Runtime<R>>,
                         system_contract_index,
                         dest_ptr,
                         dest_size| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();

                            runtime.charge_host_function_call(
                                &host_function_costs.get_system_contract,
                                [system_contract_index, dest_ptr, dest_size],
                            )?;
                            let ret = runtime.get_system_contract(
                                function_context,
                                system_contract_index,
                                dest_ptr,
                                dest_size,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_load_named_keys",
                        |mut caller: Caller<Runtime<R>>, total_keys_ptr, result_size_ptr| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();

                            runtime.charge_host_function_call(
                                &host_function_costs.load_named_keys,
                                [total_keys_ptr, result_size_ptr],
                            )?;
                            let ret = runtime.load_named_keys(
                                function_context,
                                total_keys_ptr,
                                result_size_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_create_contract_package_at_hash",
                        |mut caller: Caller<Runtime<R>>,
                         hash_dest_ptr,
                         access_dest_ptr,
                         is_locked: u32| {
                            let (mut function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();

                            runtime.charge_host_function_call(
                                &host_function_costs.create_contract_package_at_hash,
                                [hash_dest_ptr, access_dest_ptr],
                            )?;
                            let package_status = ContractPackageStatus::new(is_locked != 0);
                            let (hash_addr, access_addr) =
                                runtime.create_contract_package_at_hash(package_status)?;

                            runtime.function_address(
                                &mut function_context,
                                hash_addr,
                                hash_dest_ptr,
                            )?;
                            runtime.function_address(
                                &mut function_context,
                                access_addr,
                                access_dest_ptr,
                            )?;

                            Ok(())
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_create_contract_user_group",
                        |mut caller: Caller<Runtime<R>>,
                         package_key_ptr: u32,
                         package_key_size: u32,
                         label_ptr: u32,
                         label_size: u32,
                         num_new_urefs: u32,
                         existing_urefs_ptr: u32,
                         existing_urefs_size: u32,
                         output_size_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let ret = runtime.casper_create_contract_user_group(
                                function_context,
                                package_key_ptr,
                                package_key_size,
                                label_ptr,
                                label_size,
                                num_new_urefs,
                                existing_urefs_ptr,
                                existing_urefs_size,
                                output_size_ptr,
                            )?;

                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_provision_contract_user_group_uref",
                        |mut caller: Caller<Runtime<R>>,
                         package_ptr,
                         package_size,
                         label_ptr,
                         label_size,
                         value_size_ptr| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();

                            runtime.charge_host_function_call(
                                &host_function_costs.provision_contract_user_group_uref,
                                [
                                    package_ptr,
                                    package_size,
                                    label_ptr,
                                    label_size,
                                    value_size_ptr,
                                ],
                            )?;
                            let ret = runtime.provision_contract_user_group_uref(
                                function_context,
                                package_ptr,
                                package_size,
                                label_ptr,
                                label_size,
                                value_size_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();
                linker
                    .func_wrap(
                        "env",
                        "casper_remove_contract_user_group",
                        |mut caller: Caller<Runtime<R>>,
                         package_key_ptr,
                         package_key_size,
                         label_ptr,
                         label_size| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.casper_remove_contract_user_group(
                                function_context,
                                package_key_ptr,
                                package_key_size,
                                label_ptr,
                                label_size,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_remove_contract_user_group_urefs",
                        |mut caller: Caller<Runtime<R>>,
                         package_ptr,
                         package_size,
                         label_ptr,
                         label_size,
                         urefs_ptr,
                         urefs_size| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();

                            runtime.charge_host_function_call(
                                &host_function_costs.remove_contract_user_group_urefs,
                                [
                                    package_ptr,
                                    package_size,
                                    label_ptr,
                                    label_size,
                                    urefs_ptr,
                                    urefs_size,
                                ],
                            )?;
                            let ret = runtime.remove_contract_user_group_urefs(
                                function_context,
                                package_ptr,
                                package_size,
                                label_ptr,
                                label_size,
                                urefs_ptr,
                                urefs_size,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_call_versioned_contract",
                        |mut caller: Caller<Runtime<R>>,
                         contract_package_hash_ptr,
                         contract_package_hash_size,
                         contract_version_ptr,
                         contract_package_size,
                         entry_point_name_ptr,
                         entry_point_name_size,
                         args_ptr,
                         args_size,
                         result_size_ptr| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();

                            runtime.charge_host_function_call(
                                &host_function_costs.call_versioned_contract,
                                [
                                    contract_package_hash_ptr,
                                    contract_package_hash_size,
                                    contract_version_ptr,
                                    contract_package_size,
                                    entry_point_name_ptr,
                                    entry_point_name_size,
                                    args_ptr,
                                    args_size,
                                    result_size_ptr,
                                ],
                            )?;

                            // TODO: Move

                            let contract_package_hash: ContractPackageHash = {
                                let contract_package_hash_bytes = function_context
                                    .memory_read(
                                        contract_package_hash_ptr,
                                        contract_package_hash_size as usize,
                                    )
                                    .map_err(|e| execution::Error::Interpreter(e.to_string()))?;
                                bytesrepr::deserialize(contract_package_hash_bytes)
                                    .map_err(execution::Error::BytesRepr)?
                            };
                            let contract_version: Option<ContractVersion> = {
                                let contract_version_bytes = function_context
                                    .memory_read(
                                        contract_version_ptr,
                                        contract_package_size as usize,
                                    )
                                    .map_err(|e| execution::Error::Interpreter(e.to_string()))?;
                                bytesrepr::deserialize(contract_version_bytes)
                                    .map_err(execution::Error::BytesRepr)?
                            };
                            let entry_point_name: String = {
                                let entry_point_name_bytes = function_context
                                    .memory_read(
                                        entry_point_name_ptr,
                                        entry_point_name_size as usize,
                                    )
                                    .map_err(|e| execution::Error::Interpreter(e.to_string()))?;
                                bytesrepr::deserialize(entry_point_name_bytes)
                                    .map_err(execution::Error::BytesRepr)?
                            };
                            let args_bytes: Vec<u8> = function_context
                                .memory_read(args_ptr, args_size as usize)
                                .map_err(|e| execution::Error::Interpreter(e.to_string()))?;

                            let ret = runtime.call_versioned_contract_host_buffer(
                                function_context,
                                contract_package_hash,
                                contract_version,
                                entry_point_name,
                                args_bytes,
                                result_size_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_add_contract_version",
                        |mut caller: Caller<Runtime<R>>,
                         contract_package_hash_ptr,
                         contract_package_hash_size,
                         version_ptr,
                         entry_points_ptr,
                         entry_points_size,
                         named_keys_ptr,
                         named_keys_size,
                         output_ptr,
                         output_size,
                         bytes_written_ptr| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();

                            let ret = runtime.casper_add_contract_version(
                                function_context,
                                contract_package_hash_ptr,
                                contract_package_hash_size,
                                version_ptr,
                                entry_points_ptr,
                                entry_points_size,
                                named_keys_ptr,
                                named_keys_size,
                                output_ptr,
                                output_size,
                                bytes_written_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_call_contract",
                        |mut caller: Caller<Runtime<R>>,
                         contract_hash_ptr,
                         contract_hash_size,
                         entry_point_name_ptr,
                         entry_point_name_size,
                         args_ptr,
                         args_size,
                         result_size_ptr| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();

                            let ret = runtime.casper_call_contract(
                                function_context,
                                contract_hash_ptr,
                                contract_hash_size,
                                entry_point_name_ptr,
                                entry_point_name_size,
                                args_ptr,
                                args_size,
                                result_size_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_load_call_stack",
                        |mut caller: Caller<Runtime<R>>, call_stack_len_ptr, result_size_ptr| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            runtime.charge_host_function_call(
                                &host_function_costs::HostFunction::fixed(10_000),
                                [call_stack_len_ptr, result_size_ptr],
                            )?;
                            let ret = runtime.load_call_stack(
                                function_context,
                                call_stack_len_ptr,
                                result_size_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_new_dictionary",
                        |mut caller: Caller<Runtime<R>>, output_size_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            runtime.charge_host_function_call(
                                &host_function_costs::DEFAULT_HOST_FUNCTION_NEW_DICTIONARY,
                                [output_size_ptr],
                            )?;
                            let ret = runtime.new_dictionary(function_context, output_size_ptr)?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_dictionary_get",
                        |mut caller: Caller<Runtime<R>>,
                         uref_ptr: u32,
                         uref_size: u32,
                         key_bytes_ptr: u32,
                         key_bytes_size: u32,
                         output_size_ptr: u32| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();
                            runtime.charge_host_function_call(
                                &host_function_costs.dictionary_get,
                                [key_bytes_ptr, key_bytes_size, output_size_ptr],
                            )?;
                            let ret = runtime.dictionary_get(
                                function_context,
                                uref_ptr,
                                uref_size,
                                key_bytes_ptr,
                                key_bytes_size,
                                output_size_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_dictionary_put",
                        |mut caller: Caller<Runtime<R>>,
                         uref_ptr,
                         uref_size,
                         key_bytes_ptr,
                         key_bytes_size,
                         value_ptr,
                         value_ptr_size| {
                            let (function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let ret = runtime.dictionary_put(
                                function_context,
                                uref_ptr,
                                uref_size,
                                key_bytes_ptr,
                                key_bytes_size,
                                value_ptr,
                                value_ptr_size,
                            )?;
                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();
                            runtime.charge_host_function_call(
                                &host_function_costs.dictionary_put,
                                [key_bytes_ptr, key_bytes_size, value_ptr, value_ptr_size],
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_blake2b",
                        |mut caller: Caller<Runtime<R>>,
                         in_ptr: u32,
                         in_size: u32,
                         out_ptr: u32,
                         out_size: u32| {
                            let (mut function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);
                            let host_function_costs =
                                runtime.config().wasm_config().take_host_function_costs();
                            runtime.charge_host_function_call(
                                &host_function_costs.blake2b,
                                [in_ptr, in_size, out_ptr, out_size],
                            )?;
                            let digest = account::blake2b(
                                function_context
                                    .memory_read(in_ptr, in_size as usize)
                                    .map_err(|e| execution::Error::Interpreter(e.into()))?,
                            );
                            if digest.len() != out_size as usize {
                                let err_value =
                                    u32::from(api_error::ApiError::BufferTooSmall) as i32;
                                return Ok(err_value);
                            }
                            function_context
                                .memory_write(out_ptr, &digest)
                                .map_err(|e| execution::Error::Interpreter(e.into()))?;
                            Ok(0_i32)
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_load_authorization_keys",
                        |mut caller: Caller<Runtime<R>>, len_ptr: u32, result_size_ptr: u32| {
                            let (mut function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let ret = runtime.casper_load_authorization_keys(
                                function_context,
                                len_ptr,
                                result_size_ptr,
                            )?;
                            Ok(api_error::i32_from(ret))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_disable_contract_version",
                        |mut caller: Caller<Runtime<R>>,
                         package_key_ptr: u32,
                         package_key_size: u32,
                         contract_hash_ptr: u32,
                         contract_hash_size: u32| {
                            let (mut function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let result = runtime.casper_disable_contract_version(
                                function_context,
                                package_key_ptr,
                                package_key_size,
                                contract_hash_ptr,
                                contract_hash_size,
                            )?;

                            Ok(api_error::i32_from(result))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_dictionary_read",
                        |mut caller: Caller<Runtime<R>>,
                         key_ptr: u32,
                         key_size: u32,
                         output_size_ptr: u32| {
                            let (mut function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let result = runtime.casper_dictionary_read(
                                function_context,
                                key_ptr,
                                key_size,
                                output_size_ptr,
                            )?;

                            Ok(api_error::i32_from(result))
                        },
                    )
                    .unwrap();

                linker
                    .func_wrap(
                        "env",
                        "casper_random_bytes",
                        |mut caller: Caller<Runtime<R>>, out_ptr: u32, out_size: u32| {
                            let (mut function_context, runtime) =
                                caller_adapter_and_runtime(&mut caller);

                            let result =
                                runtime.casper_random_bytes(function_context, out_ptr, out_size)?;

                            Ok(api_error::i32_from(result))
                        },
                    )
                    .unwrap();

                let start = Instant::now();

                let instance = linker
                    .instantiate(&mut store, &compiled_module)
                    .map_err(|error| RuntimeError::Other(error.to_string()))?;

                let exported_func = instance
                    .get_typed_func::<(), (), _>(&mut store, func_name)
                    .expect("should get typed func");

                let ret = exported_func
                    .call(&mut store, ())
                    .map_err(RuntimeError::from);

                let stop = start.elapsed();

                record_performance_log(
                    &original_bytes,
                    vec![
                        LogProperty::Size(original_bytes.len()),
                        LogProperty::EntryPoint(func_name.to_string()),
                        LogProperty::PrecompileTime(precompile_time.unwrap_or_default()),
                        LogProperty::Runtime(stop),
                    ],
                );

                // record_performance_log(&original_bytes, LogProperty::Runtime(stop));
                // record_performance_log(&original_bytes, LogProperty::Finish);

                ret?;

                Ok(Some(RuntimeValue::I64(0)))
            }
            Instance::Singlepass {
                original_bytes,
                module,
                wasmer_module,
                runtime,
            } => {
                // let import_object = imports! {};
                let mut import_object = wasmer::Imports::new();
                // import_object.define("env", "host_function", host_function);
                let memory_pages = wasm_engine.wasm_config().max_memory;
                let mut wasmer_store = wasm_engine.wasmer_store.write().unwrap();

                let import_type = wasmer_module
                    .imports()
                    .find(|import_type| {
                        (import_type.module(), import_type.name()) == ("env", "memory")
                    })
                    .expect("should be validated already");
                let memory_import = import_type.ty().memory().expect("should be memory");

                let memory = wasmer::Memory::new(wasmer_store.deref_mut(), memory_import.clone())
                    .expect("should create wasmer memory");

                import_object.define("env", "memory", memory.clone());

                let function_env = FunctionEnv::new(
                    wasmer_store.deref_mut(),
                    WasmerEnv {
                        runtime: Box::new(runtime.clone()),
                        // memory: memory.clone(),
                    },
                );

                let gas_func = wasmer::Function::new_typed_with_env(
                    wasmer_store.deref_mut(),
                    &function_env,
                    |mut env: FunctionEnvMut<WasmerEnv<R>>,
                     gas_arg: u32|
                     -> Result<(), execution::Error> {
                        env.data_mut().runtime.gas(Gas::new(gas_arg.into()))
                    },
                );

                let revert_func = wasmer::Function::new_typed_with_env(
                    wasmer_store.deref_mut(),
                    &function_env,
                    |mut env: FunctionEnvMut<WasmerEnv<R>>,
                     status: u32|
                     -> Result<(), execution::Error> {
                        let error = env.data_mut().runtime.casper_revert(status).unwrap_err();
                        Err(error)
                    },
                );

                let mem2 = memory.clone();
                let new_uref_func = wasmer::Function::new_typed_with_env(
                    wasmer_store.deref_mut(),
                    &function_env,
                    move |mut env: FunctionEnvMut<WasmerEnv<R>>,
                          uref_ptr: u32,
                          value_ptr: u32,
                          value_size: u32|
                          -> Result<(), execution::Error> {
                        // let cloned_mem = mem2;
                        let function_context = WasmerAdapter::new(mem2.view(&env.as_store_ref()));

                        env.data_mut().runtime.casper_new_uref(
                            function_context,
                            uref_ptr,
                            value_ptr,
                            value_size,
                        )?;
                        Ok(())
                    },
                );
                let mem2 = memory.clone();
                let get_main_purse_func = wasmer::Function::new_typed_with_env(
                    wasmer_store.deref_mut(),
                    &function_env,
                    move |mut env: FunctionEnvMut<WasmerEnv<R>>,
                          dest_ptr: u32|
                          -> Result<(), execution::Error> {
                        let function_context = WasmerAdapter::new(mem2.view(&env.as_store_ref()));
                        env.data_mut()
                            .runtime
                            .casper_get_main_purse(function_context, dest_ptr)?;
                        Ok(())
                    },
                );

                let memory = memory.clone();

                let write_func = wasmer::Function::new_typed_with_env(
                    wasmer_store.deref_mut(),
                    &function_env,
                    move |mut env: FunctionEnvMut<WasmerEnv<R>>,
                          key_ptr: u32,
                          key_size: u32,
                          value_ptr: u32,
                          value_size: u32|
                          -> Result<(), execution::Error> {
                        let function_context = WasmerAdapter::new(memory.view(&env.as_store_ref()));
                        env.data_mut().runtime.casper_write(
                            function_context,
                            key_ptr,
                            key_size,
                            value_ptr,
                            value_size,
                        )?;
                        Ok(())
                    },
                );

                import_object.define("env", "gas", wasmer::Extern::from(gas_func));
                import_object.define("env", "casper_revert", wasmer::Extern::from(revert_func));
                import_object.define(
                    "env",
                    "casper_new_uref",
                    wasmer::Extern::from(new_uref_func),
                );
                import_object.define(
                    "env",
                    "casper_get_main_purse",
                    wasmer::Extern::from(get_main_purse_func),
                );
                import_object.define("env", "casper_write", wasmer::Extern::from(write_func));

                let instance =
                    wasmer::Instance::new(wasmer_store.deref_mut(), &wasmer_module, &import_object)
                        .expect("should create new wasmer instance");

                let call_func: TypedFunction<(), ()> = instance
                    .exports
                    .get_typed_function(wasmer_store.deref_mut(), "call")
                    .expect("should get wasmer call func");

                call_func.call(wasmer_store.deref_mut())?;

                Ok(Some(RuntimeValue::I64(0)))
            }
        }
    }
}

/// An error emitted by the Wasm preprocessor.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum WasmValidationError {
    /// Initial table size outside allowed bounds.
    #[error("initial table size of {actual} exceeds allowed limit of {max}")]
    InitialTableSizeExceeded {
        /// Allowed maximum table size.
        max: u32,
        /// Actual initial table size specified in the Wasm.
        actual: u32,
    },
    /// Maximum table size outside allowed bounds.
    #[error("maximum table size of {actual} exceeds allowed limit of {max}")]
    MaxTableSizeExceeded {
        /// Allowed maximum table size.
        max: u32,
        /// Actual max table size specified in the Wasm.
        actual: u32,
    },
    /// Number of the tables in a Wasm must be at most one.
    #[error("the number of tables must be at most one")]
    MoreThanOneTable,
    /// Length of a br_table exceeded the maximum allowed size.
    #[error("maximum br_table size of {actual} exceeds allowed limit of {max}")]
    BrTableSizeExceeded {
        /// Maximum allowed br_table length.
        max: u32,
        /// Actual size of a br_table in the code.
        actual: usize,
    },
    /// Declared number of globals exceeds allowed limit.
    #[error("declared number of globals ({actual}) exceeds allowed limit of {max}")]
    TooManyGlobals {
        /// Maximum allowed globals.
        max: u32,
        /// Actual number of globals declared in the Wasm.
        actual: usize,
    },
    /// Module declares a function type with too many parameters.
    #[error("use of a function type with too many parameters (limit of {max} but function declares {actual})")]
    TooManyParameters {
        /// Maximum allowed parameters.
        max: u32,
        /// Actual number of parameters a function has in the Wasm.
        actual: usize,
    },
    /// Module tries to import a function that the host does not provide.
    #[error("module imports a non-existent function")]
    MissingHostFunction,
    /// Opcode for a global access refers to a non-existing global
    #[error("opcode for a global access refers to non-existing global index {index}")]
    IncorrectGlobalOperation {
        /// Provided index.
        index: u32,
    },
    /// Missing function index.
    #[error("missing function index {index}")]
    MissingFunctionIndex {
        /// Provided index.
        index: u32,
    },
    /// Missing function type.
    #[error("missing type index {index}")]
    MissingFunctionType {
        /// Provided index.
        index: u32,
    },
}

/// An error emitted by the Wasm preprocessor.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum PreprocessingError {
    /// Unable to deserialize Wasm bytes.
    #[error("Deserialization error: {0}")]
    Deserialize(String),
    /// Found opcodes forbidden by gas rules.
    #[error(
        "Encountered operation forbidden by gas rules. Consult instruction -> metering config map"
    )]
    OperationForbiddenByGasRules,
    /// Stack limiter was unable to instrument the binary.
    #[error("Stack limiter error")]
    StackLimiter,
    /// Wasm bytes is missing memory section.
    #[error("Memory section should exist")]
    MissingMemorySection,
    /// The module is missing.
    #[error("Missing module")]
    MissingModule,
    /// Unable to validate wasm bytes.
    #[error("Wasm validation error: {0}")]
    WasmValidation(#[from] WasmValidationError),
    #[error("Precompile error: {0}")]
    Precompile(RuntimeError),
}

impl From<elements::Error> for PreprocessingError {
    fn from(error: elements::Error) -> Self {
        PreprocessingError::Deserialize(error.to_string())
    }
}

/// Ensures that all the references to functions and global variables in the wasm bytecode are
/// properly declared.
///
/// This validates that:
///
/// - Start function points to a function declared in the Wasm bytecode
/// - All exported functions are pointing to functions declared in the Wasm bytecode
/// - `call` instructions reference a function declared in the Wasm bytecode.
/// - `global.set`, `global.get` instructions are referencing an existing global declared in the
///   Wasm bytecode.
/// - All members of the "elem" section point at functions declared in the Wasm bytecode.
fn ensure_valid_access(module: &WasmiModule) -> Result<(), WasmValidationError> {
    let function_types_count = module
        .type_section()
        .map(|ts| ts.types().len())
        .unwrap_or_default();

    let mut function_count = 0_u32;
    if let Some(import_section) = module.import_section() {
        for import_entry in import_section.entries() {
            if let External::Function(function_type_index) = import_entry.external() {
                if (*function_type_index as usize) < function_types_count {
                    function_count = function_count.saturating_add(1);
                } else {
                    return Err(WasmValidationError::MissingFunctionType {
                        index: *function_type_index,
                    });
                }
            }
        }
    }
    if let Some(function_section) = module.function_section() {
        for function_entry in function_section.entries() {
            let function_type_index = function_entry.type_ref();
            if (function_type_index as usize) < function_types_count {
                function_count = function_count.saturating_add(1);
            } else {
                return Err(WasmValidationError::MissingFunctionType {
                    index: function_type_index,
                });
            }
        }
    }

    if let Some(function_index) = module.start_section() {
        ensure_valid_function_index(function_index, function_count)?;
    }
    if let Some(export_section) = module.export_section() {
        for export_entry in export_section.entries() {
            if let Internal::Function(function_index) = export_entry.internal() {
                ensure_valid_function_index(*function_index, function_count)?;
            }
        }
    }

    if let Some(code_section) = module.code_section() {
        let global_len = module
            .global_section()
            .map(|global_section| global_section.entries().len())
            .unwrap_or(0);

        for instr in code_section
            .bodies()
            .iter()
            .flat_map(|body| body.code().elements())
        {
            match instr {
                Instruction::Call(idx) => {
                    ensure_valid_function_index(*idx, function_count)?;
                }
                Instruction::GetGlobal(idx) | Instruction::SetGlobal(idx)
                    if *idx as usize >= global_len =>
                {
                    return Err(WasmValidationError::IncorrectGlobalOperation { index: *idx });
                }
                _ => {}
            }
        }
    }

    if let Some(element_section) = module.elements_section() {
        for element_segment in element_section.entries() {
            for idx in element_segment.members() {
                ensure_valid_function_index(*idx, function_count)?;
            }
        }
    }

    Ok(())
}

fn ensure_valid_function_index(index: u32, function_count: u32) -> Result<(), WasmValidationError> {
    if index >= function_count {
        return Err(WasmValidationError::MissingFunctionIndex { index });
    }
    Ok(())
}

/// Checks if given wasm module contains a memory section.
fn memory_section(module: &WasmiModule) -> Option<&MemorySection> {
    for section in module.sections() {
        if let Section::Memory(section) = section {
            return Some(section);
        }
    }
    None
}

/// Ensures (table) section has at most one table entry, and initial, and maximum values are
/// normalized.
///
/// If a maximum value is not specified it will be defaulted to 4k to prevent OOM.
fn ensure_table_size_limit(
    mut module: WasmiModule,
    limit: u32,
) -> Result<WasmiModule, WasmValidationError> {
    if let Some(sect) = module.table_section_mut() {
        // Table section is optional and there can be at most one.
        if sect.entries().len() > 1 {
            return Err(WasmValidationError::MoreThanOneTable);
        }

        if let Some(table_entry) = sect.entries_mut().first_mut() {
            let initial = table_entry.limits().initial();
            if initial > limit {
                return Err(WasmValidationError::InitialTableSizeExceeded {
                    max: limit,
                    actual: initial,
                });
            }

            match table_entry.limits().maximum() {
                Some(max) => {
                    if max > limit {
                        return Err(WasmValidationError::MaxTableSizeExceeded {
                            max: limit,
                            actual: max,
                        });
                    }
                }
                None => {
                    // rewrite wasm and provide a maximum limit for a table section
                    *table_entry = TableType::new(initial, Some(limit))
                }
            }
        }
    }

    Ok(module)
}

/// Ensure that any `br_table` instruction adheres to its immediate value limit.
fn ensure_br_table_size_limit(module: &WasmiModule, limit: u32) -> Result<(), WasmValidationError> {
    let code_section = if let Some(type_section) = module.code_section() {
        type_section
    } else {
        return Ok(());
    };
    for instr in code_section
        .bodies()
        .iter()
        .flat_map(|body| body.code().elements())
    {
        if let Instruction::BrTable(br_table_data) = instr {
            if br_table_data.table.len() > limit as usize {
                return Err(WasmValidationError::BrTableSizeExceeded {
                    max: limit,
                    actual: br_table_data.table.len(),
                });
            }
        }
    }
    Ok(())
}

/// Ensures that module doesn't declare too many globals.
///
/// Globals are not limited through the `stack_height` as locals are. Neither does
/// the linear memory limit `memory_pages` applies to them.
fn ensure_global_variable_limit(
    module: &WasmiModule,
    limit: u32,
) -> Result<(), WasmValidationError> {
    if let Some(global_section) = module.global_section() {
        let actual = global_section.entries().len();
        if actual > limit as usize {
            return Err(WasmValidationError::TooManyGlobals { max: limit, actual });
        }
    }
    Ok(())
}

/// Ensure maximum numbers of parameters a function can have.
///
/// Those need to be limited to prevent a potentially exploitable interaction with
/// the stack height instrumentation: The costs of executing the stack height
/// instrumentation for an indirectly called function scales linearly with the amount
/// of parameters of this function. Because the stack height instrumentation itself is
/// is not weight metered its costs must be static (via this limit) and included in
/// the costs of the instructions that cause them (call, call_indirect).
fn ensure_parameter_limit(module: &WasmiModule, limit: u32) -> Result<(), WasmValidationError> {
    let type_section = if let Some(type_section) = module.type_section() {
        type_section
    } else {
        return Ok(());
    };

    for Type::Function(func) in type_section.types() {
        let actual = func.params().len();
        if actual > limit as usize {
            return Err(WasmValidationError::TooManyParameters { max: limit, actual });
        }
    }

    Ok(())
}

/// Ensures that Wasm module has valid imports.
fn ensure_valid_imports(module: &WasmiModule) -> Result<(), WasmValidationError> {
    let import_entries = module
        .import_section()
        .map(|is| is.entries())
        .unwrap_or(&[]);

    // Gas counter is currently considered an implementation detail.
    //
    // If a wasm module tries to import it will be rejected.

    for import in import_entries {
        if import.module() == DEFAULT_GAS_MODULE_NAME
            && import.field() == INTERNAL_GAS_FUNCTION_NAME
        {
            return Err(WasmValidationError::MissingHostFunction);
        }
    }

    Ok(())
}

#[derive(Clone)]
struct WasmtimeEngine(wasmtime::Engine);

impl fmt::Debug for WasmtimeEngine {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("WasmtimeEngine").finish()
    }
}

impl std::ops::Deref for WasmtimeEngine {
    type Target = wasmtime::Engine;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// NOTE: Imitates persistent on disk cache for precompiled artifacts
static GLOBAL_CACHE: Lazy<Mutex<HashMap<Vec<u8>, Vec<u8>>>> = Lazy::new(Default::default);

/// Wasm preprocessor.
#[derive(Debug, Clone)]
pub struct WasmEngine {
    wasm_config: WasmConfig,
    execution_mode: ExecutionMode,
    compiled_engine: Arc<WasmtimeEngine>,
    wasmer_store: Arc<RwLock<wasmer::Store>>,
}

fn setup_wasmtime_caching(cache_path: &Path, config: &mut wasmtime::Config) -> Result<(), String> {
    let wasmtime_cache_root = cache_path.join("wasmtime");
    fs::create_dir_all(&wasmtime_cache_root)
        .map_err(|err| format!("cannot create the dirs to cache: {:?}", err))?;

    // Canonicalize the path after creating the directories.
    let wasmtime_cache_root = wasmtime_cache_root
        .canonicalize()
        .map_err(|err| format!("failed to canonicalize the path: {:?}", err))?;

    // Write the cache config file
    let cache_config_path = wasmtime_cache_root.join("cache-config.toml");

    let config_content = format!(
        "\
[cache]
enabled = true
directory = \"{cache_dir}\"
",
        cache_dir = wasmtime_cache_root.display()
    );
    fs::write(&cache_config_path, config_content)
        .map_err(|err| format!("cannot write the cache config: {:?}", err))?;

    config
        .cache_config_load(cache_config_path)
        .map_err(|err| format!("failed to parse the config: {:?}", err))?;

    Ok(())
}

// TODO: There are some issues with multithreaded test runner and how we set up the cache. We should
// figure out better way to deal with this initialization.
static GLOBAL_CACHE_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

fn new_compiled_engine(wasm_config: &WasmConfig) -> WasmtimeEngine {
    let mut config = wasmtime::Config::new();

    // This should solve nondeterministic issue with access to the directory when running EE in
    // non-compiled mode
    {
        let _global_cache_guard = GLOBAL_CACHE_MUTEX.lock().unwrap();
        setup_wasmtime_caching(&Path::new("/tmp/wasmtime_test"), &mut config)
            .expect("should setup wasmtime cache path");
    }

    config.cranelift_opt_level(wasmtime::OptLevel::SpeedAndSize);
    // config.async_support(false);
    config.wasm_reference_types(false);
    config.wasm_simd(false);
    config.wasm_bulk_memory(false);
    config.wasm_multi_value(false);
    config.wasm_multi_memory(false);
    config.wasm_module_linking(false);
    config.wasm_threads(false);

    // TODO: Tweak more
    let wasmtime_engine = wasmtime::Engine::new(&config).expect("should create new engine");
    WasmtimeEngine(wasmtime_engine)
}

impl WasmEngine {
    /// Creates a new instance of the preprocessor.
    pub fn new(wasm_config: WasmConfig) -> Self {
        let singlepass = Singlepass::new();
        Self {
            wasm_config,
            execution_mode: wasm_config.execution_mode,
            compiled_engine: Arc::new(new_compiled_engine(&wasm_config)),
            wasmer_store: Arc::new(RwLock::new(wasmer::Store::new(singlepass))),
        }
    }

    pub fn execution_mode(&self) -> &ExecutionMode {
        &self.execution_mode
    }

    pub fn execution_mode_mut(&mut self) -> &mut ExecutionMode {
        &mut self.execution_mode
    }

    /// Preprocesses Wasm bytes and returns a module.
    ///
    /// This process consists of a few steps:
    /// - Validate that the given bytes contain a memory section, and check the memory page limit.
    /// - Inject gas counters into the code, which makes it possible for the executed Wasm to be
    ///   charged for opcodes; this also validates opcodes and ensures that there are no forbidden
    ///   opcodes in use, such as floating point opcodes.
    /// - Ensure that the code has a maximum stack height.
    ///
    /// In case the preprocessing rules can't be applied, an error is returned.
    /// Otherwise, this method returns a valid module ready to be executed safely on the host.
    pub fn preprocess(
        &self,
        wasm_config: WasmConfig,
        module_bytes: &[u8],
    ) -> Result<Module, PreprocessingError> {
        let module = deserialize_interpreted(module_bytes)?;

        ensure_valid_access(&module)?;

        if memory_section(&module).is_none() {
            // `pwasm_utils::externalize_mem` expects a non-empty memory section to exist in the
            // module, and panics otherwise.
            return Err(PreprocessingError::MissingMemorySection);
        }

        let module = ensure_table_size_limit(module, DEFAULT_MAX_TABLE_SIZE)?;
        ensure_br_table_size_limit(&module, DEFAULT_BR_TABLE_MAX_SIZE)?;
        ensure_global_variable_limit(&module, DEFAULT_MAX_GLOBALS)?;
        ensure_parameter_limit(&module, DEFAULT_MAX_PARAMETER_COUNT)?;
        ensure_valid_imports(&module)?;

        let module = pwasm_utils::externalize_mem(module, None, wasm_config.max_memory);
        let module = pwasm_utils::inject_gas_counter(
            module,
            &wasm_config.opcode_costs().to_set(),
            DEFAULT_GAS_MODULE_NAME,
        )
        .map_err(|_| PreprocessingError::OperationForbiddenByGasRules)?;
        let module = stack_height::inject_limiter(module, wasm_config.max_stack_height)
            .map_err(|_| PreprocessingError::StackLimiter)?;

        match self.execution_mode {
            ExecutionMode::Interpreted => Ok(Module::Interpreted {
                original_bytes: module_bytes.to_vec(),
                wasmi_module: module,
            }),
            ExecutionMode::Compiled => {
                // TODO: Gas injected module is used here but we might want to use `module` instead
                // with other preprocessing done.
                let preprocessed_wasm_bytes =
                    parity_wasm::serialize(module.clone()).expect("preprocessed wasm to bytes");

                // aot compile
                let (precompiled_bytes, duration) = self
                    .precompile(module_bytes, &preprocessed_wasm_bytes)
                    .map_err(PreprocessingError::Precompile)?;

                // let wasmtime_module =
                //     wasmtime::Module::new(&self.compiled_engine, &preprocessed_wasm_bytes)
                //         .expect("should process");
                Ok(Module::Compiled {
                    original_bytes: module_bytes.to_vec(),
                    wasmi_module: module,
                    precompile_time: duration,
                    compiled_artifact: precompiled_bytes.to_owned(),
                })

                // let compiled_module =
                //             wasmtime::Module::new(&self.compiled_engine,
                // &preprocessed_wasm_bytes)                 .map_err(|e|
                // PreprocessingError::Deserialize(e.to_string()))?;
                //     Ok(compiled_module.into())
            }
            ExecutionMode::JustInTime => {
                let start = Instant::now();

                let preprocessed_wasm_bytes =
                    parity_wasm::serialize(module.clone()).expect("preprocessed wasm to bytes");
                let compiled_module =
                    wasmtime::Module::new(&self.compiled_engine, preprocessed_wasm_bytes)
                        .map_err(|e| PreprocessingError::Deserialize(e.to_string()))?;

                let stop = start.elapsed();

                Ok(Module::Jitted {
                    original_bytes: module_bytes.to_vec(),
                    wasmi_module: module,
                    precompile_time: Some(stop),
                    module: compiled_module,
                })
            }
            ExecutionMode::Singlepass => {
                let preprocessed_wasm_bytes =
                    parity_wasm::serialize(module.clone()).expect("preprocessed wasm to bytes");

                // let singlepass = Singlepass::new();
                // let store = wasmer::Store::new(singlepass);
                // wasmer::Module::new

                let start = Instant::now();

                let wasmer_module = wasmer::Module::from_binary(
                    self.wasmer_store.write().unwrap().deref_mut(),
                    &preprocessed_wasm_bytes,
                )
                .expect("should create wasmer module");

                let stop = start.elapsed();

                Ok(Module::Singlepass {
                    original_bytes: module_bytes.to_vec(),
                    wasmi_module: module,
                    wasmer_module: wasmer_module,
                })
            }
        }
        // let module = deserialize_interpreted(module_bytes)?;
    }

    /// Get a reference to the wasm engine's wasm config.
    pub fn wasm_config(&self) -> &WasmConfig {
        &self.wasm_config
    }

    /// Creates module specific for execution mode.
    pub fn module_from_bytes(&self, wasm_bytes: &[u8]) -> Result<Module, PreprocessingError> {
        let parity_module = deserialize_interpreted(wasm_bytes)?;

        let module = match self.execution_mode {
            ExecutionMode::Interpreted => Module::Interpreted {
                wasmi_module: parity_module,
                original_bytes: wasm_bytes.to_vec(),
            },
            ExecutionMode::Compiled => {
                // aot compile
                let (precompiled_bytes, duration) =
                    self.precompile(wasm_bytes, wasm_bytes).unwrap();
                // self.compiled_engine.precompile_module(&wasm_bytes).expect("should preprocess");
                // let module = wasmtime::Module::new(&self.compiled_engine, wasm_bytes).unwrap();
                Module::Compiled {
                    original_bytes: wasm_bytes.to_vec(),
                    wasmi_module: parity_module,
                    precompile_time: duration,
                    compiled_artifact: precompiled_bytes.to_owned(),
                }
                // let compiled_module =
                //             wasmtime::Module::new(&self.compiled_engine,
                // &preprocessed_wasm_bytes)                 .map_err(|e|
                // PreprocessingError::Deserialize(e.to_string()))?;
                //     Ok(compiled_module.into())
                // let compiled_module = wasmtime::Module::new(&self.compiled_engine, wasm_bytes)
                //     .map_err(|e| PreprocessingError::Deserialize(e.to_string()))?;
                // Module::Compiled(compiled_module)
            }
            ExecutionMode::JustInTime => {
                let start = Instant::now();
                let module = wasmtime::Module::new(&self.compiled_engine, wasm_bytes).unwrap();
                let stop = start.elapsed();
                Module::Jitted {
                    original_bytes: wasm_bytes.to_vec(),
                    wasmi_module: parity_module,
                    precompile_time: Some(stop),
                    module,
                }
            }
            ExecutionMode::Singlepass => todo!(),
        };
        Ok(module)
    }

    /// Get a reference to the wasm engine's compiled engine.
    pub fn compiled_engine(&self) -> &wasmtime::Engine {
        &self.compiled_engine
    }
    /// Creates an WASM module instance and a memory instance.
    ///
    /// This ensures that a memory instance is properly resolved into a pre-allocated memory area,
    /// and a host function resolver is attached to the module.
    ///
    /// The WASM module is also validated to not have a "start" section as we currently don't
    /// support running it.
    ///
    /// Both [`ModuleRef`] and a [`MemoryRef`] are ready to be executed.
    pub fn instance_and_memory<R>(
        &self,
        wasm_module: Module,
        protocol_version: ProtocolVersion,
        runtime: Runtime<R>,
    ) -> Result<Instance<R>, execution::Error>
    where
        R: Send + Sync + 'static + Clone + StateReader<Key, StoredValue>,
        R::Error: Into<execution::Error>,
    {
        // match wasm_engine.execution_mode() {
        match wasm_module {
            Module::Noop => Ok(Instance::Noop),
            Module::Interpreted {
                original_bytes,
                wasmi_module,
            } => {
                let module = wasmi::Module::from_parity_wasm_module(wasmi_module)?;
                let resolver = create_module_resolver(protocol_version, self.wasm_config())?;
                let mut imports = ImportsBuilder::new();
                imports.push_resolver("env", &resolver);
                let not_started_module = ModuleInstance::new(&module, &imports)?;
                if not_started_module.has_start() {
                    return Err(execution::Error::UnsupportedWasmStart);
                }
                let instance = not_started_module.not_started_instance().clone();
                let memory = resolver.memory_ref()?;
                Ok(Instance::Interpreted {
                    original_bytes,
                    module: instance,
                    memory,
                    runtime,
                })
            }
            Module::Compiled {
                original_bytes,
                wasmi_module,
                compiled_artifact,
                precompile_time,
            } => {
                if wasmi_module.start_section().is_some() {
                    return Err(execution::Error::UnsupportedWasmStart);
                }
                let compiled_module = self.deserialize_compiled(&compiled_artifact)?;

                Ok(Instance::Compiled {
                    original_bytes,
                    module: wasmi_module,
                    compiled_module,
                    precompile_time,
                    runtime,
                })
            }
            Module::Jitted {
                original_bytes,
                wasmi_module,
                // compiled_artifact,
                precompile_time,
                module: compiled_module,
                // runtime,
            } => {
                if wasmi_module.start_section().is_some() {
                    return Err(execution::Error::UnsupportedWasmStart);
                }
                // aot compile
                // let precompiled_bytes =
                // self.compiled_engine.precompile_module(&preprocessed_wasm_bytes).expect("should
                // preprocess"); Ok(Module::Compiled(precompiled_bytes))

                // todo!("compiled mode")
                // let mut store = wasmtime::Store::new(&wasm_engine.compiled_engine(), ());
                // let instance = wasmtime::Instance::new(&mut store, &compiled_module,
                // &[]).expect("should create compiled module");

                // let compiled_module = self.deserialize_compiled(&compiled_artifact)?;
                Ok(Instance::Compiled {
                    original_bytes,
                    module: wasmi_module,
                    compiled_module,
                    precompile_time,
                    runtime,
                })
            }
            Module::Singlepass {
                original_bytes,
                wasmi_module,
                wasmer_module,
            } => {
                if wasmi_module.start_section().is_some() {
                    return Err(execution::Error::UnsupportedWasmStart);
                }
                Ok(Instance::Singlepass {
                    original_bytes,
                    module: wasmi_module,
                    wasmer_module,
                    runtime,
                })
            }
        }
    }

    fn deserialize_compiled(&self, bytes: &[u8]) -> Result<wasmtime::Module, execution::Error> {
        let compiled_module =
            unsafe { wasmtime::Module::deserialize(&self.compiled_engine(), bytes) }.unwrap();
        Ok(compiled_module)
    }

    fn precompile(
        &self,
        original_bytes: &[u8],
        bytes: &[u8],
    ) -> Result<(Vec<u8>, Option<Duration>), RuntimeError> {
        // todo!("precompile; disabled because of trait issues colliding with wasmer");
        let mut cache = GLOBAL_CACHE.lock().unwrap();
        // let mut cache = cache.borrow_mut();
        // let mut cache = GLOBAL_CACHE.lborrow_mut();
        let (bytes, maybe_duration) = match cache.entry(bytes.to_vec()) {
            Entry::Occupied(o) => (o.get().clone(), None),
            Entry::Vacant(v) => {
                let start = Instant::now();
                let precompiled_bytes = self
                    .compiled_engine
                    .precompile_module(bytes)
                    .map_err(|e| RuntimeError::Other(e.to_string()))?;
                let stop = start.elapsed();
                // record_performance_log(original_bytes, LogProperty::PrecompileTime(stop));
                let artifact = v.insert(precompiled_bytes).clone();
                (artifact, Some(stop))
            }
        };
        Ok((bytes, maybe_duration))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasmtime_trap_recover() {
        let error = execution::Error::Revert(ApiError::User(100));
        let trap: wasmtime::Trap = wasmtime::Trap::from(error);
        let runtime_error = RuntimeError::from(trap);
        let recovered = runtime_error
            .into_execution_error()
            .expect("should have error");
        assert!(matches!(
            recovered,
            execution::Error::Revert(ApiError::User(100))
        ));
    }
}
