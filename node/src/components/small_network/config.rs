use std::net::{Ipv4Addr, SocketAddrV4};

use openssl::{
    pkey::{PKey, Private},
    x509::X509,
};
use serde::{Deserialize, Serialize};

use crate::{utils::External, ROOT_VALIDATOR_LISTENING_PORT};

/// Small network configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
// Disallow unknown fields to ensure config files and command-line overrides contain valid keys.
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Interface to bind to.
    ///
    /// If it is the same as that in `root_addr` and the `bind_port` is non-zero and the same as
    /// that in `root_addr`, attempt to become the root node for this particular small network.
    pub bind_interface: String,

    /// Port to bind to when not the root node. Use 0 for a random port.
    pub bind_port: u16,

    /// Address to connect to join the network.
    pub root_addr: String,

    /// Path to certificate file.
    pub cert_path: Option<External<X509>>,

    /// Path to secret key for certificate.
    pub secret_key_path: Option<External<PKey<Private>>>,

    /// Maximum number of retries before removing an outgoing node. Unlimited if `None`.
    pub max_outgoing_retries: Option<u32>,

    /// Number of milliseconds to delay between each reconnection attempt.
    pub outgoing_retry_delay_millis: u64,
}

impl Config {
    /// Creates a default instance for `SmallNetwork` with a constant port.
    pub fn default_on_port(port: u16) -> Self {
        Config {
            bind_interface: Ipv4Addr::LOCALHOST.to_string(),
            bind_port: 0,
            root_addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).to_string(),
            cert_path: None,
            secret_key_path: None,
            max_outgoing_retries: Some(360),
            outgoing_retry_delay_millis: 10_000,
        }
    }
}

#[derive(Debug)]
pub struct RetrySettings {
    pub max_outgoing: Option<u32>,
    pub outgoing_delay_millis: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self::default_on_port(ROOT_VALIDATOR_LISTENING_PORT)
    }
}
