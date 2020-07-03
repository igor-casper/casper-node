use std::{io, result, time::SystemTimeError};

use openssl::error::ErrorStack;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_openssl::HandshakeError;

use crate::tls::ValidationError;

pub(super) type Result<T> = result::Result<T, Error>;

/// Error type returned by the `SmallNetwork` component.
#[derive(Debug, Error)]
pub enum Error {
    /// Server failed to present certificate.
    #[error("no server certificate presented")]
    NoServerCertificate,
    /// Client failed to present certificate.
    #[error("no client certificate presented")]
    NoClientCertificate,
    /// Peer ID presented does not match the expected one.
    #[error("remote node has wrong ID")]
    WrongId,
    /// The config must have both or neither of certificate and private key.
    #[error("need either both or none of cert, private_key in network config")]
    InvalidConfig,
    /// Our own certificate is not valid.
    #[error("own certificate invalid")]
    OwnCertificateInvalid(#[source] ValidationError),
    /// Failed to create a TCP listener.
    #[error("failed to create listener")]
    ListenerCreation(#[source] io::Error),
    /// Failed to get TCP listener address.
    #[error("failed to get listener addr")]
    ListenerAddr(#[source] io::Error),
    /// Failed to convert std TCP listener to tokio TCP listener.
    #[error("failed to convert listener to tokio")]
    ListenerConversion(#[source] io::Error),
    /// Failed to send message.
    #[error("failed to send message")]
    MessageNotSent(#[source] io::Error),
    /// Failed to create TLS acceptor.
    #[error("failed to create acceptor")]
    AcceptorCreation(#[source] ErrorStack),
    /// Failed to create configuration for TLS connector.
    #[error("failed to configure connector")]
    ConnectorConfiguration(#[source] ErrorStack),
    /// Failed to generate node TLS certificate.
    #[error("failed to generate cert")]
    CertificateGeneration(#[source] ErrorStack),
    /// Handshaking error.
    #[error("handshake error: {0}")]
    Handshake(#[from] HandshakeError<TcpStream>),
    /// TLS validation error.
    #[error("TLS validation error: {0}")]
    TlsValidation(#[from] ValidationError),
    /// System time error.
    #[error("system time error: {0}")]
    SystemTime(#[from] SystemTimeError),
    /// Other error.
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}
