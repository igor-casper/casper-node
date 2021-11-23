//! This executable is designed to be used to profile a single execution of a simple contract which
//! transfers an amount between two accounts.
//!
//! In order to set up the required global state for the transfer, the `state-initializer` should
//! have been run first.
//!
//! By avoiding setting up global state as part of this executable, it will allow profiling to be
//! done only on meaningful code, rather than including test setup effort in the profile results.

use std::{convert::TryFrom, env, io, path::PathBuf, fs::{File, self}};

use clap::{crate_version, App, Arg};

use casper_engine_test_support::{DEFAULT_ACCOUNT_ADDR, internal::{
    DeployItemBuilder, ExecuteRequestBuilder, LmdbWasmTestBuilder, DEFAULT_PAYMENT,
}};
use casper_execution_engine::core::engine_state::EngineConfig;
use casper_hashing::Digest;
use casper_types::{runtime_args, RuntimeArgs, U512, DeployHash};

use casper_engine_tests::profiling;

const ABOUT: &str = "Executes a simple contract which transfers an amount between two accounts.  \
     Note that the 'state-initializer' executable should be run first to set up the required \
     global state.";

const ROOT_HASH_ARG_NAME: &str = "root-hash";
const ROOT_HASH_ARG_VALUE_NAME: &str = "HEX-ENCODED HASH";
const ROOT_HASH_ARG_HELP: &str =
    "Initial root hash; the output of running the 'state-initializer' executable";

const VERBOSE_ARG_NAME: &str = "verbose";
const VERBOSE_ARG_SHORT: &str = "v";
const VERBOSE_ARG_LONG: &str = "verbose";
const VERBOSE_ARG_HELP: &str = "Display the transforms resulting from the contract execution";

const TRANSFER_AMOUNT: u64 = 1;

fn root_hash_arg() -> Arg<'static, 'static> {
    Arg::with_name(ROOT_HASH_ARG_NAME)
        .value_name(ROOT_HASH_ARG_VALUE_NAME)
        .help(ROOT_HASH_ARG_HELP)
}

fn verbose_arg() -> Arg<'static, 'static> {
    Arg::with_name(VERBOSE_ARG_NAME)
        .short(VERBOSE_ARG_SHORT)
        .long(VERBOSE_ARG_LONG)
        .help(VERBOSE_ARG_HELP)
}

#[derive(Debug)]
struct Args {
    root_hash: Option<Vec<u8>>,
    data_dir: PathBuf,
    verbose: bool,
}

impl Args {
    fn new() -> Self {
        let exe_name = profiling::exe_name();
        let data_dir_arg = profiling::data_dir_arg();
        let arg_matches = App::new(&exe_name)
            .version(crate_version!())
            .about(ABOUT)
            .arg(root_hash_arg())
            .arg(data_dir_arg)
            .arg(verbose_arg())
            .get_matches();
        let root_hash = arg_matches
            .value_of(ROOT_HASH_ARG_NAME)
            .map(profiling::parse_hash);
        let data_dir = profiling::data_dir(&arg_matches);
        let verbose = arg_matches.is_present(VERBOSE_ARG_NAME);
        Args {
            root_hash,
            data_dir,
            verbose,
        }
    }
}

const STATE_HASH_FILE: &str = "state_hash.raw";

fn main() {
    let args = Args::new();

    // If the required initial root hash wasn't passed as a command line arg, expect to read it in
    // from stdin to allow for it to be piped from the output of 'state-initializer'.
    let state_root_hash = {
        let hash_bytes = match args.root_hash {
            Some(root_hash) => root_hash,
            None => fs::read(STATE_HASH_FILE).unwrap(),
        };

        Digest::try_from(hash_bytes.as_slice()).unwrap()
    };

    // let account_1_account_hash = profiling::account_1_account_hash();
    // let account_2_account_hash = profiling::account_2_account_hash();

    let exec_request = {
        let deploy = DeployItemBuilder::new()
            .with_address(*DEFAULT_ACCOUNT_ADDR)
            .with_deploy_hash([3; 32])
            .with_stored_session_named_key("contract_hash", "create_domains", runtime_args! {
                "number" => 50_000u64,
            })
            // .with_session_code(
            //     "simple_transfer.wasm",
            //     runtime_args! { "target" =>account_2_account_hash, "amount" => U512::from(TRANSFER_AMOUNT) },
            // )
            .with_empty_payment_bytes( runtime_args! { "amount" => (*DEFAULT_PAYMENT * 10)})
            .with_authorization_keys(&[*DEFAULT_ACCOUNT_ADDR])
            .build();

        ExecuteRequestBuilder::new().push_deploy(deploy).build()
    };

    let engine_config = EngineConfig::default();

    let mut test_builder =
        LmdbWasmTestBuilder::open(&args.data_dir, engine_config, state_root_hash);

    test_builder.exec(exec_request).expect_success().commit();

    if args.verbose {
        println!("{:#?}", test_builder.get_transforms());
    }

    let post_state_hash = test_builder.get_post_state_hash();
    fs::write(STATE_HASH_FILE, &post_state_hash).unwrap();
}
