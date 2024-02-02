pub mod error;
pub mod security_badge;

use borsh::{BorshDeserialize, BorshSerialize};
use casper_macros::{casper, selector, CasperABI, CasperSchema, Contract};
use casper_sdk::{
    collections::{Map, Set},
    host, log, revert,
    schema::CasperSchema,
    types::Address,
    Contract, UnwrapOrRevert,
};
use error::Cep18Error;
use security_badge::SecurityBadge;
use std::string::String;

#[derive(Contract, CasperSchema, BorshSerialize, BorshDeserialize, CasperABI, Debug)]
pub struct CEP18 {
    name: String,
    symbol: String,
    decimals: u8,
    total_supply: u64, // TODO: U256
    balances: Map<Address, u64>,
    allowances: Map<(Address, Address), u64>,
    security_badges: Map<Address, SecurityBadge>,
    enable_mint_burn: bool,
}

impl Default for CEP18 {
    fn default() -> Self {
        Self {
            name: "Default name".to_string(),
            symbol: "Default symbol".to_string(),
            decimals: 0,
            total_supply: 0,
            balances: Map::new("balances"),
            allowances: Map::new("allowances"),
            security_badges: Map::new("security_badges"),
            enable_mint_burn: false,
        }
    }
}

impl CEP18 {
    fn sec_check(&self, allowed_badge_list: &[SecurityBadge]) -> Result<(), Cep18Error> {
        let caller = host::get_caller();
        let security_badge = self
            .security_badges
            .get(&caller)
            .ok_or(Cep18Error::InsufficientRights)?;
        if !allowed_badge_list.contains(&security_badge) {
            return Err(Cep18Error::InsufficientRights);
        }
        Ok(())
    }

    fn transfer_balance(
        &mut self,
        sender: &Address,
        recipient: &Address,
        amount: u64,
    ) -> Result<(), Cep18Error> {
        if amount == 0 {
            return Ok(());
        }

        let sender_balance = self.balances.get(&sender).unwrap_or_default();

        let new_sender_balance = sender_balance
            .checked_sub(amount)
            .ok_or(Cep18Error::InsufficientBalance)?;

        let recipient_balance = self.balances.get(&recipient).unwrap_or_default();

        let new_recipient_balance = recipient_balance
            .checked_add(amount)
            .ok_or(Cep18Error::Overflow)?;

        self.balances.insert(sender, &new_sender_balance);
        self.balances.insert(recipient, &new_recipient_balance);
        Ok(())
    }
}

#[casper(entry_points)]
impl CEP18 {
    #[casper(constructor)]
    pub fn new(token_name: String) -> Self {
        // TODO: If argument has same name as another entrypoint there's a compile error for some
        // reason, so can't use "name"
        let mut instance = Self::default();
        instance.name = token_name;
        instance.enable_mint_burn = true;
        instance
            .security_badges
            .insert(&host::get_caller(), &SecurityBadge::Admin);
        instance
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    pub fn decimals(&self) -> u8 {
        self.decimals
    }

    pub fn total_supply(&self) -> u64 {
        self.total_supply
    }

    pub fn balance_of(&self, address: Address) -> u64 {
        self.balances.get(&address).unwrap_or_default()
    }

    pub fn allowance(&self, spender: Address, owner: Address) {
        self.allowances.get(&(spender, owner)).unwrap_or_default();
    }

    #[casper(revert_on_error)]
    pub fn approve(&mut self, spender: Address, amount: u64) -> Result<(), Cep18Error> {
        let owner = host::get_caller();
        if owner == spender {
            return Err(Cep18Error::CannotTargetSelfUser);
        }
        let lookup_key = (owner, spender);
        self.allowances.insert(&lookup_key, &amount);
        Ok(())
    }

    #[casper(revert_on_error)]
    pub fn decrease_allowance(&mut self, spender: Address, amount: u64) -> Result<(), Cep18Error> {
        let owner = host::get_caller();
        if owner == spender {
            return Err(Cep18Error::CannotTargetSelfUser);
        }
        let lookup_key = (owner, spender);
        let allowance = self.allowances.get(&lookup_key).unwrap_or_default();
        let allowance = allowance.saturating_sub(amount);
        self.allowances.insert(&lookup_key, &allowance);
        Ok(())
    }

    #[casper(revert_on_error)]
    pub fn increase_allowance(&mut self, spender: Address, amount: u64) -> Result<(), Cep18Error> {
        let owner = host::get_caller();
        if owner == spender {
            return Err(Cep18Error::CannotTargetSelfUser);
        }
        let lookup_key = (owner, spender);
        let allowance = self.allowances.get(&lookup_key).unwrap_or_default();
        let allowance = allowance.saturating_add(amount);
        self.allowances.insert(&lookup_key, &allowance);
        Ok(())
    }

    #[casper(revert_on_error)]
    pub fn transfer(&mut self, recipient: Address, amount: u64) -> Result<(), Cep18Error> {
        let sender = host::get_caller();
        if sender == recipient {
            return Err(Cep18Error::CannotTargetSelfUser);
        }
        self.transfer_balance(&sender, &recipient, amount)?;
        Ok(())
    }

    #[casper(revert_on_error)]
    pub fn transfer_from(
        &mut self,
        owner: Address,
        recipient: Address,
        amount: u64,
    ) -> Result<(), Cep18Error> {
        let spender = host::get_caller();
        if owner == recipient {
            return Err(Cep18Error::CannotTargetSelfUser);
        }

        if amount == 0 {
            return Ok(());
        }

        let spender_allowance = self.allowances.get(&(owner, spender)).unwrap_or_default();
        let new_spender_allowance = spender_allowance
            .checked_sub(amount)
            .ok_or(Cep18Error::InsufficientAllowance)?;

        self.transfer_balance(&owner, &recipient, amount)?;

        self.allowances
            .insert(&(owner, spender), &new_spender_allowance);

        Ok(())
    }

    #[casper(revert_on_error)]
    pub fn mint(&mut self, owner: Address, amount: u64) -> Result<(), Cep18Error> {
        if !self.enable_mint_burn {
            return Err(Cep18Error::MintBurnDisabled);
        }

        self.sec_check(&[SecurityBadge::Admin, SecurityBadge::Minter])?;

        let balance = self.balances.get(&owner).unwrap_or_default();
        let new_balance = balance.checked_add(amount).ok_or(Cep18Error::Overflow)?;
        self.balances.insert(&owner, &new_balance);
        self.total_supply = self
            .total_supply
            .checked_add(amount)
            .ok_or(Cep18Error::Overflow)?;
        Ok(())
    }

    #[casper(revert_on_error)]
    pub fn burn(&mut self, owner: Address, amount: u64) -> Result<(), Cep18Error> {
        if !self.enable_mint_burn {
            return Err(Cep18Error::MintBurnDisabled);
        }

        if owner != host::get_caller() {
            return Err(Cep18Error::InvalidBurnTarget);
        }

        let balance = self.balances.get(&owner).unwrap_or_default();
        let new_balance = balance.checked_add(amount).ok_or(Cep18Error::Overflow)?;
        self.balances.insert(&owner, &new_balance);
        self.total_supply = self
            .total_supply
            .checked_sub(amount)
            .ok_or(Cep18Error::Overflow)?;
        Ok(())
    }
}

#[casper(export)]
pub fn call() {
    let result = CEP18::create(selector!("new"), None).unwrap();
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    use casper_sdk::{abi::CasperABI, host::native::Stub};

    const DEFAULT_ACCOUNT: Address = [42; 32];
    const ALICE: Address = [1; 32];
    const BOB: Address = [2; 32];

    #[test]
    fn should_generate_abi() {
        dbg!(CEP18::definition());
        // add verification of ABI
    }

    #[test]
    fn schema() {
        let s = serde_json::to_string_pretty(&CEP18::schema()).unwrap();
        fs::write("/tmp/cep18_schema.json", &s).unwrap();
    }

    #[test]
    fn it_works() {
        let stub = Stub::new(Default::default(), [42; 32]);

        let result = host::native::dispatch_with(stub, || {
            let mut contract = CEP18::new("Foo Token".to_string());

            contract.sec_check(&[SecurityBadge::Admin]).unwrap();

            assert_eq!(contract.name(), "Foo Token");
            assert_eq!(contract.balance_of(ALICE), 0);
            assert_eq!(contract.balance_of(BOB), 0);

            contract.approve(BOB, 111).unwrap();
            assert_eq!(contract.balance_of(ALICE), 0);
            contract.mint(ALICE, 1000).unwrap();
            assert_eq!(contract.balance_of(ALICE), 1000);

            // [42; 32] -> ALICE - not much balance
            assert_eq!(contract.balance_of(host::get_caller()), 0);
            assert_eq!(
                contract.transfer(ALICE, 1),
                Err(Cep18Error::InsufficientBalance)
            );
        });
        assert_eq!(result, Ok(()));
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    panic!("This executable should not be used in a non-WASM environment.");
}