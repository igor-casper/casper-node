//! Fully connected overlay network
//!
//! The *small network* is an overlay network where each node participating is connected to every
//! other node on the network. The *small* portion of the name stems from the fact that this
//! approach is not scalable, as it requires at least $O(n)$ network connections and broadcast will
//! result in $O(n^2)$ messages.
//!
//! # Node IDs
//!
//! Each node has a self-generated node ID based on its self-signed TLS certificate. Whenever a
//! connection is made to another node, it verifies the "server"'s certificate to check that it
//! connected to the correct node and sends its own certificate during the TLS handshake,
//! establishing identity.
//!
//! # Messages and payloads
//!
//! The network itself is best-effort, during regular operation, no messages should be lost. A node
//! will attempt to reconnect when it loses a connection, however messages and broadcasts may be
//! lost during that time.
//!
//! # Connection
//!
//! Every node has an ID and a listening address. The objective of each node is to constantly
//! maintain an outgoing connection to each other node (and thus have an incoming connection from
//! these nodes as well).
//!
//! Any incoming connection is strictly read from, while any outgoing connection is strictly used
//! for sending messages.
//!
//! Nodes track the signed (timestamp, listening address, certificate) tuples called "endpoints"
//! internally and whenever they connecting to a new node, they share this state with the other
//! node, as well as notifying them about any updates they receive.
//!
//! # Joining the network
//!
//! When a node connects to any other network node, it sends its current list of endpoints down the
//! new outgoing connection. This will cause the receiving node to initiate a connection attempt to
//! all nodes in the list and simultaneously tell all of its connected nodes about the new node,
//! repeating the process.

// TODO: remove clippy relaxation
#![allow(clippy::type_complexity)]

mod config;
mod endpoint;
mod error;
mod event;
mod message;
#[cfg(test)]
mod test;

use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Debug, Display, Formatter},
    io,
    net::{SocketAddr, TcpListener},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use futures::{
    future::{select, Either},
    stream::{SplitSink, SplitStream},
    FutureExt, SinkExt, StreamExt,
};
use maplit::hashmap;
use openssl::pkey;
use pkey::{PKey, Private};
use rand::{seq::IteratorRandom, Rng};
use serde::{de::DeserializeOwned, Serialize};
use tokio::{
    net::TcpStream,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    task::JoinHandle,
};
use tokio_openssl::SslStream;
use tokio_serde::{formats::SymmetricalMessagePack, SymmetricallyFramed};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::{debug, error, info, warn, Span};

pub(crate) use self::{endpoint::Endpoint, event::Event, message::Message};
use self::{endpoint::EndpointUpdate, error::Result};
use crate::{
    components::Component,
    effect::{
        announcements::NetworkAnnouncement, requests::NetworkRequest, Effect, EffectBuilder,
        EffectExt, EffectResultExt, Multiple,
    },
    reactor::{EventQueueHandle, QueueKind},
    tls::{self, KeyFingerprint, Signed, TlsCert},
};
// Seems to be a false positive.
#[allow(unreachable_pub)]
pub use config::Config;
// Seems to be a false positive.
#[allow(unreachable_pub)]
pub use error::Error;

/// A node ID.
///
/// The key fingerprint found on TLS certificates.
pub(crate) type NodeId = KeyFingerprint;

pub(crate) struct SmallNetwork<REv: 'static, P> {
    /// Configuration.
    cfg: Config,
    /// Server certificate.
    cert: Arc<TlsCert>,
    /// Server private key.
    private_key: Arc<PKey<Private>>,
    /// Handle to event queue.
    event_queue: EventQueueHandle<REv>,
    /// A list of known endpoints by node ID.
    endpoints: HashMap<NodeId, Endpoint>,
    /// Stored signed endpoints that can be sent to other nodes.
    signed_endpoints: HashMap<NodeId, Signed<Endpoint>>,
    /// Outgoing network connections' messages.
    outgoing: HashMap<NodeId, UnboundedSender<Message<P>>>,
    /// Channel signaling a shutdown of the small network.
    // Note: This channel never sends anything, instead it is closed when `SmallNetwork` is dropped,
    //       signalling the receiver that it should cease operation. Don't listen to clippy!
    #[allow(dead_code)]
    shutdown: Option<oneshot::Sender<()>>,
    /// Join handle for the server thread.
    #[allow(dead_code)]
    server_join_handle: Option<JoinHandle<()>>,
}

impl<REv, P> SmallNetwork<REv, P>
where
    P: Serialize + DeserializeOwned + Clone + Debug + Send + 'static,
    REv: Send + From<Event<P>>,
{
    pub(crate) fn new(
        event_queue: EventQueueHandle<REv>,
        cfg: Config,
    ) -> Result<(SmallNetwork<REv, P>, Multiple<Effect<Event<P>>>)> {
        let span = tracing::debug_span!("net");
        let _enter = span.enter();

        let server_span = tracing::info_span!("server");

        // First, we load or generate the TLS keys.
        let (cert, private_key) = match (&cfg.cert, &cfg.private_key) {
            // We're given a cert_file and a private_key file. Just load them, additional checking
            // will be performed once we create the acceptor and connector.
            (Some(cert_file), Some(private_key_file)) => (
                tls::load_cert(cert_file).context("could not load TLS certificate")?,
                tls::load_private_key(private_key_file)
                    .context("could not load TLS private key")?,
            ),

            // Neither was passed, so we auto-generate a pair.
            (None, None) => tls::generate_node_cert().map_err(Error::CertificateGeneration)?,

            // If we get only one of the two, return an error.
            _ => return Err(Error::InvalidConfig),
        };

        // We can now create a listener.
        let (listener, we_are_root) = create_listener(&cfg).map_err(Error::ListenerCreation)?;
        let addr = listener.local_addr().map_err(Error::ListenerAddr)?;

        // Create the model. Initially we know our own endpoint address.
        let our_endpoint = Endpoint::new(
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64,
            addr,
            tls::validate_cert(cert.clone())?,
        );
        let our_fingerprint = our_endpoint.cert().public_key_fingerprint();

        // Run the server task.
        // We spawn it ourselves instead of through an effect to get a hold of the join handle,
        // which we need to shutdown cleanly later on.
        info!(%our_endpoint, "starting server background task");
        let (server_shutdown_sender, server_shutdown_receiver) = oneshot::channel();
        let server_join_handle = tokio::spawn(server_task(
            event_queue,
            tokio::net::TcpListener::from_std(listener).map_err(Error::ListenerConversion)?,
            server_shutdown_receiver,
            server_span,
        ));

        let model = SmallNetwork {
            cfg,
            signed_endpoints: hashmap! { our_fingerprint => Signed::new(&our_endpoint, &private_key)? },
            endpoints: hashmap! { our_fingerprint => our_endpoint },
            cert: Arc::new(tls::validate_cert(cert).map_err(Error::OwnCertificateInvalid)?),
            private_key: Arc::new(private_key),
            event_queue,
            outgoing: HashMap::new(),
            shutdown: Some(server_shutdown_sender),
            server_join_handle: Some(server_join_handle),
        };

        // Connect to the root node if we are not the root node.
        let mut effects: Multiple<_> = Default::default();
        if !we_are_root {
            effects.extend(model.connect_to_root());
        } else {
            debug!("will not connect to root node, since we are the root");
        }

        Ok((model, effects))
    }

    /// Close down the listening server socket.
    ///
    /// Signals that the background process that runs the server should shutdown completely and
    /// waits for it to complete the shutdown. This explicitly allows the background task to finish
    /// and drop everything it owns, ensuring that resources such as allocated ports are free to be
    /// reused once this completes.
    #[cfg(test)]
    async fn shutdown_server(&mut self) {
        // Close the shutdown socket, causing the server to exit.
        drop(self.shutdown.take());

        // Wait for the server to exit cleanly.
        if let Some(join_handle) = self.server_join_handle.take() {
            match join_handle.await {
                Ok(_) => debug!("server exited cleanly"),
                Err(err) => error!(%err, "could not join server task cleanly"),
            }
        } else {
            warn!("server shutdown while already shut down")
        }
    }

    /// Attempts to connect to the root node.
    fn connect_to_root(&self) -> Multiple<Effect<Event<P>>> {
        connect_trusted(
            self.cfg.root_addr,
            self.cert.clone(),
            self.private_key.clone(),
        )
        .result(
            move |(cert, transport)| Event::RootConnected { cert, transport },
            move |error| Event::RootFailed { error },
        )
    }

    /// Queues a message to be sent to all nodes.
    fn broadcast_message(&self, msg: Message<P>) {
        for node_id in self.outgoing.keys() {
            self.send_message(*node_id, msg.clone());
        }
    }

    /// Queues a message to `count` random nodes on the network.
    fn gossip_message<R: Rng + ?Sized>(
        &self,
        rng: &mut R,
        msg: Message<P>,
        count: usize,
        exclude: HashSet<NodeId>,
    ) {
        let node_ids = self
            .outgoing
            .keys()
            .filter(|&node_id| !exclude.contains(node_id))
            .choose_multiple(rng, count);

        if node_ids.len() != count {
            warn!(
                wanted = count,
                selected = node_ids.len(),
                "could not select enough random nodes for gossiping, not enough non-excluded \
                outgoing connections"
            );
        }

        for node_id in node_ids {
            self.send_message(*node_id, msg.clone());
        }
    }

    /// Queues a message to be sent to a specific node.
    fn send_message(&self, dest: NodeId, msg: Message<P>) {
        // Try to send the message.
        if let Some(sender) = self.outgoing.get(&dest) {
            if let Err(msg) = sender.send(msg) {
                // We lost the connection, but that fact has not reached us yet.
                warn!(%dest, ?msg, "dropped outgoing message, lost connection");
            }
        } else {
            // We are not connected, so the reconnection is likely already in progress.
            warn!(%dest, ?msg, "dropped outgoing message, no connection");
        }
    }

    /// Updates the internal endpoint store with a given endpoint.
    ///
    /// Will update both, the store for signed endpoints and unpacked ones and indicate what kind
    /// of update happened.
    #[inline]
    fn update_endpoint(&mut self, signed: Signed<Endpoint>) -> EndpointUpdate {
        match signed.validate_self_signed(|endpoint| Ok(endpoint.cert().public_key())) {
            Ok(endpoint) => {
                let fingerprint = endpoint.cert().public_key_fingerprint();

                match self.endpoints.get(&fingerprint) {
                    None => {
                        // Endpoint was not known at all.
                        self.endpoints.insert(fingerprint, endpoint.clone());
                        self.signed_endpoints.insert(fingerprint, signed);

                        EndpointUpdate::New { cur: endpoint }
                    }
                    Some(prev_ep) if prev_ep >= &endpoint => {
                        // The stored timestamp is newer or equal, we ignore the stored value. This
                        // branch is also taken if we hit a duplicate endpoint that is logically
                        // less than ours, which is a rare edge-case or an attack.
                        EndpointUpdate::Unchanged
                    }
                    Some(prev_ep) if prev_ep.dest() == endpoint.dest() => {
                        // The new endpoint has newer timestamp, but points to same destination.
                        self.signed_endpoints.insert(fingerprint, signed);
                        let prev = self
                            .endpoints
                            .insert(fingerprint, endpoint.clone())
                            .unwrap();
                        EndpointUpdate::Refreshed {
                            cur: endpoint,
                            prev,
                        }
                    }
                    Some(_) => {
                        // Newer timestamp, different endpoint.
                        self.signed_endpoints.insert(fingerprint, signed);
                        let prev = self
                            .endpoints
                            .insert(fingerprint, endpoint.clone())
                            .unwrap();
                        EndpointUpdate::Updated {
                            cur: endpoint,
                            prev,
                        }
                    }
                }
            }
            Err(err) => EndpointUpdate::InvalidSignature {
                signed,
                err: err.into(),
            },
        }
    }

    /// Updates internal endpoint store and if new, output a `BroadcastEndpoint` effect.
    #[inline]
    fn update_and_broadcast_if_new(
        &mut self,
        signed: Signed<Endpoint>,
    ) -> Multiple<Effect<Event<P>>> {
        let change = self.update_endpoint(signed);
        debug!(%change, "endpoint change");

        match change {
            EndpointUpdate::New { cur } | EndpointUpdate::Updated { cur, .. } => {
                let node_id = cur.node_id();

                // New/updated endpoint, now establish or replace the outgoing connection.
                let effect = match self.outgoing.remove(&node_id) {
                    None => {
                        info!(%node_id, endpoint=%cur, "new outgoing channel");

                        connect_outgoing(cur, self.cert.clone(), self.private_key.clone()).result(
                            move |transport| Event::OutgoingEstablished { node_id, transport },
                            move |error| Event::OutgoingFailed {
                                node_id,
                                attempt_count: 0,
                                error: Some(error),
                            },
                        )
                    }
                    Some(_sender) => {
                        // There was a previous endpoint, whose sender has now been dropped. This
                        // will cause the sender task to exit and trigger a reconnect, so no action
                        // must be taken at this point.

                        Multiple::new()
                    }
                };

                // Let others know what we've learned.
                self.broadcast_message(Message::BroadcastEndpoint(
                    self.signed_endpoints[&node_id].clone(),
                ));

                effect
            }
            EndpointUpdate::Refreshed { cur, .. } => {
                let node_id = cur.node_id();

                // On a refresh we propagate the newer signature.
                self.broadcast_message(Message::BroadcastEndpoint(
                    self.signed_endpoints[&node_id].clone(),
                ));

                Multiple::new()
            }
            EndpointUpdate::Unchanged => {
                // Nothing to do.
                Multiple::new()
            }
            EndpointUpdate::InvalidSignature { signed, err } => {
                warn!(%err, ?signed, "received invalid endpoint");
                Multiple::new()
            }
        }
    }

    /// Sets up an established outgoing connection.
    fn setup_outgoing(
        &mut self,
        node_id: NodeId,
        transport: Transport,
    ) -> Multiple<Effect<Event<P>>> {
        // This connection is send-only, we only use the sink.
        let (sink, _stream) = framed::<P>(transport).split();

        let (sender, receiver) = mpsc::unbounded_channel();
        if self.outgoing.insert(node_id, sender).is_some() {
            // We assume that for a reconnect to have happened, the outgoing entry must have
            // been either non-existent yet or cleaned up by the handler of the connection
            // closing event. If this is not the case, an assumed invariant has been violated.
            error!(%node_id, "did not expect leftover channel in outgoing map");
        }

        // We can now send a snapshot.
        let snapshot = Message::Snapshot(self.signed_endpoints.values().cloned().collect());
        self.send_message(node_id, snapshot);

        message_sender(receiver, sink).event(move |result| Event::OutgoingFailed {
            node_id,
            attempt_count: 0, // reset to 0, since we have had a successful connection
            error: result.err().map(Into::into),
        })
    }

    /// Handles a received message.
    // Internal function to keep indentation and nesting sane.
    fn handle_message(
        &mut self,
        effect_builder: EffectBuilder<REv>,
        node_id: NodeId,
        msg: Message<P>,
    ) -> Multiple<Effect<Event<P>>>
    where
        REv: From<NetworkAnnouncement<NodeId, P>>,
    {
        match msg {
            Message::Snapshot(snapshot) => snapshot
                .into_iter()
                .map(|signed| self.update_and_broadcast_if_new(signed))
                .flatten()
                .collect(),
            Message::BroadcastEndpoint(signed) => self.update_and_broadcast_if_new(signed),
            Message::Payload(payload) => {
                // We received a message payload, announce it.
                effect_builder
                    .announce_message_received(node_id, payload)
                    .ignore()
            }
        }
    }

    /// Returns the set of connected nodes.
    ///
    /// This inspection function is usually used in testing.
    #[cfg(test)]
    pub(crate) fn connected_nodes(&self) -> HashSet<NodeId> {
        self.outgoing.keys().cloned().collect()
    }

    /// Returns the node id of this network node.
    pub(crate) fn node_id(&self) -> NodeId {
        self.cert.public_key_fingerprint()
    }
}

impl<REv, P> Component<REv> for SmallNetwork<REv, P>
where
    REv: Send + From<Event<P>> + From<NetworkAnnouncement<NodeId, P>>,
    P: Serialize + DeserializeOwned + Clone + Debug + Display + Send + 'static,
{
    type Event = Event<P>;

    #[allow(clippy::cognitive_complexity)]
    fn handle_event<R: Rng + ?Sized>(
        &mut self,
        effect_builder: EffectBuilder<REv>,
        rng: &mut R,
        event: Self::Event,
    ) -> Multiple<Effect<Self::Event>> {
        match event {
            Event::RootConnected { cert, transport } => {
                // Create a pseudo-endpoint for the root node with the lowest priority (time 0)
                let root_node_id = cert.public_key_fingerprint();

                let ep = Endpoint::new(0, self.cfg.root_addr, cert);
                if self.endpoints.insert(root_node_id, ep).is_some() {
                    // This connection is the very first we will ever make, there should never be
                    // a root node registered, as we will never re-attempt this connection if it
                    // succeeded once.
                    error!("Encountered a second root node connection.")
                }

                // We're now almost setup exactly as if the root node was any other node, proceed
                // as normal.
                self.setup_outgoing(root_node_id, transport)
            }
            Event::RootFailed { error } => {
                warn!(%error, "connection to root failed");
                self.connect_to_root()

                // TODO: delay next attempt
            }
            Event::IncomingNew { stream, addr } => {
                debug!(%addr, "incoming connection, starting TLS handshake");

                setup_tls(stream, self.cert.clone(), self.private_key.clone())
                    .boxed()
                    .event(move |result| Event::IncomingHandshakeCompleted { result, addr })
            }
            Event::IncomingHandshakeCompleted { result, addr } => {
                match result {
                    Ok((fingerprint, transport)) => {
                        debug!(%addr, peer=%fingerprint, "established new connection");
                        // The sink is never used, as we only read data from incoming connections.
                        let (_sink, stream) = framed::<P>(transport).split();

                        message_reader(self.event_queue, stream, fingerprint)
                            .event(move |result| Event::IncomingClosed { result, addr })
                    }
                    Err(err) => {
                        warn!(%addr, %err, "TLS handshake failed");
                        Multiple::new()
                    }
                }
            }
            Event::IncomingMessage { node_id, msg } => {
                self.handle_message(effect_builder, node_id, msg)
            }
            Event::IncomingClosed { result, addr } => {
                match result {
                    Ok(()) => info!(%addr, "connection closed"),
                    Err(err) => warn!(%addr, %err, "connection dropped"),
                }
                Multiple::new()
            }
            Event::OutgoingEstablished { node_id, transport } => {
                self.setup_outgoing(node_id, transport)
            }
            Event::OutgoingFailed {
                node_id,
                attempt_count,
                error,
            } => {
                if let Some(err) = error {
                    warn!(%node_id, %err, "outgoing connection failed");
                } else {
                    warn!(%node_id, "outgoing connection closed");
                }

                if let Some(max) = self.cfg.max_outgoing_retries {
                    if attempt_count >= max {
                        // We're giving up connecting to the node. We will remove it completely
                        // (this only carries the danger of the stale addresses being sent to us by
                        // other nodes again).
                        self.endpoints.remove(&node_id);
                        self.signed_endpoints.remove(&node_id);
                        self.outgoing.remove(&node_id);

                        warn!(%attempt_count, %node_id, "gave up on outgoing connection");
                        return Multiple::new();
                    }
                }

                if let Some(endpoint) = self.endpoints.get(&node_id) {
                    let ep = endpoint.clone();
                    let cert = self.cert.clone();
                    let private_key = self.private_key.clone();

                    effect_builder
                        .set_timeout(Duration::from_millis(self.cfg.outgoing_retry_delay_millis))
                        .then(move |_| connect_outgoing(ep, cert, private_key))
                        .result(
                            move |transport| Event::OutgoingEstablished { node_id, transport },
                            move |error| Event::OutgoingFailed {
                                node_id,
                                attempt_count: attempt_count + 1,
                                error: Some(error),
                            },
                        )
                } else {
                    error!("endpoint disappeared");
                    Multiple::new()
                }
            }
            Event::NetworkRequest {
                req:
                    NetworkRequest::SendMessage {
                        dest,
                        payload,
                        responder,
                    },
            } => {
                // We're given a message to send out.
                self.send_message(dest, Message::Payload(payload));
                responder.respond(()).ignore()
            }
            Event::NetworkRequest {
                req: NetworkRequest::Broadcast { payload, responder },
            } => {
                // We're given a message to broadcast.
                self.broadcast_message(Message::Payload(payload));
                responder.respond(()).ignore()
            }
            Event::NetworkRequest {
                req:
                    NetworkRequest::Gossip {
                        payload,
                        count,
                        exclude,
                        responder,
                    },
            } => {
                // We're given a message to gossip.
                self.gossip_message(rng, Message::Payload(payload), count, exclude);
                responder.respond(()).ignore()
            }
        }
    }
}

/// Determines bind address for now.
///
/// Will attempt to bind on the root address first if the `bind_interface` is the same as the
/// interface of `root_addr`. Otherwise uses an unused port on `bind_interface`.
///
/// Returns a `(listener, is_root)` pair. `is_root` is `true` if the node is a root node.
fn create_listener(cfg: &Config) -> io::Result<(TcpListener, bool)> {
    if cfg.root_addr.ip() == cfg.bind_interface
        && (cfg.bind_port == 0 || cfg.root_addr.port() == cfg.bind_port)
    {
        // Try to become the root node, if the root nodes interface is available.
        match TcpListener::bind(cfg.root_addr) {
            Ok(listener) => {
                info!("we are the root node");
                return Ok((listener, true));
            }
            Err(err) => {
                warn!(
                    %err,
                    "could not bind to {}, will become a non-root node", cfg.root_addr
                );
            }
        };
    }

    // We did not become the root node, bind on the specified port.
    Ok((
        TcpListener::bind((cfg.bind_interface, cfg.bind_port))?,
        false,
    ))
}

/// Core accept loop for the networking server.
///
/// Never terminates.
async fn server_task<P, REv>(
    event_queue: EventQueueHandle<REv>,
    mut listener: tokio::net::TcpListener,
    shutdown: oneshot::Receiver<()>,
    span: Span,
) where
    REv: From<Event<P>>,
{
    let _enter = span.enter();

    // The server task is a bit tricky, since it has to wait on incoming connections while at the
    // same time shut down if the networking component is dropped, otherwise the TCP socket will
    // stay open, preventing reuse.

    // We first create a future that never terminates, handling incoming connections:
    let accept_connections = async move {
        loop {
            // We handle accept errors here, since they can be caused by a temporary resource
            // shortage or the remote side closing the connection while it is waiting in
            // the queue.
            match listener.accept().await {
                Ok((stream, addr)) => {
                    // Move the incoming connection to the event queue for handling.
                    let event = Event::IncomingNew { stream, addr };
                    event_queue
                        .schedule(event, QueueKind::NetworkIncoming)
                        .await;
                }
                // TODO: Handle resource errors gracefully.
                //       In general, two kinds of errors occur here: Local resource exhaustion,
                //       which should be handled by waiting a few milliseconds, or remote connection
                //       errors, which can be dropped immediately.
                //
                //       The code in its current state will consume 100% CPU if local resource
                //       exhaustion happens, as no distinction is made and no delay introduced.
                Err(err) => warn!(%err, "dropping incoming connection during accept"),
            }
        }
    };

    // Now we can wait for either the `shutdown` channel's remote end to do be dropped or the
    // infinite loop to terminate, which never happens.
    match select(shutdown, Box::pin(accept_connections)).await {
        Either::Left(_) => info!("shutting down socket, no longer accepting incoming connections"),
        Either::Right(_) => unreachable!(),
    }
}

/// Server-side TLS handshake.
///
/// This function groups the TLS handshake into a convenient function, enabling the `?` operator.
async fn setup_tls(
    stream: TcpStream,
    cert: Arc<TlsCert>,
    private_key: Arc<PKey<Private>>,
) -> Result<(NodeId, Transport)> {
    let tls_stream = tokio_openssl::accept(
        &tls::create_tls_acceptor(&cert.as_x509().as_ref(), &private_key.as_ref())
            .map_err(Error::AcceptorCreation)?,
        stream,
    )
    .await?;

    // We can now verify the certificate.
    let peer_cert = tls_stream
        .ssl()
        .peer_certificate()
        .ok_or_else(|| Error::NoClientCertificate)?;

    Ok((
        tls::validate_cert(peer_cert)?.public_key_fingerprint(),
        tls_stream,
    ))
}

/// Network message reader.
///
/// Schedules all received messages until the stream is closed or an error occurs.
async fn message_reader<REv, P>(
    event_queue: EventQueueHandle<REv>,
    mut stream: SplitStream<FramedTransport<P>>,
    node_id: NodeId,
) -> io::Result<()>
where
    P: DeserializeOwned + Send + Display,
    REv: From<Event<P>>,
{
    while let Some(msg_result) = stream.next().await {
        match msg_result {
            Ok(msg) => {
                debug!(%msg, %node_id, "message received");
                // We've received a message, push it to the reactor.
                event_queue
                    .schedule(
                        Event::IncomingMessage { node_id, msg },
                        QueueKind::NetworkIncoming,
                    )
                    .await;
            }
            Err(err) => {
                warn!(%err, peer=%node_id, "receiving message failed, closing connection");
                return Err(err);
            }
        }
    }
    Ok(())
}

/// Network message sender.
///
/// Reads from a channel and sends all messages, until the stream is closed or an error occurs.
async fn message_sender<P>(
    mut queue: UnboundedReceiver<Message<P>>,
    mut sink: SplitSink<FramedTransport<P>, Message<P>>,
) -> Result<()>
where
    P: Serialize + Send,
{
    while let Some(payload) = queue.recv().await {
        // We simply error-out if the sink fails, it means that our connection broke.
        sink.send(payload).await.map_err(Error::MessageNotSent)?;
    }

    Ok(())
}

/// Transport type alias for base encrypted connections.
type Transport = SslStream<TcpStream>;

/// A framed transport for `Message`s.
type FramedTransport<P> = SymmetricallyFramed<
    Framed<Transport, LengthDelimitedCodec>,
    Message<P>,
    SymmetricalMessagePack<Message<P>>,
>;

/// Constructs a new framed transport on a stream.
fn framed<P>(stream: Transport) -> FramedTransport<P> {
    let length_delimited = Framed::new(stream, LengthDelimitedCodec::new());
    SymmetricallyFramed::new(
        length_delimited,
        SymmetricalMessagePack::<Message<P>>::default(),
    )
}

/// Initiates a TLS connection to an endpoint.
async fn connect_outgoing(
    endpoint: Endpoint,
    cert: Arc<TlsCert>,
    private_key: Arc<PKey<Private>>,
) -> Result<Transport> {
    let (server_cert, transport) = connect_trusted(endpoint.addr(), cert, private_key).await?;

    let remote_id = server_cert.public_key_fingerprint();

    if remote_id != endpoint.cert().public_key_fingerprint() {
        return Err(Error::WrongId);
    }

    Ok(transport)
}

/// Initiates a TLS connection to a remote address, regardless of what ID the remote node reports.
async fn connect_trusted(
    addr: SocketAddr,
    cert: Arc<TlsCert>,
    private_key: Arc<PKey<Private>>,
) -> Result<(TlsCert, Transport)> {
    let mut config = tls::create_tls_connector(&cert.as_x509(), &private_key)
        .context("could not create TLS connector")?
        .configure()
        .map_err(Error::ConnectorConfiguration)?;
    config.set_verify_hostname(false);

    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .context("TCP connection failed")?;

    let tls_stream = tokio_openssl::connect(config, "this-will-not-be-checked.example.com", stream)
        .await
        .context("tls handshake failed")?;

    let server_cert = tls_stream
        .ssl()
        .peer_certificate()
        .ok_or_else(|| Error::NoServerCertificate)?;
    Ok((tls::validate_cert(server_cert)?, tls_stream))
}

impl<R, P> Debug for SmallNetwork<R, P>
where
    P: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmallNetwork")
            .field("cert", &"<SSL cert>")
            .field("private_key", &"<hidden>")
            .field("event_queue", &"<event_queue>")
            .field("endpoints", &self.endpoints)
            .field("signed_endpoints", &self.signed_endpoints)
            .field("outgoing", &self.outgoing)
            .finish()
    }
}
