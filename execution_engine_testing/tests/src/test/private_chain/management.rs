use std::collections::BTreeSet;

use casper_engine_test_support::{
    ExecuteRequestBuilder, InMemoryWasmTestBuilder, DEFAULT_ACCOUNT_ADDR, DEFAULT_AUCTION_DELAY,
    DEFAULT_CHAINSPEC_REGISTRY, DEFAULT_GENESIS_CONFIG_HASH, DEFAULT_GENESIS_TIMESTAMP_MILLIS,
    DEFAULT_LOCKED_FUNDS_PERIOD_MILLIS, DEFAULT_PROTOCOL_VERSION, DEFAULT_ROUND_SEIGNIORAGE_RATE,
    DEFAULT_RUN_GENESIS_REQUEST, DEFAULT_SYSTEM_CONFIG, DEFAULT_UNBONDING_DELAY,
    DEFAULT_VALIDATOR_SLOTS, DEFAULT_WASM_CONFIG,
};
use casper_execution_engine::{
    core::{
        engine_state::{
            engine_config::EngineConfigBuilder, Error, ExecConfig, GenesisAccount,
            RunGenesisRequest,
        },
        execution,
    },
    shared::chain_kind::ChainKind,
};
use casper_types::{
    account::{AccountHash, ActionThresholds, Weight},
    contracts::DEFAULT_ENTRY_POINT_NAME,
    runtime_args,
    system::mint::{self, ADMINISTRATIVE_ACCOUNTS_KEY},
    ApiError, CLValue, RuntimeArgs, StoredValue, U512,
};
use parity_wasm::{
    builder,
    elements::{Instruction, Instructions},
};

use crate::test::private_chain::ACCOUNT_2_ADDR;

use super::{
    ACCOUNT_1_ADDR, DEFAULT_ADMIN_ACCOUNT_ADDR, DEFAULT_PRIVATE_CHAIN_GENESIS,
    PRIVATE_CHAIN_DEFAULT_ACCOUNTS,
};

const ACCOUNT_MANAGEMENT_CONTRACT: &str = "account_management.wasm";
const ADD_ASSOCIATED_KEY_CONTRACT: &str = "add_associated_key.wasm";
const SET_ACTION_THRESHOLDS_CONTRACT: &str = "set_action_thresholds.wasm";
const UPDATE_ASSOCIATED_KEY_CONTRACT: &str = "update_associated_key.wasm";
const TRANSFER_TO_ACCOUNT_CONTRACT: &&str = &"transfer_to_account.wasm";
const CONTRACT_HASH_NAME: &str = "contract_hash";
const DISABLE_ACCOUNT_ENTRYPOINT: &str = "disable_account";
const ENABLE_ACCOUNT_ENTRYPOINT: &str = "enable_account";
const ARG_ACCOUNT_HASH: &str = "account_hash";

const ARG_ACCOUNT: &str = "account";
const ARG_WEIGHT: &str = "weight";

const ARG_KEY_MANAGEMENT_THRESHOLD: &str = "key_management_threshold";
const ARG_DEPLOY_THRESHOLD: &str = "deploy_threshold";

const EXPECTED_PRIVATE_CHAIN_THRESHOLDS: ActionThresholds = ActionThresholds {
    deployment: Weight::new(1),
    key_management: Weight::MAX,
};

/// Creates minimal session code that does only one "nop" opcode
pub fn do_minimum_bytes() -> Vec<u8> {
    let module = builder::module()
        .function()
        // A signature with 0 params and no return type
        .signature()
        .build()
        .body()
        .with_instructions(Instructions::new(vec![Instruction::Nop, Instruction::End]))
        .build()
        .build()
        // Export above function
        .export()
        .field(DEFAULT_ENTRY_POINT_NAME)
        .build()
        // Memory section is mandatory
        .memory()
        .build()
        .build();
    parity_wasm::serialize(module).expect("should serialize")
}

#[should_panic(expected = "UnsupportedAdministratorAccounts")]
#[ignore]
#[test]
fn should_not_run_genesis_with_administrator_accounts_on_public_chain() {
    let chain_kind = ChainKind::Public;

    let engine_config = EngineConfigBuilder::default()
        // This change below makes genesis config validation to fail as administrator accounts are
        // only valid for private chains.
        .with_chain_kind(chain_kind)
        .build();

    let mut builder = InMemoryWasmTestBuilder::new_with_config(engine_config);

    let genesis_config = ExecConfig::new(
        PRIVATE_CHAIN_DEFAULT_ACCOUNTS.clone(),
        *DEFAULT_WASM_CONFIG,
        *DEFAULT_SYSTEM_CONFIG,
        DEFAULT_VALIDATOR_SLOTS,
        DEFAULT_AUCTION_DELAY,
        DEFAULT_LOCKED_FUNDS_PERIOD_MILLIS,
        DEFAULT_ROUND_SEIGNIORAGE_RATE,
        DEFAULT_UNBONDING_DELAY,
        DEFAULT_GENESIS_TIMESTAMP_MILLIS,
        chain_kind,
    );

    let modified_genesis_request = RunGenesisRequest::new(
        *DEFAULT_GENESIS_CONFIG_HASH,
        *DEFAULT_PROTOCOL_VERSION,
        genesis_config,
        DEFAULT_CHAINSPEC_REGISTRY.clone(),
    );

    builder.run_genesis(&modified_genesis_request);
}

#[should_panic(expected = "DuplicatedAdministratorEntry")]
#[ignore]
#[test]
fn should_not_run_genesis_with_duplicated_administrator_accounts() {
    let chain_kind = ChainKind::Private;

    let engine_config = EngineConfigBuilder::default()
        // This change below makes genesis config validation to fail as administrator accounts are
        // only valid for private chains.
        .with_chain_kind(chain_kind)
        .build();

    let mut builder = InMemoryWasmTestBuilder::new_with_config(engine_config);

    let duplicated_administrator_accounts = {
        let mut accounts = PRIVATE_CHAIN_DEFAULT_ACCOUNTS.clone();
        accounts.extend(
            PRIVATE_CHAIN_DEFAULT_ACCOUNTS
                .iter()
                .filter_map(GenesisAccount::as_administrator_account)
                .cloned()
                .map(GenesisAccount::Administrator),
        );
        accounts
    };

    let genesis_config = ExecConfig::new(
        duplicated_administrator_accounts,
        *DEFAULT_WASM_CONFIG,
        *DEFAULT_SYSTEM_CONFIG,
        DEFAULT_VALIDATOR_SLOTS,
        DEFAULT_AUCTION_DELAY,
        DEFAULT_LOCKED_FUNDS_PERIOD_MILLIS,
        DEFAULT_ROUND_SEIGNIORAGE_RATE,
        DEFAULT_UNBONDING_DELAY,
        DEFAULT_GENESIS_TIMESTAMP_MILLIS,
        chain_kind,
    );

    let modified_genesis_request = RunGenesisRequest::new(
        *DEFAULT_GENESIS_CONFIG_HASH,
        *DEFAULT_PROTOCOL_VERSION,
        genesis_config,
        DEFAULT_CHAINSPEC_REGISTRY.clone(),
    );

    builder.run_genesis(&modified_genesis_request);
}

#[ignore]
#[test]
fn should_not_resolve_private_chain_host_functions_on_public_chain() {
    let mut builder = InMemoryWasmTestBuilder::default();
    builder.run_genesis(&*DEFAULT_RUN_GENESIS_REQUEST);

    let exec_request = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        ACCOUNT_MANAGEMENT_CONTRACT,
        RuntimeArgs::default(),
    )
    .build();

    builder.exec(exec_request).expect_failure();

    let error = builder.get_error().expect("should have error");

    assert!(matches!(
        error,
        Error::Exec(execution::Error::Interpreter(msg))
        if msg == "host module doesn't export function with name casper_control_management"
    ));
}

#[ignore]
#[test]
fn genesis_accounts_should_not_update_key_weight() {
    let mut builder = setup();

    let account_1 = builder
        .get_account(*ACCOUNT_1_ADDR)
        .expect("should have account 1");
    assert_eq!(
        account_1.action_thresholds(),
        &EXPECTED_PRIVATE_CHAIN_THRESHOLDS,
    );

    let exec_request_1 = {
        let session_args = runtime_args! {
            ARG_ACCOUNT => *ACCOUNT_1_ADDR,
            ARG_WEIGHT => Weight::MAX,
        };
        ExecuteRequestBuilder::standard(
            *ACCOUNT_1_ADDR,
            UPDATE_ASSOCIATED_KEY_CONTRACT,
            session_args,
        )
        .build()
    };

    builder.exec(exec_request_1).expect_failure().commit();

    let error = builder.get_error().expect("should have error");
    assert!(
        matches!(
            error,
            Error::Exec(execution::Error::Revert(ApiError::PermissionDenied))
        ),
        "{:?}",
        error
    );

    let exec_request_2 = {
        let session_args = runtime_args! {
            ARG_ACCOUNT => *DEFAULT_ADMIN_ACCOUNT_ADDR,
            ARG_WEIGHT => Weight::new(1),
        };
        ExecuteRequestBuilder::standard(
            *ACCOUNT_1_ADDR,
            UPDATE_ASSOCIATED_KEY_CONTRACT,
            session_args,
        )
        .build()
    };

    builder.exec(exec_request_2).expect_failure().commit();
}

#[ignore]
#[test]
fn genesis_accounts_should_not_modify_action_thresholds() {
    let mut builder = setup();

    let account_1 = builder
        .get_account(*ACCOUNT_1_ADDR)
        .expect("should have account 1");
    assert_eq!(
        account_1.action_thresholds(),
        &ActionThresholds {
            deployment: Weight::new(1),
            key_management: Weight::MAX,
        }
    );

    let exec_request = {
        let session_args = runtime_args! {
            ARG_DEPLOY_THRESHOLD => Weight::new(1),
            ARG_KEY_MANAGEMENT_THRESHOLD => Weight::new(1),
        };
        ExecuteRequestBuilder::standard(
            *ACCOUNT_1_ADDR,
            SET_ACTION_THRESHOLDS_CONTRACT,
            session_args,
        )
        .build()
    };

    builder.exec(exec_request).expect_failure().commit();
    let error = builder.get_error().expect("should have error");
    assert!(
        matches!(
            error,
            Error::Exec(execution::Error::Revert(ApiError::PermissionDenied))
        ),
        "{:?}",
        error
    );
}

#[ignore]
#[test]
fn genesis_accounts_should_not_manage_their_own_keys() {
    let secondary_account_hash = AccountHash::new([55; 32]);

    let mut builder = setup();

    let account_1 = builder
        .get_account(*ACCOUNT_1_ADDR)
        .expect("should have account 1");
    assert_eq!(
        account_1.action_thresholds(),
        &ActionThresholds {
            deployment: Weight::new(1),
            key_management: Weight::MAX,
        }
    );

    let exec_request = {
        let session_args = runtime_args! {
            ARG_ACCOUNT => secondary_account_hash,
            ARG_WEIGHT => Weight::MAX,
        };
        ExecuteRequestBuilder::standard(*ACCOUNT_1_ADDR, ADD_ASSOCIATED_KEY_CONTRACT, session_args)
            .build()
    };

    builder.exec(exec_request).expect_failure().commit();

    let error = builder.get_error().expect("should have error");
    assert!(
        matches!(
            error,
            Error::Exec(execution::Error::Revert(ApiError::PermissionDenied))
        ),
        "{:?}",
        error
    );
}

#[ignore]
#[test]
fn genesis_accounts_should_have_special_associated_key() {
    let builder = setup();

    let account_1 = builder
        .get_account(*ACCOUNT_1_ADDR)
        .expect("should create genesis account");

    let identity_weight = account_1
        .associated_keys()
        .get(&*ACCOUNT_1_ADDR)
        .expect("should have identity key");
    assert_eq!(identity_weight, &Weight::new(1));

    let administrator_account_weight = account_1
        .associated_keys()
        .get(&*DEFAULT_ADMIN_ACCOUNT_ADDR)
        .expect("should have special account");
    assert_eq!(administrator_account_weight, &Weight::MAX);

    let administrative_accounts = read_administrative_accounts(&builder);

    assert!(
        itertools::equal(administrative_accounts, [*DEFAULT_ADMIN_ACCOUNT_ADDR]),
        "administrators should be populated with single account"
    );

    let administrator_account = builder
        .get_account(*DEFAULT_ADMIN_ACCOUNT_ADDR)
        .expect("should create special account");
    assert_eq!(
        administrator_account.associated_keys().len(),
        1,
        "should not have duplicate identity key"
    );

    let identity_weight = administrator_account
        .associated_keys()
        .get(&*DEFAULT_ADMIN_ACCOUNT_ADDR)
        .expect("should have identity special key");
    assert_eq!(identity_weight, &Weight::new(1));
}

#[ignore]
#[test]
fn administrator_account_should_disable_any_account() {
    let mut builder = setup();

    let account_1_genesis = builder
        .get_account(*ACCOUNT_1_ADDR)
        .expect("should have account 1 after genesis");

    // Account 1 can deploy after genesis
    let exec_request_1 = ExecuteRequestBuilder::module_bytes(
        *ACCOUNT_1_ADDR,
        do_minimum_bytes(),
        RuntimeArgs::default(),
    )
    .build();
    builder.exec(exec_request_1).expect_success().commit();

    // Freeze account 1
    let freeze_request_1 = {
        let session_args = runtime_args! {
            ARG_ACCOUNT_HASH => *ACCOUNT_1_ADDR,
        };

        ExecuteRequestBuilder::contract_call_by_name(
            *DEFAULT_ADMIN_ACCOUNT_ADDR,
            CONTRACT_HASH_NAME,
            DISABLE_ACCOUNT_ENTRYPOINT,
            session_args,
        )
        .build()
    };

    builder.exec(freeze_request_1).expect_success().commit();
    // Account 1 can not deploy after freezing
    let exec_request_2 = ExecuteRequestBuilder::module_bytes(
        *ACCOUNT_1_ADDR,
        do_minimum_bytes(),
        RuntimeArgs::default(),
    )
    .build();
    builder.exec(exec_request_2).expect_failure().commit();

    let error = builder.get_error().expect("should have error");
    assert!(matches!(
        error,
        Error::Exec(execution::Error::DeploymentAuthorizationFailure)
    ));

    let account_1_frozen = builder
        .get_account(*ACCOUNT_1_ADDR)
        .expect("should have account 1 after genesis");
    assert_ne!(
        account_1_genesis, account_1_frozen,
        "account 1 should be modified"
    );

    // Unfreeze account 1
    let unfreeze_request_1 = {
        let session_args = runtime_args! {
            ARG_ACCOUNT_HASH => *ACCOUNT_1_ADDR,
        };

        ExecuteRequestBuilder::contract_call_by_name(
            *DEFAULT_ADMIN_ACCOUNT_ADDR,
            CONTRACT_HASH_NAME,
            ENABLE_ACCOUNT_ENTRYPOINT,
            session_args,
        )
        .build()
    };

    builder.exec(unfreeze_request_1).expect_success().commit();

    // Account 1 can deploy after unfreezing
    let exec_request_3 = ExecuteRequestBuilder::module_bytes(
        *ACCOUNT_1_ADDR,
        do_minimum_bytes(),
        RuntimeArgs::default(),
    )
    .build();
    builder.exec(exec_request_3).expect_success().commit();

    let account_1_unfrozen = builder
        .get_account(*ACCOUNT_1_ADDR)
        .expect("should have account 1 after genesis");
    assert_eq!(
        account_1_genesis, account_1_unfrozen,
        "account 1 should be modified back to genesis state"
    );
}

#[ignore]
#[test]
fn native_transfer_should_create_new_restricted_private_account() {
    let mut builder = setup();

    // Account 1 can deploy after genesis
    let transfer_args = runtime_args! {
        mint::ARG_TARGET => *ACCOUNT_2_ADDR,
        mint::ARG_AMOUNT => U512::one(),
        mint::ARG_ID => Some(1u64),
    };
    let transfer_request =
        ExecuteRequestBuilder::transfer(*DEFAULT_ADMIN_ACCOUNT_ADDR, transfer_args).build();

    let administrative_accounts = read_administrative_accounts(&builder);

    builder.exec(transfer_request).expect_success().commit();

    let account_2 = builder
        .get_account(*ACCOUNT_2_ADDR)
        .expect("should have account 1 after genesis");

    assert!(
        itertools::equal(
            account_2
                .associated_keys()
                .keys()
                .filter(|account_hash| *account_hash != &*ACCOUNT_2_ADDR), // skip identity key
            administrative_accounts.iter(),
        ),
        "newly created account should have administrator accounts set"
    );

    assert_eq!(
        account_2.action_thresholds(),
        &EXPECTED_PRIVATE_CHAIN_THRESHOLDS,
        "newly created account should have expected thresholds"
    );
}

#[ignore]
#[test]
fn wasm_transfer_should_create_new_restricted_private_account() {
    let mut builder = setup();

    // Account 1 can deploy after genesis
    let transfer_args = runtime_args! {
        mint::ARG_TARGET => *ACCOUNT_2_ADDR,
        mint::ARG_AMOUNT => 1u64,
    };
    let transfer_request = ExecuteRequestBuilder::standard(
        *DEFAULT_ADMIN_ACCOUNT_ADDR,
        TRANSFER_TO_ACCOUNT_CONTRACT,
        transfer_args,
    )
    .build();

    let administrative_accounts = read_administrative_accounts(&builder);

    builder.exec(transfer_request).expect_success().commit();

    let account_2 = builder
        .get_account(*ACCOUNT_2_ADDR)
        .expect("should have account 1 after genesis");

    assert!(
        itertools::equal(
            account_2
                .associated_keys()
                .keys()
                .filter(|account_hash| *account_hash != &*ACCOUNT_2_ADDR), // skip identity key
            administrative_accounts.iter(),
        ),
        "newly created account should have administrator accounts set"
    );

    assert_eq!(
        account_2.action_thresholds(),
        &EXPECTED_PRIVATE_CHAIN_THRESHOLDS,
        "newly created account should have expected thresholds"
    );
}

fn setup() -> InMemoryWasmTestBuilder {
    let engine_config = EngineConfigBuilder::default()
        .with_chain_kind(ChainKind::Private)
        .build();

    let mut builder = InMemoryWasmTestBuilder::new_with_config(engine_config);
    builder.run_genesis(&DEFAULT_PRIVATE_CHAIN_GENESIS);

    let exec_request = ExecuteRequestBuilder::standard(
        *DEFAULT_ADMIN_ACCOUNT_ADDR,
        ACCOUNT_MANAGEMENT_CONTRACT,
        RuntimeArgs::default(),
    )
    .build();

    builder.exec(exec_request).expect_success().commit();

    builder
}

fn read_administrative_accounts(builder: &InMemoryWasmTestBuilder) -> BTreeSet<AccountHash> {
    let mint_contract_hash = builder.get_mint_contract_hash();
    let mint_contract = builder
        .get_contract(mint_contract_hash)
        .expect("should create mint");
    let administrative_accounts_key = mint_contract
        .named_keys()
        .get(ADMINISTRATIVE_ACCOUNTS_KEY)
        .expect("special accounts should exist");
    let administrative_accounts_stored: StoredValue = builder
        .query(None, *administrative_accounts_key, &[])
        .expect("should query special accounts");
    let administrative_accounts_cl_value: CLValue = administrative_accounts_stored
        .into_clvalue()
        .expect("should have clvalue");
    administrative_accounts_cl_value
        .into_t()
        .expect("should have a list of account hashes")
}
