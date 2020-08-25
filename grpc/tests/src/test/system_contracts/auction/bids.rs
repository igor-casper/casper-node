use lazy_static::lazy_static;

use auction::{
    EraId, SeigniorageRecipients, ARG_DELEGATOR, ARG_PUBLIC_KEY, AUCTION_DELAY, ERA_ID_KEY,
    INITIAL_ERA_ID, SNAPSHOT_SIZE,
};
use casperlabs_engine_test_support::{
    internal::{
        utils, ExecuteRequestBuilder, InMemoryWasmTestBuilder, DEFAULT_ACCOUNTS,
        DEFAULT_ACCOUNT_PUBLIC_KEY, DEFAULT_RUN_GENESIS_REQUEST,
    },
    DEFAULT_ACCOUNT_ADDR,
};
use casperlabs_node::{
    crypto::asymmetric_key::{PublicKey, SecretKey},
    types::Motes,
    GenesisAccount,
};
use casperlabs_types::{
    self,
    account::AccountHash,
    auction::{
        self, ActiveBids, DelegationRate, Delegators, EraValidators, FoundingValidators,
        UnbondingPurses, ARG_AMOUNT, ARG_DELEGATION_RATE, ARG_VALIDATOR, DEFAULT_UNBONDING_DELAY,
        FOUNDING_VALIDATORS_KEY,
    },
    bytesrepr::FromBytes,
    runtime_args, CLTyped, ContractHash, RuntimeArgs, U512,
};
use std::iter::FromIterator;

const ARG_ENTRY_POINT: &str = "entry_point";

const CONTRACT_TRANSFER_TO_ACCOUNT: &str = "transfer_to_account_u512.wasm";
const CONTRACT_AUCTION_BIDS: &str = "auction_bids.wasm";
const TRANSFER_AMOUNT: u64 = 250_000_000 + 1000;
const SYSTEM_ADDR: AccountHash = AccountHash::new([0u8; 32]);
lazy_static! {
    static ref NON_FOUNDER_VALIDATOR_1_SK: SecretKey = SecretKey::new_ed25519([55; 32]);
    static ref NON_FOUNDER_VALIDATOR_1: PublicKey = PublicKey::from(&*NON_FOUNDER_VALIDATOR_1_SK);
}

const ADD_BID_AMOUNT_1: u64 = 95_000;
const ADD_BID_DELEGATION_RATE_1: DelegationRate = 125;
const BID_AMOUNT_2: u64 = 5_000;
const ADD_BID_DELEGATION_RATE_2: DelegationRate = 126;
const WITHDRAW_BID_AMOUNT_2: u64 = 15_000;

const ARG_ADD_BID: &str = "add_bid";
const ARG_WITHDRAW_BID: &str = "withdraw_bid";
const ARG_DELEGATE: &str = "delegate";
const ARG_UNDELEGATE: &str = "undelegate";
const ARG_RUN_AUCTION: &str = "run_auction";
const ARG_READ_SEIGNIORAGE_RECIPIENTS: &str = "read_seigniorage_recipients";

const DELEGATE_AMOUNT_1: u64 = 125_000;
const DELEGATE_AMOUNT_2: u64 = 15_000;
const UNDELEGATE_AMOUNT_1: u64 = 35_000;

lazy_static! {
    static ref ACCOUNT_1_SK: SecretKey = SecretKey::new_ed25519([200; 32]);
    static ref ACCOUNT_1_PK: PublicKey = PublicKey::from(&*ACCOUNT_1_SK);
    static ref ACCOUNT_1_ADDR: AccountHash = ACCOUNT_1_PK.to_account_hash();
}
const ACCOUNT_1_BALANCE: u64 = 10_000_000;
const ACCOUNT_1_BOND: u64 = 100_000;

lazy_static! {
    static ref ACCOUNT_2_SK: SecretKey = SecretKey::new_ed25519([201; 32]);
    static ref ACCOUNT_2_PK: PublicKey = PublicKey::from(&*ACCOUNT_2_SK);
    static ref ACCOUNT_2_ADDR: AccountHash = ACCOUNT_2_PK.to_account_hash();
}
const ACCOUNT_2_BALANCE: u64 = 25_000_000;
const ACCOUNT_2_BOND: u64 = 200_000;

lazy_static! {
    static ref BID_ACCOUNT_SK: SecretKey = SecretKey::new_ed25519([202; 32]);
    static ref BID_ACCOUNT_PK: PublicKey = PublicKey::from(&*BID_ACCOUNT_SK);
    static ref BID_ACCOUNT_ADDR: AccountHash = BID_ACCOUNT_PK.to_account_hash();
}

fn get_value<T>(builder: &mut InMemoryWasmTestBuilder, contract_hash: ContractHash, name: &str) -> T
where
    T: FromBytes + CLTyped,
{
    let contract = builder
        .get_contract(contract_hash)
        .expect("should have contract");
    let key = contract.named_keys().get(name).expect("should have purse");
    let stored_value = builder.query(None, *key, &[]).expect("should query");
    let cl_value = stored_value
        .as_cl_value()
        .cloned()
        .expect("should be cl value");
    let result: T = cl_value.into_t().expect("should convert");
    result
}

#[ignore]
#[test]
fn should_run_add_bid() {
    let mut builder = InMemoryWasmTestBuilder::default();

    builder.run_genesis(&DEFAULT_RUN_GENESIS_REQUEST);

    let auction_hash = builder.get_auction_contract_hash();

    //
    let exec_request_1 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_PUBLIC_KEY => casperlabs_types::PublicKey::from(*BID_ACCOUNT_PK),
            ARG_ENTRY_POINT => ARG_ADD_BID,
            ARG_AMOUNT => U512::from(ADD_BID_AMOUNT_1),
            ARG_DELEGATION_RATE => ADD_BID_DELEGATION_RATE_1,
        },
    )
    .build();

    builder.exec(exec_request_1).commit().expect_success();

    let active_bids: ActiveBids = get_value(&mut builder, auction_hash, "active_bids");

    assert_eq!(active_bids.len(), 1);

    let active_bid = active_bids.get(&BID_ACCOUNT_PK.clone().into()).unwrap();
    assert_eq!(
        builder.get_purse_balance(active_bid.bid_purse),
        U512::from(ADD_BID_AMOUNT_1)
    );
    assert_eq!(active_bid.delegation_rate, ADD_BID_DELEGATION_RATE_1);

    // 2nd bid top-up
    let exec_request_2 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_PUBLIC_KEY => casperlabs_types::PublicKey::from(*BID_ACCOUNT_PK),
            ARG_ENTRY_POINT => ARG_ADD_BID,
            ARG_AMOUNT => U512::from(BID_AMOUNT_2),
            ARG_DELEGATION_RATE => ADD_BID_DELEGATION_RATE_2,
        },
    )
    .build();

    builder.exec(exec_request_2).commit().expect_success();

    let active_bids: ActiveBids = get_value(&mut builder, auction_hash, "active_bids");

    assert_eq!(active_bids.len(), 1);

    let active_bid = active_bids.get(&BID_ACCOUNT_PK.clone().into()).unwrap();
    assert_eq!(
        builder.get_purse_balance(active_bid.bid_purse),
        U512::from(ADD_BID_AMOUNT_1 + BID_AMOUNT_2)
    );
    assert_eq!(active_bid.delegation_rate, ADD_BID_DELEGATION_RATE_2);

    // 3. withdraw some amount
    let exec_request_3 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_ENTRY_POINT => ARG_WITHDRAW_BID,
            ARG_PUBLIC_KEY => casperlabs_types::PublicKey::from(*BID_ACCOUNT_PK),
            ARG_AMOUNT => U512::from(WITHDRAW_BID_AMOUNT_2),
        },
    )
    .build();
    builder.exec(exec_request_3).commit().expect_success();

    let active_bids: ActiveBids = get_value(&mut builder, auction_hash, "active_bids");

    assert_eq!(active_bids.len(), 1);

    let active_bid = active_bids.get(&BID_ACCOUNT_PK.clone().into()).unwrap();
    assert_eq!(
        builder.get_purse_balance(active_bid.bid_purse),
        U512::from(ADD_BID_AMOUNT_1 + BID_AMOUNT_2) // Since we don't pay out immediately `WITHDRAW_BID_AMOUNT_2` is locked in unbonding queue
    );
    let unbonding_purses: UnbondingPurses =
        get_value(&mut builder, auction_hash, "unbonding_purses");
    let unbond_list = unbonding_purses
        .get(&casperlabs_types::PublicKey::from(*BID_ACCOUNT_PK))
        .expect("should have unbond");
    assert_eq!(unbond_list.len(), 1);
    assert_eq!(
        unbond_list[0].origin,
        casperlabs_types::PublicKey::from(*BID_ACCOUNT_PK)
    );
    // `WITHDRAW_BID_AMOUNT_2` is in unbonding list
    assert_eq!(
        builder.get_purse_balance(unbond_list[0].purse),
        U512::zero(),
    );
    assert_eq!(unbond_list[0].amount, U512::from(WITHDRAW_BID_AMOUNT_2),);

    assert_eq!(
        unbond_list[0].era_of_withdrawal,
        INITIAL_ERA_ID + DEFAULT_UNBONDING_DELAY,
    );
}

#[ignore]
#[test]
fn should_run_delegate_and_undelegate() {
    let mut builder = InMemoryWasmTestBuilder::default();

    let transfer_request_1 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_TRANSFER_TO_ACCOUNT,
        runtime_args! {
            "target" => SYSTEM_ADDR,
            ARG_AMOUNT => U512::from(TRANSFER_AMOUNT)
        },
    )
    .build();

    let transfer_request_2 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_TRANSFER_TO_ACCOUNT,
        runtime_args! {
            "target" => NON_FOUNDER_VALIDATOR_1.to_account_hash(),
            ARG_AMOUNT => U512::from(TRANSFER_AMOUNT)
        },
    )
    .build();

    // non-founding validator request
    let add_bid_request_1 = ExecuteRequestBuilder::standard(
        NON_FOUNDER_VALIDATOR_1.to_account_hash(),
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_PUBLIC_KEY => casperlabs_types::PublicKey::from(*NON_FOUNDER_VALIDATOR_1),
            ARG_ENTRY_POINT => ARG_ADD_BID,
            ARG_AMOUNT => U512::from(ADD_BID_AMOUNT_1),
            ARG_DELEGATION_RATE => ADD_BID_DELEGATION_RATE_1,
        },
    )
    .build();

    builder.run_genesis(&DEFAULT_RUN_GENESIS_REQUEST);
    builder.exec(transfer_request_1).commit().expect_success();
    builder.exec(transfer_request_2).commit().expect_success();
    builder.exec(add_bid_request_1).commit().expect_success();

    let auction_hash = builder.get_auction_contract_hash();

    let active_bids: ActiveBids = get_value(&mut builder, auction_hash, "active_bids");
    assert_eq!(active_bids.len(), 1);
    let active_bid = active_bids
        .get(&casperlabs_types::PublicKey::from(*NON_FOUNDER_VALIDATOR_1))
        .unwrap();
    assert_eq!(
        builder.get_purse_balance(active_bid.bid_purse),
        U512::from(ADD_BID_AMOUNT_1)
    );
    assert_eq!(active_bid.delegation_rate, ADD_BID_DELEGATION_RATE_1);

    let auction_stored_value = builder
        .query(None, auction_hash.into(), &[])
        .expect("should query auction hash");
    let _auction = auction_stored_value
        .as_contract()
        .expect("should be contract");

    //
    let exec_request_1 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_ENTRY_POINT => ARG_DELEGATE,
            ARG_AMOUNT => U512::from(DELEGATE_AMOUNT_1),
            ARG_VALIDATOR => casperlabs_types::PublicKey::from(*NON_FOUNDER_VALIDATOR_1),
            ARG_DELEGATOR => casperlabs_types::PublicKey::from(*BID_ACCOUNT_PK),
        },
    )
    .build();

    builder.exec(exec_request_1).commit().expect_success();
    let delegators: Delegators = get_value(&mut builder, auction_hash, "delegators");
    assert_eq!(delegators.len(), 1);

    let delegated_amount_1 = delegators
        .get(&NON_FOUNDER_VALIDATOR_1.clone().into())
        .and_then(|map| map.get(&BID_ACCOUNT_PK.clone().into()))
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        delegated_amount_1,
        U512::from(DELEGATE_AMOUNT_1),
        "{:?}",
        delegators
    );

    // 2nd bid top-up
    let exec_request_2 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_ENTRY_POINT => ARG_DELEGATE,
            ARG_AMOUNT => U512::from(DELEGATE_AMOUNT_2),
            ARG_VALIDATOR => casperlabs_types::PublicKey::from(*NON_FOUNDER_VALIDATOR_1),
            ARG_DELEGATOR => casperlabs_types::PublicKey::from(*BID_ACCOUNT_PK),
        },
    )
    .build();

    builder.exec(exec_request_2).commit().expect_success();

    let delegators: Delegators = get_value(&mut builder, auction_hash, "delegators");
    assert_eq!(delegators.len(), 1);

    let delegated_amount_2 = delegators
        .get(&casperlabs_types::PublicKey::from(*NON_FOUNDER_VALIDATOR_1))
        .and_then(|map| map.get(&BID_ACCOUNT_PK.clone().into()))
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        delegated_amount_2,
        U512::from(DELEGATE_AMOUNT_1 + DELEGATE_AMOUNT_2),
        "{:?}",
        delegators
    );

    let exec_request_3 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_ENTRY_POINT => ARG_UNDELEGATE,
            ARG_AMOUNT => U512::from(UNDELEGATE_AMOUNT_1),
            ARG_VALIDATOR => casperlabs_types::PublicKey::from(*NON_FOUNDER_VALIDATOR_1),
            ARG_DELEGATOR => casperlabs_types::PublicKey::from(*BID_ACCOUNT_PK),
        },
    )
    .build();
    builder.exec(exec_request_3).commit().expect_success();

    let active_bids: ActiveBids = get_value(&mut builder, auction_hash, "active_bids");

    assert_eq!(active_bids.len(), 1);

    let delegators: Delegators = get_value(&mut builder, auction_hash, "delegators");
    assert_eq!(delegators.len(), 1);

    let delegated_amount_3 = delegators
        .get(&NON_FOUNDER_VALIDATOR_1.clone().into())
        .and_then(|map| map.get(&BID_ACCOUNT_PK.clone().into()))
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        delegated_amount_3,
        U512::from(DELEGATE_AMOUNT_1 + DELEGATE_AMOUNT_2 - UNDELEGATE_AMOUNT_1),
        "{:?}",
        delegators
    );
}

#[ignore]
#[test]
fn should_calculate_era_validators() {
    assert_ne!(
        ACCOUNT_1_PK.to_account_hash(),
        ACCOUNT_2_PK.to_account_hash()
    );
    assert_ne!(
        ACCOUNT_2_PK.to_account_hash(),
        BID_ACCOUNT_PK.to_account_hash()
    );
    assert_ne!(
        ACCOUNT_2_PK.to_account_hash(),
        DEFAULT_ACCOUNT_PUBLIC_KEY.to_account_hash()
    );
    let accounts = {
        let mut tmp: Vec<GenesisAccount> = DEFAULT_ACCOUNTS.clone();
        let account_1 = GenesisAccount::with_public_key(
            *ACCOUNT_1_PK,
            Motes::new(ACCOUNT_1_BALANCE.into()),
            Motes::new(ACCOUNT_1_BOND.into()),
        );
        let account_2 = GenesisAccount::with_public_key(
            *ACCOUNT_2_PK,
            Motes::new(ACCOUNT_2_BALANCE.into()),
            Motes::new(ACCOUNT_2_BOND.into()),
        );
        tmp.push(account_1);
        tmp.push(account_2);
        tmp
    };

    let run_genesis_request = utils::create_run_genesis_request(accounts);

    let mut builder = InMemoryWasmTestBuilder::default();

    let transfer_request_1 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_TRANSFER_TO_ACCOUNT,
        runtime_args! {
            "target" => SYSTEM_ADDR,
            ARG_AMOUNT => U512::from(TRANSFER_AMOUNT)
        },
    )
    .build();
    let transfer_request_2 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_TRANSFER_TO_ACCOUNT,
        runtime_args! {
            "target" => NON_FOUNDER_VALIDATOR_1.to_account_hash(),
            ARG_AMOUNT => U512::from(TRANSFER_AMOUNT)
        },
    )
    .build();

    builder.run_genesis(&run_genesis_request);

    let auction_hash = builder.get_auction_contract_hash();
    let founding_validators: FoundingValidators =
        get_value(&mut builder, auction_hash, FOUNDING_VALIDATORS_KEY);
    assert_eq!(
        founding_validators.len(),
        2,
        "founding validators {:?}",
        founding_validators
    );

    builder.exec(transfer_request_1).commit().expect_success();
    builder.exec(transfer_request_2).commit().expect_success();

    // non-founding validator request
    let add_bid_request_1 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_PUBLIC_KEY => casperlabs_types::PublicKey::from(*BID_ACCOUNT_PK),
            ARG_ENTRY_POINT => ARG_ADD_BID,
            ARG_AMOUNT => U512::from(ADD_BID_AMOUNT_1),
            ARG_DELEGATION_RATE => ADD_BID_DELEGATION_RATE_1,
        },
    )
    .build();

    builder.exec(add_bid_request_1).commit().expect_success();

    let pre_era_id: EraId = get_value(&mut builder, auction_hash, ERA_ID_KEY);
    assert_eq!(pre_era_id, 0);

    // non-founding validator request
    let run_auction_request_1 = ExecuteRequestBuilder::standard(
        SYSTEM_ADDR,
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_ENTRY_POINT => ARG_RUN_AUCTION,
        },
    )
    .build();

    builder
        .exec(run_auction_request_1)
        .commit()
        .expect_success();

    let post_era_id: EraId = get_value(&mut builder, auction_hash, ERA_ID_KEY);
    assert_eq!(post_era_id, 1);

    let era_validators: EraValidators = get_value(&mut builder, auction_hash, "era_validators");

    // Check if there are no missing eras after the calculation, but we don't care about what the elements are
    let eras = Vec::from_iter(era_validators.keys().copied());
    assert!(!era_validators.is_empty());
    assert!(era_validators.len() >= AUCTION_DELAY as usize); // definetely more than 1 element
    let (first_era, _) = era_validators.iter().min().unwrap();
    let (last_era, _) = era_validators.iter().max().unwrap();
    let expected_eras: Vec<EraId> = (*first_era..=*last_era).collect();
    assert_eq!(eras, expected_eras, "Eras {:?}", eras);

    assert!(post_era_id > 0);
    let consensus_next_era_id: EraId = AUCTION_DELAY + 1 + post_era_id;

    assert_eq!(
        era_validators.len(),
        SNAPSHOT_SIZE,
        "era_id={} {:?}",
        consensus_next_era_id,
        era_validators
    ); // eraindex==1 - ran once

    let validator_weights = era_validators
        .get(&(consensus_next_era_id - 1)) // indexed from 0
        .unwrap_or_else(|| {
            panic!(
                "should have era_index=={} entry {:?}",
                consensus_next_era_id, era_validators
            )
        });
    assert_eq!(
        validator_weights.len(),
        3,
        "{:?} {:?}",
        era_validators,
        validator_weights
    ); //2 genesis validators "winners"
    assert_eq!(
        validator_weights
            .get(&casperlabs_types::PublicKey::from(*BID_ACCOUNT_PK))
            .expect("should have bid account in this era"),
        &U512::from(ADD_BID_AMOUNT_1)
    );
}

#[ignore]
#[test]
fn should_get_first_seigniorage_recipients() {
    let accounts = {
        let mut tmp: Vec<GenesisAccount> = DEFAULT_ACCOUNTS.clone();
        let account_1 = GenesisAccount::with_public_key(
            *ACCOUNT_1_PK,
            Motes::new(ACCOUNT_1_BALANCE.into()),
            Motes::new(ACCOUNT_1_BOND.into()),
        );
        let account_2 = GenesisAccount::with_public_key(
            *ACCOUNT_2_PK,
            Motes::new(ACCOUNT_2_BALANCE.into()),
            Motes::new(ACCOUNT_2_BOND.into()),
        );
        tmp.push(account_1);
        tmp.push(account_2);
        tmp
    };

    let run_genesis_request = utils::create_run_genesis_request(accounts);

    let mut builder = InMemoryWasmTestBuilder::default();

    builder.run_genesis(&run_genesis_request);

    let transfer_request_1 = ExecuteRequestBuilder::standard(
        *DEFAULT_ACCOUNT_ADDR,
        CONTRACT_TRANSFER_TO_ACCOUNT,
        runtime_args! {
            "target" => SYSTEM_ADDR,
            ARG_AMOUNT => U512::from(TRANSFER_AMOUNT)
        },
    )
    .build();

    let auction_hash = builder.get_auction_contract_hash();
    let founding_validators: FoundingValidators =
        get_value(&mut builder, auction_hash, FOUNDING_VALIDATORS_KEY);
    assert_eq!(founding_validators.len(), 2);
    assert!(founding_validators
        .get(&casperlabs_types::PublicKey::from(*ACCOUNT_1_PK))
        .is_some());
    assert!(founding_validators
        .get(&casperlabs_types::PublicKey::from(*ACCOUNT_2_PK))
        .is_some());

    builder.exec(transfer_request_1).commit().expect_success();

    // run_auction should be executed first
    let exec_request_1 = ExecuteRequestBuilder::standard(
        SYSTEM_ADDR,
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_ENTRY_POINT => ARG_RUN_AUCTION,
        },
    )
    .build();

    builder.exec(exec_request_1).commit().expect_success();

    // read seigniorage recipients
    let exec_request_2 = ExecuteRequestBuilder::standard(
        SYSTEM_ADDR,
        CONTRACT_AUCTION_BIDS,
        runtime_args! {
            ARG_ENTRY_POINT => ARG_READ_SEIGNIORAGE_RECIPIENTS,
        },
    )
    .build();

    builder.exec(exec_request_2).commit().expect_success();

    let account = builder.get_account(SYSTEM_ADDR).unwrap();
    let key = account
        .named_keys()
        .get("seigniorage_recipients_result")
        .copied()
        .unwrap();
    let stored_value = builder.query(None, key, &[]).unwrap();
    let seigniorage_recipients: SeigniorageRecipients = stored_value
        .as_cl_value()
        .cloned()
        .unwrap()
        .into_t()
        .unwrap();
    assert_eq!(seigniorage_recipients.len(), 2);

    let era_validators: EraValidators = get_value(&mut builder, auction_hash, "era_validators");
    assert_eq!(
        era_validators.len(),
        SNAPSHOT_SIZE, // 1 because `0..AUCTION_DELAY` is an inclusive range and +1 because one `run_auction` was ran
        "{:?}",
        era_validators
    ); // eraindex==1 - ran once

    assert!(era_validators.contains_key(&(AUCTION_DELAY as u64 + 1)));

    let era_id = AUCTION_DELAY - 1;

    let validator_weights = era_validators.get(&era_id).unwrap_or_else(|| {
        panic!(
            "should have era_index=={} entry {:?}",
            era_id, era_validators
        )
    });
    assert_eq!(validator_weights.len(), 2, "{:?}", validator_weights); // 2 genesis validators "winners" with non-zero bond
    assert_eq!(
        validator_weights
            .get(&casperlabs_types::PublicKey::from(*ACCOUNT_1_PK))
            .unwrap(),
        &U512::from(ACCOUNT_1_BOND)
    );
    assert_eq!(
        validator_weights
            .get(&casperlabs_types::PublicKey::from(*ACCOUNT_2_PK))
            .unwrap(),
        &U512::from(ACCOUNT_2_BOND)
    );
}
