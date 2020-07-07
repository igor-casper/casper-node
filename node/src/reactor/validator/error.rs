use thiserror::Error;

use crate::components::{small_network, storage};

/// Error type returned by the validator reactor.
#[derive(Debug, Error)]
pub enum Error {
    /// `SmallNetwork` component error.
    #[error("small network error: {0}")]
    SmallNetwork(#[from] small_network::Error),
    /// `Storage` component error.
    #[error("storage error: {0}")]
    Storage(#[from] storage::Error),
}
