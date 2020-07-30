//! Contains implementation of a Auction contract functionality.
mod active_bid;
mod auction;
mod delegator;
mod era_validators;
mod founding_validator;
mod internal;
mod providers;
mod seigniorage_recipient;
mod types;

pub use active_bid::{ActiveBid, ActiveBids};
pub use auction::{
    Auction, ACTIVE_BIDS_KEY, DELEGATORS_KEY, ERA_VALIDATORS_KEY, FOUNDER_VALIDATORS_KEY,
};
pub use delegator::Delegators;
pub use era_validators::EraValidators;
pub use founding_validator::{FoundingValidator, FoundingValidators};
pub use providers::{ProofOfStakeProvider, StorageProvider, SystemProvider};
pub use seigniorage_recipient::{SeigniorageRecipient, SeigniorageRecipients};
pub use types::DelegationRate;
