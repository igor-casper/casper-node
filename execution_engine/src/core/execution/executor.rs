use std::{
    cell::RefCell,
    collections::BTreeSet,
    rc::Rc,
    sync::{Arc, RwLock},
};

use casper_types::{
    account::{Account, AccountHash},
    bytesrepr::FromBytes,
    contracts::{NamedKeys, DEFAULT_ENTRY_POINT_NAME},
    system::{auction, handle_payment, mint, AUCTION, HANDLE_PAYMENT, MINT},
    AccessRights, BlockTime, CLTyped, CLValue, ContextAccessRights, DeployHash, EntryPointType,
    Gas, Key, Phase, ProtocolVersion, RuntimeArgs, StoredValue, U512,
};

use crate::{
    core::{
        engine_state::{
            executable_deploy_item::ExecutionKind, execution_result::ExecutionResult, EngineConfig,
            ExecError,
        },
        execution::{address_generator::AddressGenerator, Error},
        runtime::{utils, Runtime, RuntimeStack},
        runtime_context::RuntimeContext,
        tracking_copy::{TrackingCopy, TrackingCopyExt},
    },
    shared::{
        newtypes::CorrelationId,
        wasm_engine::{FunctionContext, Instance, Module, NativeCode, WasmEngine},
    },
    storage::global_state::StateReader,
};

const ARG_AMOUNT: &str = "amount";

fn try_get_amount(runtime_args: &RuntimeArgs) -> Result<U512, ExecError> {
    runtime_args
        .try_get_number(ARG_AMOUNT)
        .map_err(ExecError::from)
}

/// Executor object deals with execution of WASM modules.
pub struct Executor {
    config: EngineConfig,
    wasm_engine: WasmEngine,
}

impl Executor {
    /// Creates new executor object.
    pub fn new(config: EngineConfig) -> Self {
        let wasm_engine = WasmEngine::new(*config.wasm_config());
        Executor {
            config,
            wasm_engine,
        }
    }

    /// Returns config.
    pub fn config(&self) -> EngineConfig {
        self.config
    }

    /// Wasm engine.
    pub fn wasm_engine(&self) -> &WasmEngine {
        &self.wasm_engine
    }

    /// Executes a WASM module.
    ///
    /// This method checks if a given contract hash is a system contract, and then short circuits to
    /// a specific native implementation of it. Otherwise, a supplied WASM module is executed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn exec<R>(
        &self,
        execution_kind: ExecutionKind,
        args: RuntimeArgs,
        account: Account,
        named_keys: NamedKeys,
        access_rights: ContextAccessRights,
        authorization_keys: BTreeSet<AccountHash>,
        blocktime: BlockTime,
        deploy_hash: DeployHash,
        gas_limit: Gas,
        protocol_version: ProtocolVersion,
        correlation_id: CorrelationId,
        tracking_copy: Arc<RwLock<TrackingCopy<R>>>,
        phase: Phase,
        stack: RuntimeStack,
    ) -> ExecutionResult
    where
        R: Send + Sync + 'static + StateReader<Key, StoredValue>,
        R::Error: Into<Error>,
    {
        let spending_limit: U512 = match try_get_amount(&args) {
            Ok(spending_limit) => spending_limit,
            Err(error) => {
                return ExecutionResult::precondition_failure(error.into());
            }
        };

        let address_generator = {
            let generator = AddressGenerator::new(deploy_hash.as_bytes(), phase);
            Arc::new(RwLock::new(generator))
        };

        let context = self.create_runtime_context(
            EntryPointType::Session,
            args.clone(),
            named_keys,
            access_rights,
            Key::from(account.account_hash()),
            account,
            authorization_keys,
            blocktime,
            deploy_hash,
            // gas_limit,
            address_generator,
            protocol_version,
            correlation_id.clone(),
            tracking_copy,
            phase,
            spending_limit,
        );

        let mut runtime = Runtime::new(self.config, context, self.wasm_engine.clone());

        let result = match execution_kind {
            ExecutionKind::Module(module_bytes) => {
                runtime.stack = Some(stack);

                match utils::attenuate_uref_in_args(
                    runtime.context.args().clone(),
                    runtime.context.account().main_purse().addr(),
                    AccessRights::WRITE,
                ) {
                    Ok(attenuated_args) => {
                        runtime.context.set_args(attenuated_args);
                    }
                    Err(error) => return runtime.into_failure(error.into()),
                };

                let module = match runtime.wasm_engine().preprocess(
                    Some(correlation_id.clone()),
                    &bytes::Bytes::from(module_bytes.into_inner()),
                ) {
                    Ok(module) => module,
                    Err(error) => return runtime.into_failure(error.into()),
                };

                // runtime.module = Some(module.get_wasmi_module());
                runtime.module = Some(module.get_original_bytes().clone());

                let mut instance = match self
                    .wasm_engine
                    .instance_and_memory(module, runtime.clone())
                {
                    Ok(instance) => instance,
                    Err(error) => return runtime.into_failure(error.into()),
                };

                let result = instance.invoke_export::<(), _>(
                    Some(correlation_id),
                    &self.wasm_engine,
                    DEFAULT_ENTRY_POINT_NAME,
                    (),
                );

                // instance.get_remaining_points(wasm_engine)
                dbg!(&instance.get_remaining_points());

                match result {
                    Ok(_) => {
                        return runtime.into_success();
                    }
                    Err(error) => match error.into_host_error() {
                        Ok(host_error) => return runtime.into_failure(host_error),
                        Err(error) => {
                            return runtime.into_failure(Error::Interpreter(error.to_string()))
                        }
                    },
                }
            }
            ExecutionKind::Contract {
                contract_hash,
                entry_point_name,
            } => {
                // These args are passed through here as they are required to construct the new
                // `Runtime` during the contract's execution (i.e. inside
                // `Runtime::execute_contract`).

                // In the new model where we hand out a generic context instance which could refer to NativeCode (for calling system contract) or coming from Wasm this construction is strange,
                // we should probably inline most of call_contract_with_stack to avoid creating this NativeCode instance.

                let mut native_code = NativeCode::new(gas_limit.value_u64());
                runtime.call_contract_with_stack(
                    &mut native_code,
                    contract_hash,
                    &entry_point_name,
                    args,
                    stack,
                )
            }
        };

        match result {
            Ok(_) => runtime.into_success(),
            Err(error) => runtime.into_failure(error),
        }
    }

    /// Executes standard payment code natively.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn exec_standard_payment<R>(
        &self,
        payment_args: RuntimeArgs,
        payment_base_key: Key,
        account: Account,
        payment_named_keys: NamedKeys,
        access_rights: ContextAccessRights,
        authorization_keys: BTreeSet<AccountHash>,
        blocktime: BlockTime,
        deploy_hash: DeployHash,
        payment_gas_limit: Gas,
        protocol_version: ProtocolVersion,
        correlation_id: CorrelationId,
        tracking_copy: Arc<RwLock<TrackingCopy<R>>>,
        phase: Phase,
        stack: RuntimeStack,
    ) -> ExecutionResult
    where
        R: Send + Sync + 'static + StateReader<Key, StoredValue>,
        R::Error: Into<Error>,
    {
        let spending_limit: U512 = match try_get_amount(&payment_args) {
            Ok(spending_limit) => spending_limit,
            Err(error) => {
                return ExecutionResult::precondition_failure(error.into());
            }
        };

        let address_generator = {
            let generator = AddressGenerator::new(deploy_hash.as_bytes(), phase);
            Arc::new(RwLock::new(generator))
        };

        let runtime_context = self.create_runtime_context(
            EntryPointType::Session,
            payment_args,
            payment_named_keys,
            access_rights,
            payment_base_key,
            account,
            authorization_keys,
            blocktime,
            deploy_hash,
            // payment_gas_limit,
            address_generator,
            protocol_version,
            correlation_id.clone(),
            Arc::clone(&tracking_copy),
            phase,
            spending_limit,
        );

        let execution_journal = tracking_copy.read().unwrap().execution_journal();

        // Standard payment is executed in the calling account's context; the stack already
        // captures that.
        let mut runtime = Runtime::new(self.config, runtime_context, self.wasm_engine.clone());

        let mut context = NativeCode::new(payment_gas_limit.value_u64());

        let mut result = runtime.call_host_standard_payment(&mut context, stack);

        let maybe_cost = context.get_remaining_points().into_remaining();

        let cost = match maybe_cost {
            Some(cost) => payment_gas_limit.value_u64() - cost,
            None => {
                result = Err(Error::GasLimit);
                payment_gas_limit.value_u64()
            }
        };

        match result {
            Ok(()) => {
                ExecutionResult::Success {
                    execution_journal: runtime.context().execution_journal(),
                    transfers: runtime.context().transfers().to_owned(),
                    // cost: runtime.context().gas_counter(),
                    cost: cost.into(),
                }
            }
            Err(error) => ExecutionResult::Failure {
                execution_journal,
                error: error.into(),
                transfers: runtime.context().transfers().to_owned(),
                cost: cost.into(),
            },
        }
    }

    /// Handles necessary address resolution and orchestration to securely call a system contract
    /// using the runtime.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn call_system_contract<R, T>(
        &self,
        direct_system_contract_call: DirectSystemContractCall,
        runtime_args: RuntimeArgs,
        account: Account,
        authorization_keys: BTreeSet<AccountHash>,
        blocktime: BlockTime,
        deploy_hash: DeployHash,
        gas_limit: Gas,
        protocol_version: ProtocolVersion,
        correlation_id: CorrelationId,
        tracking_copy: Arc<RwLock<TrackingCopy<R>>>,
        phase: Phase,
        stack: RuntimeStack,
        remaining_spending_limit: U512,
    ) -> (Option<T>, ExecutionResult)
    where
        R: Send + Sync + 'static + StateReader<Key, StoredValue>,
        R::Error: Into<Error>,
        T: FromBytes + CLTyped,
    {
        let address_generator = {
            let generator = AddressGenerator::new(deploy_hash.as_bytes(), phase);
            Arc::new(RwLock::new(generator))
        };

        // Today lack of existence of the system contract registry and lack of entry
        // for the minimum defined system contracts (mint, auction, handle_payment)
        // should cause the EE to panic. Do not remove the panics.
        let system_contract_registry = tracking_copy
            .write()
            .unwrap()
            .get_system_contracts(correlation_id.clone())
            .unwrap_or_else(|error| panic!("Could not retrieve system contracts: {:?}", error));

        // Snapshot of effects before execution, so in case of error only nonce update
        // can be returned.
        let execution_journal = tracking_copy.read().unwrap().execution_journal();

        let entry_point_name = direct_system_contract_call.entry_point_name();

        let contract_hash = match direct_system_contract_call {
            DirectSystemContractCall::Slash
            | DirectSystemContractCall::RunAuction
            | DirectSystemContractCall::DistributeRewards => {
                let auction_hash = system_contract_registry
                    .get(AUCTION)
                    .expect("should have auction hash");
                *auction_hash
            }
            DirectSystemContractCall::CreatePurse | DirectSystemContractCall::Transfer => {
                let mint_hash = system_contract_registry
                    .get(MINT)
                    .expect("should have mint hash");
                *mint_hash
            }
            DirectSystemContractCall::FinalizePayment
            | DirectSystemContractCall::GetPaymentPurse => {
                let handle_payment_hash = system_contract_registry
                    .get(HANDLE_PAYMENT)
                    .expect("should have handle payment");
                *handle_payment_hash
            }
        };

        let contract = match tracking_copy
            .write()
            .unwrap()
            .get_contract(correlation_id.clone(), contract_hash)
        {
            Ok(contract) => contract,
            Err(error) => return (None, ExecutionResult::precondition_failure(error.into())),
        };

        let mut named_keys = contract.named_keys().clone();
        let access_rights = contract.extract_access_rights(contract_hash);
        let base_key = Key::from(contract_hash);

        let runtime_context = self.create_runtime_context(
            EntryPointType::Contract,
            runtime_args.clone(),
            named_keys,
            access_rights,
            base_key,
            account,
            authorization_keys,
            blocktime,
            deploy_hash,
            // gas_limit,
            address_generator,
            protocol_version,
            correlation_id.clone(),
            tracking_copy,
            phase,
            remaining_spending_limit,
        );

        let mut runtime = Runtime::new(self.config, runtime_context, self.wasm_engine.clone());

        let mut native_code = NativeCode::new(gas_limit.value_u64());

        // DO NOT alter this logic to call a system contract directly (such as via mint_internal,
        // etc). Doing so would bypass necessary context based security checks in some use cases. It
        // is intentional to use the runtime machinery for this interaction with the system
        // contracts, to force all such security checks for usage via the executor into a single
        // execution path.
        let result = runtime.call_contract_with_stack(
            &mut native_code,
            contract_hash,
            entry_point_name,
            runtime_args,
            stack,
        );

        let maybe_cost = native_code.get_remaining_points().into_remaining();

        let cost = match maybe_cost {
            Some(cost) => gas_limit.value_u64() - cost,
            None => {
                result = Err(Error::GasLimit);
                gas_limit.value_u64()
            }
        };

        match result {
            Ok(value) => match value.into_t() {
                Ok(ret) => ExecutionResult::Success {
                    execution_journal: runtime.context().execution_journal(),
                    transfers: runtime.context().transfers().to_owned(),
                    cost: cost.into(),
                }
                .take_with_ret(ret),
                Err(error) => ExecutionResult::Failure {
                    execution_journal,
                    error: Error::CLValue(error).into(),
                    transfers: runtime.context().transfers().to_owned(),
                    cost: cost.into(),
                }
                .take_without_ret(),
            },
            Err(error) => ExecutionResult::Failure {
                execution_journal,
                error: error.into(),
                transfers: runtime.context().transfers().to_owned(),
                cost: cost.into(),
            }
            .take_without_ret(),
        }
    }

    /// Creates new runtime context.
    #[allow(clippy::too_many_arguments)]
    fn create_runtime_context<R>(
        &self,
        entry_point_type: EntryPointType,
        runtime_args: RuntimeArgs,
        mut named_keys: NamedKeys,
        access_rights: ContextAccessRights,
        base_key: Key,
        account: Account,
        authorization_keys: BTreeSet<AccountHash>,
        blocktime: BlockTime,
        deploy_hash: DeployHash,
        // gas_limit: Gas,
        address_generator: Arc<RwLock<AddressGenerator>>,
        protocol_version: ProtocolVersion,
        correlation_id: CorrelationId,
        tracking_copy: Arc<RwLock<TrackingCopy<R>>>,
        phase: Phase,
        remaining_spending_limit: U512,
    ) -> RuntimeContext<R>
    where
        R: Send + Sync + 'static + StateReader<Key, StoredValue>,
        R::Error: Into<Error>,
    {
        // let gas_counter = Gas::default();
        let transfers = Vec::default();

        RuntimeContext::new(
            tracking_copy,
            entry_point_type,
            named_keys,
            access_rights,
            runtime_args,
            authorization_keys,
            account,
            base_key,
            blocktime,
            deploy_hash,
            // gas_limit,
            // gas_counter,
            address_generator,
            protocol_version,
            correlation_id.clone(),
            phase,
            self.config,
            transfers,
            remaining_spending_limit,
        )
    }
}

/// Represents a variant of a system contract call.
pub(crate) enum DirectSystemContractCall {
    /// Calls auction's `slash` entry point.
    Slash,
    /// Calls auction's `run_auction` entry point.
    RunAuction,
    /// Calls auction's `distribute` entry point.
    DistributeRewards,
    /// Calls handle payment's `finalize` entry point.
    FinalizePayment,
    /// Calls mint's `create` entry point.
    CreatePurse,
    /// Calls mint's `transfer` entry point.
    Transfer,
    /// Calls handle payment's `
    GetPaymentPurse,
}

impl DirectSystemContractCall {
    fn entry_point_name(&self) -> &str {
        match self {
            DirectSystemContractCall::Slash => auction::METHOD_SLASH,
            DirectSystemContractCall::RunAuction => auction::METHOD_RUN_AUCTION,
            DirectSystemContractCall::DistributeRewards => auction::METHOD_DISTRIBUTE,
            DirectSystemContractCall::FinalizePayment => handle_payment::METHOD_FINALIZE_PAYMENT,
            DirectSystemContractCall::CreatePurse => mint::METHOD_CREATE,
            DirectSystemContractCall::Transfer => mint::METHOD_TRANSFER,
            DirectSystemContractCall::GetPaymentPurse => handle_payment::METHOD_GET_PAYMENT_PURSE,
        }
    }
}
