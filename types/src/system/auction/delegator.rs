// TODO - remove once schemars stops causing warning.
#![allow(clippy::field_reassign_with_default)]

use alloc::vec::Vec;

use borsh::{BorshDeserialize, BorshSerialize};
#[cfg(feature = "datasize")]
use datasize::DataSize;
#[cfg(feature = "json-schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    bytesrepr::{self, FromBytes, ToBytes},
    system::auction::{bid::VestingSchedule, Error},
    CLType, CLTyped, PublicKey, URef, U512,
};

/// Represents a party delegating their stake to a validator (or "delegatee")
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[cfg_attr(feature = "datasize", derive(DataSize))]
#[cfg_attr(feature = "json-schema", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Delegator {
    delegator_public_key: PublicKey,
    staked_amount: U512,
    bonding_purse: URef,
    validator_public_key: PublicKey,
    vesting_schedule: Option<VestingSchedule>,
}

impl Delegator {
    /// Creates a new [`Delegator`]
    pub fn unlocked(
        delegator_public_key: PublicKey,
        staked_amount: U512,
        bonding_purse: URef,
        validator_public_key: PublicKey,
    ) -> Self {
        let vesting_schedule = None;
        Delegator {
            delegator_public_key,
            staked_amount,
            bonding_purse,
            validator_public_key,
            vesting_schedule,
        }
    }

    /// Creates new instance of a [`Delegator`] with locked funds.
    pub fn locked(
        delegator_public_key: PublicKey,
        staked_amount: U512,
        bonding_purse: URef,
        validator_public_key: PublicKey,
        release_timestamp_millis: u64,
    ) -> Self {
        let vesting_schedule = Some(VestingSchedule::new(release_timestamp_millis));
        Delegator {
            delegator_public_key,
            staked_amount,
            bonding_purse,
            validator_public_key,
            vesting_schedule,
        }
    }

    /// Returns public key of the delegator.
    pub fn delegator_public_key(&self) -> &PublicKey {
        &self.delegator_public_key
    }

    /// Returns the staked amount
    pub fn staked_amount(&self) -> &U512 {
        &self.staked_amount
    }

    /// Returns the mutable staked amount
    pub fn staked_amount_mut(&mut self) -> &mut U512 {
        &mut self.staked_amount
    }

    /// Returns the bonding purse
    pub fn bonding_purse(&self) -> &URef {
        &self.bonding_purse
    }

    /// Returns delegatee
    pub fn validator_public_key(&self) -> &PublicKey {
        &self.validator_public_key
    }

    /// Decreases the stake of the provided bid
    pub fn decrease_stake(
        &mut self,
        amount: U512,
        era_end_timestamp_millis: u64,
    ) -> Result<U512, Error> {
        let updated_staked_amount = self
            .staked_amount
            .checked_sub(amount)
            .ok_or(Error::InvalidAmount)?;

        let vesting_schedule = match self.vesting_schedule.as_ref() {
            Some(vesting_schedule) => vesting_schedule,
            None => {
                self.staked_amount = updated_staked_amount;
                return Ok(updated_staked_amount);
            }
        };

        match vesting_schedule.locked_amount(era_end_timestamp_millis) {
            Some(locked_amount) if updated_staked_amount < locked_amount => {
                Err(Error::DelegatorFundsLocked)
            }
            None => {
                // If `None`, then the locked amounts table has yet to be initialized (likely
                // pre-90 day mark)
                Err(Error::DelegatorFundsLocked)
            }
            Some(_) => {
                self.staked_amount = updated_staked_amount;
                Ok(updated_staked_amount)
            }
        }
    }

    /// Increases the stake of the provided bid
    pub fn increase_stake(&mut self, amount: U512) -> Result<U512, Error> {
        let updated_staked_amount = self
            .staked_amount
            .checked_add(amount)
            .ok_or(Error::InvalidAmount)?;

        self.staked_amount = updated_staked_amount;

        Ok(updated_staked_amount)
    }

    /// Returns a reference to the vesting schedule of the provided
    /// delegator bid.  `None` if a non-genesis validator.
    pub fn vesting_schedule(&self) -> Option<&VestingSchedule> {
        self.vesting_schedule.as_ref()
    }

    /// Returns a mutable reference to the vesting schedule of the provided
    /// delegator bid.  `None` if a non-genesis validator.
    pub fn vesting_schedule_mut(&mut self) -> Option<&mut VestingSchedule> {
        self.vesting_schedule.as_mut()
    }
}

impl CLTyped for Delegator {
    fn cl_type() -> CLType {
        CLType::Any
    }
}
