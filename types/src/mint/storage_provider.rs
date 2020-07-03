use crate::{
    bytesrepr::{FromBytes, ToBytes},
    system_contract_errors::mint::Error,
    CLTyped, URef,
};

/// Provides functionality of a contract storage.
pub trait StorageProvider {
    /// Create new [`URef`].
    fn new_uref<T: CLTyped + ToBytes>(&mut self, init: T) -> URef;

    /// Write data to a local key.
    fn write_local<K: ToBytes, V: CLTyped + ToBytes>(&mut self, key: K, value: V);

    /// Read data from a local key.
    fn read_local<K: ToBytes, V: CLTyped + FromBytes>(
        &mut self,
        key: &K,
    ) -> Result<Option<V>, Error>;

    /// Read data from [`URef`].
    fn read<T: CLTyped + FromBytes>(&mut self, uref: URef) -> Result<Option<T>, Error>;

    /// Write data under a [`URef`].
    fn write<T: CLTyped + ToBytes>(&mut self, uref: URef, value: T) -> Result<(), Error>;

    /// Add data to a [`URef`].
    fn add<T: CLTyped + ToBytes>(&mut self, uref: URef, value: T) -> Result<(), Error>;
}
