use casper_types::{
    account::AccountHash, system::handle_payment::Error, TransferredTo, URef, U512,
};

use crate::shared::wasm_engine::FunctionContext;

/// Provides an access to mint.
pub trait MintProvider {
    /// Transfer `amount` from `source` purse to a `target` account.
    fn transfer_purse_to_account(
        &mut self,
        context: &mut impl FunctionContext,
        source: URef,
        target: AccountHash,
        amount: U512,
    ) -> Result<TransferredTo, Error>;

    /// Transfer `amount` from `source` purse to a `target` purse.
    fn transfer_purse_to_purse(
        &mut self,
        context: &mut impl FunctionContext,
        source: URef,
        target: URef,
        amount: U512,
    ) -> Result<(), Error>;

    /// Checks balance of a `purse`. Returns `None` if given purse does not exist.
    fn balance(&mut self, purse: URef) -> Result<Option<U512>, Error>;
}
