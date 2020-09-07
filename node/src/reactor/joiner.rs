//! Reactor used to join the network.

use std::{
    fmt::{self, Display, Formatter},
    path::PathBuf,
};

use derive_more::From;
use prometheus::Registry;
use rand::{CryptoRng, Rng};

use crate::{
    components::{
        chainspec_loader::ChainspecLoader,
        contract_runtime::ContractRuntime,
        small_network,
        small_network::{NodeId, SmallNetwork},
        storage::Storage,
        Component,
    },
    effect::{announcements::NetworkAnnouncement, EffectBuilder, Effects},
    protocol::Message,
    reactor::{
        self, initializer,
        validator::{self, Error, ValidatorInitConfig},
        EventQueueHandle, Finalize,
    },
    utils::WithDir,
};

/// Top-level event for the reactor.
#[derive(Debug, From)]
#[must_use]
pub enum Event {
    /// Network event.
    #[from]
    Network(small_network::Event<Message>),

    // Announcements
    /// Network announcement.
    #[from]
    NetworkAnnouncement(NetworkAnnouncement<NodeId, Message>),
}

impl Display for Event {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Event::Network(event) => write!(f, "network: {}", event),
            Event::NetworkAnnouncement(event) => write!(f, "network announcement: {}", event),
        }
    }
}

/// Joining node reactor.
#[derive(Debug)]
pub struct Reactor {
    pub(super) root: PathBuf,
    pub(super) net: SmallNetwork<Event, Message>,
    pub(super) config: validator::Config,
    pub(super) chainspec_loader: ChainspecLoader,
    pub(super) storage: Storage,
    pub(super) contract_runtime: ContractRuntime,
}

impl<R: Rng + CryptoRng + ?Sized> reactor::Reactor<R> for Reactor {
    type Event = Event;

    // The "configuration" is in fact the whole state of the initializer reactor, which we
    // deconstruct and reuse.
    type Config = WithDir<initializer::Reactor>;
    type Error = Error;

    fn new(
        initializer: Self::Config,
        _registry: &Registry,
        event_queue: EventQueueHandle<Self::Event>,
        _rng: &mut R,
    ) -> Result<(Self, Effects<Self::Event>), Self::Error> {
        let (root, initializer) = initializer.into_parts();

        let initializer::Reactor {
            config,
            chainspec_loader,
            storage,
            contract_runtime,
        } = initializer;

        let (net, net_effects) = SmallNetwork::new(
            event_queue,
            WithDir::new(root.clone(), config.validator_net.clone()),
        )?;

        Ok((
            Self {
                net,
                root,
                config,
                chainspec_loader,
                storage,
                contract_runtime,
            },
            reactor::wrap_effects(Event::Network, net_effects),
        ))
    }

    fn dispatch_event(
        &mut self,
        effect_builder: EffectBuilder<Self::Event>,
        rng: &mut R,
        event: Self::Event,
    ) -> Effects<Self::Event> {
        match event {
            Event::Network(event) => reactor::wrap_effects(
                Event::Network,
                self.net.handle_event(effect_builder, rng, event),
            ),
            Event::NetworkAnnouncement(_) => Default::default(),
        }
    }

    fn is_stopped(&mut self) -> bool {
        // TODO!
        true
    }
}

impl Reactor {
    /// Deconstructs the reactor into config useful for creating a Validator reactor. Shuts down
    /// the network, closing all incoming and outgoing connections, and frees up the listening
    /// socket.
    pub async fn into_validator_config(self) -> ValidatorInitConfig {
        let (net, config) = (
            self.net,
            ValidatorInitConfig {
                root: self.root,
                chainspec_loader: self.chainspec_loader,
                config: self.config,
                contract_runtime: self.contract_runtime,
                storage: self.storage,
            },
        );
        net.finalize().await;
        config
    }
}
