use core::fmt;

use crate::{Motes, U512};

const GAS_MAX_AS_U512: U512 = U512([u64::MAX, 0, 0, 0, 0, 0, 0, 0]);

/// The `Gas` struct represents an amount of gas used during execution.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct Gas(u64);

impl Gas {
    /// Maximum value a [`Gas`] struct can hold.
    pub const MAX: Gas = Gas::new(u64::MAX);
    /// Minimum value a [`Gas`] struct can hold.
    pub const MIN: Gas = Gas::new(u64::MIN);

    /// Constructs a new `Gas`.
    pub const fn new(value: u64) -> Self {
        Gas(value)
    }

    /// Creates a [`Gas`] of zero.
    pub const fn zero() -> Self {
        Self(0)
    }

    /// Returns true if this [`Gas`] instance contains a zero.
    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    fn from_u512(value: U512) -> Option<Gas> {
        if value > GAS_MAX_AS_U512 {
            return None;
        }
        Some(Gas(value.as_u64()))
    }

    /// Converts the given `motes` to `Gas` by dividing them by `conv_rate`.
    ///
    /// Returns `None` if `conv_rate == 0`.
    pub fn from_motes(motes: Motes, conv_rate: u64) -> Option<Self> {
        motes
            .value()
            .checked_div(U512::from(conv_rate))
            .and_then(Self::from_u512)
    }

    /// Performs checked add operation.
    pub fn checked_add(&self, v: &Gas) -> Option<Gas> {
        self.0.checked_add(v.0).map(Gas)
    }

    /// Performs checked sub operation.
    pub fn checked_sub(&self, v: &Gas) -> Option<Gas> {
        self.0.checked_sub(v.0).map(Gas)
    }

    /// Performs checked mul operation.
    pub fn checked_mul(&self, v: &Gas) -> Option<Gas> {
        self.0.checked_mul(v.0).map(Gas)
    }

    /// Performs checked div operation.
    pub fn checked_div(&self, v: &Gas) -> Option<Gas> {
        self.0.checked_div(v.0).map(Gas)
    }

    /// Performs checked modulo operation.
    pub fn checked_rem(&self, v: &Gas) -> Option<Gas> {
        self.0.checked_rem(v.0).map(Gas)
    }
}

impl fmt::Display for Gas {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl From<u32> for Gas {
    fn from(gas: u32) -> Self {
        Gas::new(gas as u64)
    }
}

impl From<u64> for Gas {
    fn from(gas: u64) -> Self {
        Gas::new(gas)
    }
}

impl From<usize> for Gas {
    fn from(gas_value: usize) -> Self {
        Gas(gas_value as u64)
    }
}

impl From<Gas> for U512 {
    fn from(gas: Gas) -> Self {
        U512::from(gas.0)
    }
}

impl From<Gas> for u64 {
    fn from(gas: Gas) -> Self {
        gas.0
    }
}

#[cfg(test)]
mod tests {
    use crate::{Gas, Motes};

    use super::*;

    #[test]
    fn u64_max_as_u512_is_correct() {
        assert_eq!(GAS_MAX_AS_U512, U512::from(u64::MAX));
    }

    #[test]
    fn should_be_able_to_get_instance_of_gas() {
        let initial_value = 1;
        let gas = Gas::new(initial_value);
        assert_eq!(gas, Gas::from(1u64));
    }

    #[test]
    fn should_be_able_to_compare_two_instances_of_gas() {
        let left_gas = Gas::new(1);
        let right_gas = Gas::new(1);
        assert_eq!(left_gas, right_gas, "should be equal");
        let right_gas = Gas::new(2);
        assert_ne!(left_gas, right_gas, "should not be equal")
    }

    #[test]
    fn should_be_able_to_add_two_instances_of_gas() {
        let left_gas = Gas::new(1);
        let right_gas = Gas::new(1);
        let expected_gas = Gas::new(2);
        assert_eq!(
            left_gas.checked_add(&right_gas),
            Some(expected_gas),
            "should be equal"
        )
    }

    #[test]
    fn should_be_able_to_subtract_two_instances_of_gas() {
        let left_gas = Gas::new(1);
        let right_gas = Gas::new(1);
        let expected_gas = Gas::new(0);
        assert_eq!(
            left_gas.checked_sub(&right_gas),
            Some(expected_gas),
            "should be equal"
        )
    }

    #[test]
    fn should_be_able_to_multiply_two_instances_of_gas() {
        let left_gas = Gas::new(100);
        let right_gas = Gas::new(10);
        let expected_gas = Gas::new(1000);
        assert_eq!(
            left_gas.checked_mul(&right_gas),
            Some(expected_gas),
            "should be equal"
        )
    }

    #[test]
    fn should_be_able_to_divide_two_instances_of_gas() {
        let left_gas = Gas::new(1000);
        let right_gas = Gas::new(100);
        let expected_gas = Gas::new(10);
        assert_eq!(
            left_gas.checked_div(&right_gas),
            Some(expected_gas),
            "should be equal"
        )
    }

    #[test]
    fn should_be_able_to_convert_from_mote() {
        let mote = Motes::new(U512::from(100));
        let gas = Gas::from_motes(mote, 10).expect("should have gas");
        let expected_gas = Gas::new(10);
        assert_eq!(gas, expected_gas, "should be equal")
    }

    #[test]
    fn should_be_able_to_default() {
        let gas = Gas::default();
        let expected_gas = Gas::new(0);
        assert_eq!(gas, expected_gas, "should be equal")
    }

    #[test]
    fn should_be_able_to_compare_relative_value() {
        let left_gas = Gas::new(100);
        let right_gas = Gas::new(10);
        assert!(left_gas > right_gas, "should be gt");
        let right_gas = Gas::new(100);
        assert!(left_gas >= right_gas, "should be gte");
        assert!(left_gas <= right_gas, "should be lte");
        let left_gas = Gas::new(10);
        assert!(left_gas < right_gas, "should be lt");
    }

    #[test]
    fn should_default() {
        let left_gas = Gas::new(0);
        let right_gas = Gas::default();
        assert_eq!(left_gas, right_gas, "should be equal");
        assert_eq!(left_gas, Gas::zero(), "should be equal");
    }

    #[test]
    fn should_support_checked_div_from_motes() {
        let motes = Motes::new(U512::zero());
        let conv_rate = 0;
        let maybe = Gas::from_motes(motes, conv_rate);
        assert!(maybe.is_none(), "should be none due to divide by zero");
    }
}