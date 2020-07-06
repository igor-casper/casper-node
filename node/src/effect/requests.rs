use std::{
    collections::HashSet,
    fmt::{self, Debug, Display, Formatter},
};

use super::Responder;
use crate::{
    components::storage::{self, StorageType, Value},
    types::{Deploy, DeployHash},
};

#[derive(Debug)]
pub enum NetworkRequest<I, P> {
    /// Send a message on the network to a specific peer.
    SendMessage {
        dest: I,
        payload: P,
        responder: Responder<()>,
    },
    /// Send a message on the network to all peers.
    // Note: This request is deprecated and should be phased out, as not every network
    // implementation is likely to implement broadcast support.
    Broadcast {
        payload: P,
        responder: Responder<()>,
    },
    Gossip {
        payload: P,
        count: usize,
        exclude: HashSet<I>,
        responder: Responder<HashSet<I>>,
    },
}

impl<I, P> NetworkRequest<I, P> {
    /// Transform a network request by mapping the contained payload.
    ///
    /// This is a replacement for a `From` conversion that is not possible without specialization.
    pub(crate) fn map_payload<F, P2>(self, wrap_payload: F) -> NetworkRequest<I, P2>
    where
        F: FnOnce(P) -> P2,
    {
        match self {
            NetworkRequest::SendMessage {
                dest,
                payload,
                responder,
            } => NetworkRequest::SendMessage {
                dest,
                payload: wrap_payload(payload),
                responder,
            },
            NetworkRequest::Broadcast { payload, responder } => NetworkRequest::Broadcast {
                payload: wrap_payload(payload),
                responder,
            },
            NetworkRequest::Gossip {
                payload,
                count,
                exclude,
                responder,
            } => NetworkRequest::Gossip {
                payload: wrap_payload(payload),
                count,
                exclude,
                responder,
            },
        }
    }
}

impl<I, P> Display for NetworkRequest<I, P>
where
    I: Display,
    P: Display,
{
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            NetworkRequest::SendMessage { dest, payload, .. } => {
                write!(formatter, "send to {}: {}", dest, payload)
            }
            NetworkRequest::Broadcast { payload, .. } => {
                write!(formatter, "broadcast: {}", payload)
            }
            NetworkRequest::Gossip { payload, .. } => write!(formatter, "gossip: {}", payload),
        }
    }
}

#[derive(Debug)]
#[allow(clippy::type_complexity)]
// TODO: remove once all variants are used.
#[allow(dead_code)]
pub enum StorageRequest<S: StorageType + 'static> {
    /// Store given block.
    PutBlock {
        block: Box<S::Block>,
        responder: Responder<storage::Result<()>>,
    },
    /// Retrieve block with given hash.
    GetBlock {
        block_hash: <S::Block as Value>::Id,
        responder: Responder<storage::Result<S::Block>>,
    },
    /// Retrieve block header with given hash.
    GetBlockHeader {
        block_hash: <S::Block as Value>::Id,
        responder: Responder<storage::Result<<S::Block as Value>::Header>>,
    },
    /// Store given deploy.
    PutDeploy {
        deploy: Box<S::Deploy>,
        responder: Responder<storage::Result<()>>,
    },
    /// Retrieve deploy with given hash.
    GetDeploy {
        deploy_hash: <S::Deploy as Value>::Id,
        responder: Responder<storage::Result<S::Deploy>>,
    },
    /// Retrieve deploy header with given hash.
    GetDeployHeader {
        deploy_hash: <S::Deploy as Value>::Id,
        responder: Responder<storage::Result<<S::Deploy as Value>::Header>>,
    },
    /// List all deploy hashes.
    ListDeploys {
        responder: Responder<storage::Result<Vec<<S::Deploy as Value>::Id>>>,
    },
}

impl<S: StorageType> Display for StorageRequest<S> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            StorageRequest::PutBlock { block, .. } => write!(formatter, "put {}", block),
            StorageRequest::GetBlock { block_hash, .. } => write!(formatter, "get {}", block_hash),
            StorageRequest::GetBlockHeader { block_hash, .. } => {
                write!(formatter, "get {}", block_hash)
            }
            StorageRequest::PutDeploy { deploy, .. } => write!(formatter, "put {}", deploy),
            StorageRequest::GetDeploy { deploy_hash, .. } => {
                write!(formatter, "get {}", deploy_hash)
            }
            StorageRequest::GetDeployHeader { deploy_hash, .. } => {
                write!(formatter, "get {}", deploy_hash)
            }
            StorageRequest::ListDeploys { .. } => write!(formatter, "list deploys"),
        }
    }
}

/// Abstract API request
///
/// An API request is an abstract request that does not concern itself with serialization or
/// transport.
#[derive(Debug)]
pub enum ApiRequest {
    /// Submit a deploy for storing.
    ///
    /// Returns the deploy along with an error message if it could not be stored.
    SubmitDeploy {
        deploy: Box<Deploy>,
        responder: Responder<Result<(), (Deploy, storage::Error)>>,
    },
    /// Return the specified deploy if it exists, else `None`.
    GetDeploy {
        hash: DeployHash,
        responder: Responder<Result<Deploy, storage::Error>>,
    },
    /// Return the list of all deploy hashes stored on this node.
    ListDeploys {
        responder: Responder<Result<Vec<DeployHash>, storage::Error>>,
    },
}

impl Display for ApiRequest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ApiRequest::SubmitDeploy { deploy, .. } => write!(formatter, "submit {}", *deploy),
            ApiRequest::GetDeploy { hash, .. } => write!(formatter, "get {}", hash),
            ApiRequest::ListDeploys { .. } => write!(formatter, "list deploys"),
        }
    }
}

#[derive(Debug)]
pub enum DeployGossiperRequest {
    /// A new `Deploy` received from a client via the HTTP server component.
    PutFromClient { deploy: Box<Deploy> },
}

impl Display for DeployGossiperRequest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            DeployGossiperRequest::PutFromClient { deploy, .. } => {
                write!(formatter, "put from client: {}", deploy.id())
            }
        }
    }
}
