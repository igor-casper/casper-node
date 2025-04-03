use super::utils::{Assertion, TestStateSnapshot};
use crate::reactor::main_reactor::tests::transactions::{
    assert_exec_result_cost, exec_result_is_success,
};
use async_trait::async_trait;
use casper_types::{Gas, TransactionHash, U512};
use std::collections::BTreeMap;

pub(crate) struct TransactionSuccessful {
    hash: TransactionHash,
}

impl TransactionSuccessful {
    pub(crate) fn new(hash: TransactionHash) -> Self {
        Self { hash }
    }
}

#[async_trait]
impl Assertion for TransactionSuccessful {
    async fn assert(&self, snapshots_at_heights: BTreeMap<u64, TestStateSnapshot>) {
        let current_state = snapshots_at_heights.last_key_value().unwrap().1;
        assert!(current_state.exec_infos.contains_key(&self.hash));
        let exec_info = current_state.exec_infos.get(&self.hash).unwrap();
        assert!(exec_info.execution_result.is_some());
        let result = exec_info.execution_result.as_ref().unwrap();
        assert!(exec_result_is_success(result));
    }
}

pub(crate) struct TransactionFailure {
    hash: TransactionHash,
}

impl TransactionFailure {
    pub(crate) fn new(hash: TransactionHash) -> Self {
        Self { hash }
    }
}

#[async_trait]
impl Assertion for TransactionFailure {
    async fn assert(&self, snapshots_at_heights: BTreeMap<u64, TestStateSnapshot>) {
        let current_state = snapshots_at_heights.last_key_value().unwrap().1;
        assert!(current_state.exec_infos.contains_key(&self.hash));
        let exec_info = current_state.exec_infos.get(&self.hash).unwrap();
        assert!(exec_info.execution_result.is_some());
        let result = exec_info.execution_result.as_ref().unwrap();
        assert!(!exec_result_is_success(result));
    }
}

pub(crate) struct ExecResultCost {
    hash: TransactionHash,
    expected_cost: U512,
    expected_consumed_gas: Gas,
}

impl ExecResultCost {
    pub(crate) fn new(
        hash: TransactionHash,
        expected_cost: U512,
        expected_consumed_gas: Gas,
    ) -> Self {
        Self {
            hash,
            expected_cost,
            expected_consumed_gas,
        }
    }
}

#[async_trait]
impl Assertion for ExecResultCost {
    async fn assert(&self, snapshots_at_heights: BTreeMap<u64, TestStateSnapshot>) {
        let current_state = snapshots_at_heights.last_key_value().unwrap().1;
        assert!(current_state.exec_infos.contains_key(&self.hash));
        let exec_info = current_state.exec_infos.get(&self.hash).unwrap();
        assert!(exec_info.execution_result.is_some());
        let result = exec_info.execution_result.as_ref().unwrap();
        assert_exec_result_cost(
            result.clone(),
            self.expected_cost,
            self.expected_consumed_gas,
            "transfer_cost_fixed_price_no_fee_no_refund",
        );
    }
}

pub(crate) struct TotalSupplyChange {
    //It's an signed integer since we can expect either an increase or decrease.
    total_supply_change: i64,
    at_block_height: u64,
}

impl TotalSupplyChange {
    pub(crate) fn new(total_supply_change: i64, at_block_height: u64) -> Self {
        Self {
            total_supply_change,
            at_block_height,
        }
    }
}

#[async_trait]
impl Assertion for TotalSupplyChange {
    async fn assert(&self, snapshots_at_heights: BTreeMap<u64, TestStateSnapshot>) {
        let before = snapshots_at_heights.get(&1).unwrap();
        let after = snapshots_at_heights.get(&self.at_block_height).unwrap();
        let before_total_supply = before.total_supply;
        let got = after.total_supply;
        let total_supply = self.total_supply_change;
        let expected = if total_supply > 0 {
            before_total_supply
                .checked_add((total_supply.unsigned_abs()).into())
                .unwrap()
        } else {
            before_total_supply
                .checked_sub((total_supply.unsigned_abs()).into())
                .unwrap()
        };
        assert_eq!(expected, got);
    }
}

pub(crate) struct BobExpectBalanceChange {
    //It's an signed integer since we can expect either an increase or decrease.
    total_balance_change: i64,
    //It's an signed integer since we can expect either an increase or decrease.
    available_balance_change: i64,
}

impl BobExpectBalanceChange {
    pub(crate) fn new(total_balance_change: i64, available_balance_change: i64) -> Self {
        Self {
            total_balance_change,
            available_balance_change,
        }
    }
}

#[async_trait]
impl Assertion for BobExpectBalanceChange {
    async fn assert(&self, snapshots_at_heights: BTreeMap<u64, TestStateSnapshot>) {
        let before = snapshots_at_heights.get(&0).unwrap();
        let after = snapshots_at_heights.last_key_value().unwrap().1;
        let before_total = before.bob_balance.total.as_u64();
        let before_available = before.bob_balance.available.as_u64();
        let after_total = after.bob_balance.total.as_u64();
        let after_available = after.bob_balance.available.as_u64();
        assert_eq!(
            after_total as i64,
            before_total as i64 + self.total_balance_change
        );
        assert_eq!(
            after_available as i64,
            before_available as i64 + self.available_balance_change
        );
    }
}

pub(crate) struct AliceExpectBalanceChange {
    //It's an signed integer since we can expect either an increase or decrease.
    total_balance_change: i64,
    //It's an signed integer since we can expect either an increase or decrease.
    available_balance_change: i64,
}

impl AliceExpectBalanceChange {
    pub(crate) fn new(total_balance_change: i64, available_balance_change: i64) -> Self {
        Self {
            total_balance_change,
            available_balance_change,
        }
    }
}

#[async_trait]
impl Assertion for AliceExpectBalanceChange {
    async fn assert(&self, snapshots_at_heights: BTreeMap<u64, TestStateSnapshot>) {
        let before = snapshots_at_heights.get(&0).unwrap();
        let after = snapshots_at_heights.last_key_value().unwrap().1;
        let before_total = before.alice_balance.total.as_u64();
        let before_available = before.alice_balance.available.as_u64();
        let after_total = after.alice_balance.total.as_u64();
        let after_available = after.alice_balance.available.as_u64();
        assert_eq!(
            after_total as i64,
            before_total as i64 + self.total_balance_change
        );
        assert_eq!(
            after_available as i64,
            before_available as i64 + self.available_balance_change
        );
    }
}

pub(crate) struct AliceTotalMeetsAvailable {}

impl AliceTotalMeetsAvailable {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Assertion for AliceTotalMeetsAvailable {
    async fn assert(&self, snapshots_at_heights: BTreeMap<u64, TestStateSnapshot>) {
        let after = snapshots_at_heights.last_key_value().unwrap().1;
        let after_total = after.alice_balance.total;
        let after_available = after.alice_balance.available;
        assert_eq!(after_total, after_available);
    }
}

pub(crate) struct CharlieExpectBalanceChange {
    //It's an signed integer since we can expect either an increase or decrease.
    total_balance_change: i64,
    //It's an signed integer since we can expect either an increase or decrease.
    available_balance_change: i64,
}

impl CharlieExpectBalanceChange {
    pub(crate) fn new(total_balance_change: i64, available_balance_change: i64) -> Self {
        Self {
            total_balance_change,
            available_balance_change,
        }
    }
}

#[async_trait]
impl Assertion for CharlieExpectBalanceChange {
    async fn assert(&self, snapshots_at_heights: BTreeMap<u64, TestStateSnapshot>) {
        let before = snapshots_at_heights.get(&0).unwrap();
        let after = snapshots_at_heights.last_key_value().unwrap().1;
        let before_total = before.charlie_balance.total.as_u64();
        let before_available = before.charlie_balance.available.as_u64();
        let after_total = after.charlie_balance.total.as_u64();
        let after_available = after.charlie_balance.available.as_u64();
        assert_eq!(
            after_total as i64,
            before_total as i64 + self.total_balance_change
        );
        assert_eq!(
            after_available as i64,
            before_available as i64 + self.available_balance_change
        );
    }
}
