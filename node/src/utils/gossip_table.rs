#[cfg(not(test))]
use std::time::Instant;
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    fmt::Display,
    hash::Hash,
    time::Duration,
};

#[cfg(test)]
use fake_instant::FakeClock as Instant;

use serde::{
    de::{Deserializer, Error as SerdeError, Unexpected},
    Deserialize, Serialize,
};
use thiserror::Error;
use tracing::{error, warn};

use crate::small_network::NodeId;

const DEFAULT_INFECTION_TARGET: u8 = 3;
const DEFAULT_SATURATION_LIMIT_PERCENT: u8 = 80;
const MAX_SATURATION_LIMIT_PERCENT: u8 = 99;
const DEFAULT_FINISHED_ENTRY_DURATION_SECS: u64 = 3_600;
const DEFAULT_GOSSIP_REQUEST_TIMEOUT_SECS: u64 = 10;
const DEFAULT_GET_REMAINDER_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GossipAction {
    /// This is new data, previously unknown by us, and for which we don't yet hold everything
    /// required to allow us start gossiping it onwards.  We should get the remaining parts from
    /// the provided holder and not gossip the ID onwards yet.
    GetRemainder { holder: NodeId },
    /// This is data already known to us, but for which we don't yet hold everything required to
    /// allow us start gossiping it onwards.  We should already be getting the remaining parts from
    /// a holder, so there's no need to do anything else now.
    AwaitingRemainder,
    /// We hold the data locally and should gossip the ID onwards.
    ShouldGossip(ShouldGossip),
    /// We hold the data locally, and we shouldn't gossip the ID onwards.
    Noop,
}

/// Used as a return type from API methods to indicate that the caller should continue to gossip the
/// given data.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ShouldGossip {
    /// The number of copies of the gossip message to send.
    pub(crate) count: usize,
    /// Peers we should avoid gossiping this data to, since they already hold it.
    pub(crate) exclude_peers: HashSet<NodeId>,
}

/// Error returned by a `GossipTable`.
#[derive(Debug, Error)]
pub enum Error {
    /// Invalid configuration value for `saturation_limit_percent`.
    #[error(
        "invalid saturation_limit_percent - should be between 0 and {} inclusive",
        MAX_SATURATION_LIMIT_PERCENT
    )]
    InvalidSaturationLimit,

    /// Attempted to reset data which had not been paused.
    #[error("gossiping is not paused for this data")]
    NotPaused,
}

/// Configuration options for gossiping.
#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    /// Target number of peers to infect with a given piece of data.
    infection_target: u8,
    /// The saturation limit as a percentage, with a maximum value of 99.  Used as a termination
    /// condition.
    ///
    /// Example: assume the `infection_target` is 3, the `saturation_limit_percent` is 80, and we
    /// don't manage to newly infect 3 peers.  We will stop gossiping once we know of more than 15
    /// holders excluding us since 80% saturation would imply 3 new infections in 15 peers.
    #[serde(deserialize_with = "deserialize_saturation_limit_percent")]
    saturation_limit_percent: u8,
    /// The maximum duration in seconds for which to keep finished entries.
    ///
    /// The longer they are retained, the lower the likelihood of re-gossiping a piece of data.
    /// However, the longer they are retained, the larger the list of finished entries can grow.
    finished_entry_duration_secs: u64,
    /// The timeout duration in seconds for a single gossip request, i.e. for a single gossip
    /// message sent from this node, it will be considered timed out if the expected response from
    /// that peer is not received within this specified duration.
    gossip_request_timeout_secs: u64,
    /// The timeout duration in seconds for retrieving the remaining part(s) of newly-discovered
    /// data from a peer which gossiped information about that data to this node.
    get_remainder_timeout_secs: u64,
}

impl Config {
    #[cfg(test)]
    pub(crate) fn new(
        infection_target: u8,
        saturation_limit_percent: u8,
        finished_entry_duration_secs: u64,
        gossip_request_timeout_secs: u64,
        get_remainder_timeout_secs: u64,
    ) -> Result<Self, Error> {
        if saturation_limit_percent > MAX_SATURATION_LIMIT_PERCENT {
            return Err(Error::InvalidSaturationLimit);
        }
        Ok(Config {
            infection_target,
            saturation_limit_percent,
            finished_entry_duration_secs,
            gossip_request_timeout_secs,
            get_remainder_timeout_secs,
        })
    }

    pub(crate) fn gossip_request_timeout_secs(&self) -> u64 {
        self.gossip_request_timeout_secs
    }

    pub(crate) fn get_remainder_timeout_secs(&self) -> u64 {
        self.get_remainder_timeout_secs
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            infection_target: DEFAULT_INFECTION_TARGET,
            saturation_limit_percent: DEFAULT_SATURATION_LIMIT_PERCENT,
            finished_entry_duration_secs: DEFAULT_FINISHED_ENTRY_DURATION_SECS,
            gossip_request_timeout_secs: DEFAULT_GOSSIP_REQUEST_TIMEOUT_SECS,
            get_remainder_timeout_secs: DEFAULT_GET_REMAINDER_TIMEOUT_SECS,
        }
    }
}

#[derive(Debug, Default)]
struct State {
    /// The peers excluding us which hold the data.
    holders: HashSet<NodeId>,
    /// Whether we hold the full data locally yet or not.
    held_by_us: bool,
    /// The subset of `holders` we have infected.  Not just a count so we don't attribute the same
    /// peer multiple times.
    infected_by_us: HashSet<NodeId>,
    /// The count of in-flight gossip messages sent by us for this data.
    in_flight_count: usize,
}

impl State {
    /// Returns whether we should finish gossiping this data.
    fn is_finished(&self, infection_target: usize, holders_limit: usize) -> bool {
        self.infected_by_us.len() >= infection_target || self.holders.len() >= holders_limit
    }

    /// Returns a `GossipAction` derived from the given state.
    fn action(
        &mut self,
        infection_target: usize,
        holders_limit: usize,
        is_new: bool,
    ) -> GossipAction {
        if self.is_finished(infection_target, holders_limit) {
            return GossipAction::Noop;
        }

        if self.held_by_us {
            let count = infection_target.saturating_sub(self.in_flight_count);
            if count > 0 {
                self.in_flight_count += count;
                return GossipAction::ShouldGossip(ShouldGossip {
                    count,
                    exclude_peers: self.holders.clone(),
                });
            } else {
                return GossipAction::Noop;
            }
        }

        if is_new {
            let holder = self
                .holders
                .iter()
                .next()
                .expect("holders cannot be empty if we don't hold the data");
            GossipAction::GetRemainder { holder: *holder }
        } else {
            GossipAction::AwaitingRemainder
        }
    }
}

#[derive(Debug)]
pub(crate) struct GossipTable<T> {
    /// Data IDs for which gossiping is still ongoing.
    current: HashMap<T, State>,
    /// Data IDs for which gossiping is complete.  The map's values are the times after which the
    /// relevant entries can be removed.
    finished: HashMap<T, Instant>,
    /// Data IDs for which gossiping has been paused (likely due to detecting that the data was not
    /// correct as per our current knowledge).  Such data could later be decided as still requiring
    /// to be gossiped, so we retain the `State` part here in order to resume gossiping.
    paused: HashMap<T, (State, Instant)>,
    /// See `Config::infection_target`.
    infection_target: usize,
    /// Derived from `Config::saturation_limit_percent` - we gossip data while the number of
    /// holders doesn't exceed `holders_limit`.
    holders_limit: usize,
    /// See `Config::finished_entry_duration`.
    finished_entry_duration: Duration,
}

impl<T: Copy + Eq + Hash + Display> GossipTable<T> {
    /// Returns a new `GossipTable` using the provided configuration.
    pub(crate) fn new(config: Config) -> Self {
        let holders_limit = (100 * usize::from(config.infection_target))
            / (100 - usize::from(config.saturation_limit_percent));
        GossipTable {
            current: HashMap::new(),
            finished: HashMap::new(),
            paused: HashMap::new(),
            infection_target: usize::from(config.infection_target),
            holders_limit,
            finished_entry_duration: Duration::from_secs(config.finished_entry_duration_secs),
        }
    }

    /// We received knowledge about potentially new data with given ID from the given peer.  This
    /// should only be called where we don't already hold everything locally we need to be able to
    /// gossip it onwards.  If we are able to gossip the data already, call `new_data` instead.
    ///
    /// Once we have retrieved everything we need in order to begin gossiping onwards, call
    /// `new_data`.
    ///
    /// Returns whether we should gossip it, and a list of peers to exclude.
    pub(crate) fn new_partial_data(&mut self, data_id: &T, holder: NodeId) -> GossipAction {
        self.purge_finished();

        if self.finished.contains_key(data_id) {
            return GossipAction::Noop;
        }

        if let Some((state, _timeout)) = self.paused.get_mut(data_id) {
            let _ = state.holders.insert(holder);
            return GossipAction::Noop;
        }

        match self.current.entry(*data_id) {
            Entry::Occupied(mut entry) => {
                let is_new = false;
                let state = entry.get_mut();
                let _ = state.holders.insert(holder);
                state.action(self.infection_target, self.holders_limit, is_new)
            }
            Entry::Vacant(entry) => {
                let is_new = true;
                let state = entry.insert(State::default());
                let _ = state.holders.insert(holder);
                state.action(self.infection_target, self.holders_limit, is_new)
            }
        }
    }

    /// We received or generated potentially new data with given ID.  If received from a peer,
    /// its ID should be passed in `maybe_holder`.  If received from a client or generated on this
    /// node, `maybe_holder` should be `None`.
    ///
    /// This should only be called once we hold everything locally we need to be able to gossip it
    /// onwards.  If we aren't able to gossip this data yet, call `new_data_id` instead.
    ///
    /// Returns whether we should gossip it, and a list of peers to exclude.
    pub(crate) fn new_complete_data(
        &mut self,
        data_id: &T,
        maybe_holder: Option<NodeId>,
    ) -> Option<ShouldGossip> {
        self.purge_finished();

        if self.finished.contains_key(data_id) {
            return None;
        }

        let update = |state: &mut State| {
            state.holders.extend(maybe_holder);
            state.held_by_us = true;
        };

        if let Some((state, _timeout)) = self.paused.get_mut(data_id) {
            update(state);
            return None;
        }

        let action = match self.current.entry(*data_id) {
            Entry::Occupied(mut entry) => {
                let state = entry.get_mut();
                update(state);
                let is_new = false;
                state.action(self.infection_target, self.holders_limit, is_new)
            }
            Entry::Vacant(entry) => {
                let state = entry.insert(State::default());
                update(state);
                let is_new = true;
                state.action(self.infection_target, self.holders_limit, is_new)
            }
        };

        match action {
            GossipAction::ShouldGossip(should_gossip) => Some(should_gossip),
            GossipAction::Noop => None,
            GossipAction::GetRemainder { .. } | GossipAction::AwaitingRemainder => {
                unreachable!("can't be waiting for remainder since we hold the complete data")
            }
        }
    }

    /// We got a response from a peer we gossiped to indicating we infected it (it didn't previously
    /// know of this data).
    ///
    /// If the given `data_id` is not a member of the current entries (those not deemed finished),
    /// then `GossipAction::Noop` will be returned under the assumption that the data has already
    /// finished being gossiped.
    pub(crate) fn we_infected(&mut self, data_id: &T, peer: NodeId) -> GossipAction {
        let infected_by_us = true;
        self.infected(data_id, peer, infected_by_us)
    }

    /// We got a response from a peer we gossiped to indicating it was already infected (it
    /// previously knew of this data).
    ///
    /// If the given `data_id` is not a member of the current entries (those not deemed finished),
    /// then `GossipAction::Noop` will be returned under the assumption that the data has already
    /// finished being gossiped.
    pub(crate) fn already_infected(&mut self, data_id: &T, peer: NodeId) -> GossipAction {
        let infected_by_us = false;
        self.infected(data_id, peer, infected_by_us)
    }

    fn infected(&mut self, data_id: &T, peer: NodeId, by_us: bool) -> GossipAction {
        let infection_target = self.infection_target;
        let holders_limit = self.holders_limit;
        let update = |state: &mut State| {
            if !state.held_by_us {
                warn!(
                    %data_id,
                    %peer, "shouldn't have received a gossip response for partial data"
                );
                return None;
            }
            let _ = state.holders.insert(peer);
            if by_us {
                let _ = state.infected_by_us.insert(peer);
            }
            state.in_flight_count = state.in_flight_count.saturating_sub(1);
            Some(state.is_finished(infection_target, holders_limit))
        };

        let is_finished = if let Some(state) = self.current.get_mut(data_id) {
            let is_finished = match update(state) {
                Some(is_finished) => is_finished,
                None => return GossipAction::Noop,
            };
            if !is_finished {
                let is_new = false;
                return state.action(self.infection_target, self.holders_limit, is_new);
            }
            true
        } else {
            false
        };

        if is_finished {
            let _ = self.current.remove(data_id);
            let timeout = Instant::now() + self.finished_entry_duration;
            let _ = self.finished.insert(*data_id, timeout);
            return GossipAction::Noop;
        }

        let is_finished = if let Some((state, _timeout)) = self.paused.get_mut(data_id) {
            match update(state) {
                Some(is_finished) => is_finished,
                None => return GossipAction::Noop,
            }
        } else {
            false
        };

        if is_finished {
            let _ = self.paused.remove(data_id);
            let timeout = Instant::now() + self.finished_entry_duration;
            let _ = self.finished.insert(*data_id, timeout);
        }

        GossipAction::Noop
    }

    /// Checks if gossip request we sent timed out.
    ///
    /// If the peer is already counted as a holder, it has previously responded and this method
    /// returns Noop.  Otherwise it has timed out and we return the appropriate action to take.
    pub(crate) fn check_timeout(&mut self, data_id: &T, peer: NodeId) -> GossipAction {
        if let Some(state) = self.current.get_mut(data_id) {
            debug_assert!(
                state.held_by_us,
                "shouldn't check timeout for a gossip response for partial data"
            );

            if !state.holders.contains(&peer) {
                state.in_flight_count = state.in_flight_count.saturating_sub(1);
                let is_new = false;
                return state.action(self.infection_target, self.holders_limit, is_new);
            }
        }

        GossipAction::Noop
    }

    /// If we hold the full data, assume `peer` provided it to us and shouldn't be removed as a
    /// holder.  Otherwise, assume `peer` was unresponsive and remove from list of holders.
    ///
    /// If this causes the list of holders to become empty, and we also don't hold the full data,
    /// then this entry is removed as if we'd never heard of it.
    pub(crate) fn remove_holder_if_unresponsive(
        &mut self,
        data_id: &T,
        peer: NodeId,
    ) -> GossipAction {
        if let Some(mut state) = self.current.remove(data_id) {
            if !state.held_by_us {
                let _ = state.holders.remove(&peer);
                if state.holders.is_empty() {
                    // We don't hold the full data, and we don't know any holders - pause the entry
                    return GossipAction::Noop;
                }
            }
            let is_new = !state.held_by_us;
            let action = state.action(self.infection_target, self.holders_limit, is_new);
            let _ = self.current.insert(*data_id, state);
            return action;
        }

        if let Some((state, _timeout)) = self.paused.get_mut(data_id) {
            if !state.held_by_us {
                let _ = state.holders.remove(&peer);
            }
        }

        GossipAction::Noop
    }

    /// We have deemed the data not suitable for gossiping further.  If left in paused state, the
    /// entry will eventually be purged, as for finished entries.
    pub(crate) fn pause(&mut self, data_id: &T) {
        if let Some(mut state) = self.current.remove(data_id) {
            state.in_flight_count = 0;
            let timeout = Instant::now() + self.finished_entry_duration;
            let _ = self.paused.insert(*data_id, (state, timeout));
        }
    }

    /// Resumes gossiping of paused entry.
    ///
    /// Returns an error if gossiping this data is not in a paused state.
    // TODO - remove lint relaxation once the method is used.
    #[allow(dead_code)]
    pub(crate) fn resume(&mut self, data_id: &T) -> Result<GossipAction, Error> {
        let (mut state, _timeout) = self.paused.remove(data_id).ok_or(Error::NotPaused)?;
        let is_new = !state.held_by_us;
        let action = state.action(self.infection_target, self.holders_limit, is_new);
        let _ = self.current.insert(*data_id, state);
        Ok(action)
    }

    /// Retains only those finished entries which still haven't timed out.
    fn purge_finished(&mut self) {
        let now = Instant::now();
        self.finished = self
            .finished
            .drain()
            .filter(|(_, timeout)| *timeout > now)
            .collect();
        self.paused = self
            .paused
            .drain()
            .filter(|(_, (_, timeout))| *timeout > now)
            .collect();
    }
}

/// Deserializes a `usize` but fails if it's not in the range 0..100.
fn deserialize_saturation_limit_percent<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let saturation_limit_percent = u8::deserialize(deserializer)?;
    if saturation_limit_percent > MAX_SATURATION_LIMIT_PERCENT {
        error!(
            "saturation_limit_percent of {} is above {}",
            saturation_limit_percent, MAX_SATURATION_LIMIT_PERCENT
        );
        return Err(SerdeError::invalid_value(
            Unexpected::Unsigned(saturation_limit_percent as u64),
            &"a value between 0 and 99 inclusive",
        ));
    }

    Ok(saturation_limit_percent)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, iter};

    use rand::Rng;

    use super::*;
    use crate::utils::DisplayIter;

    const EXPECTED_DEFAULT_INFECTION_TARGET: usize = 3;
    const EXPECTED_DEFAULT_HOLDERS_LIMIT: usize = 15;

    #[test]
    fn invalid_config_should_fail() {
        // saturation_limit_percent > MAX_SATURATION_LIMIT_PERCENT
        let invalid_config = Config {
            infection_target: 3,
            saturation_limit_percent: MAX_SATURATION_LIMIT_PERCENT + 1,
            finished_entry_duration_secs: DEFAULT_FINISHED_ENTRY_DURATION_SECS,
            gossip_request_timeout_secs: DEFAULT_GOSSIP_REQUEST_TIMEOUT_SECS,
            get_remainder_timeout_secs: DEFAULT_GET_REMAINDER_TIMEOUT_SECS,
        };

        // Parsing should fail.
        let config_as_json = serde_json::to_string(&invalid_config).unwrap();
        assert!(serde_json::from_str::<Config>(&config_as_json).is_err());

        // Construction should fail.
        assert!(Config::new(
            3,
            MAX_SATURATION_LIMIT_PERCENT + 1,
            DEFAULT_FINISHED_ENTRY_DURATION_SECS,
            DEFAULT_GOSSIP_REQUEST_TIMEOUT_SECS,
            DEFAULT_GET_REMAINDER_TIMEOUT_SECS,
        )
        .is_err())
    }

    fn random_node_ids() -> Vec<NodeId> {
        let mut rng = rand::thread_rng();
        iter::repeat_with(|| rng.gen::<NodeId>())
            .take(EXPECTED_DEFAULT_HOLDERS_LIMIT + 3)
            .collect()
    }

    fn check_holders(expected: &[NodeId], gossip_table: &GossipTable<u64>, data_id: &u64) {
        let expected: BTreeSet<_> = expected.iter().collect();
        let actual: BTreeSet<_> = gossip_table
            .current
            .get(data_id)
            .or_else(|| {
                gossip_table
                    .paused
                    .get(data_id)
                    .map(|(state, _timeout)| state)
            })
            .map_or_else(BTreeSet::new, |state| state.holders.iter().collect());
        assert!(
            expected == actual,
            "\nexpected: {}\nactual:   {}\n",
            DisplayIter::new(expected.iter()),
            DisplayIter::new(actual.iter())
        );
    }

    #[test]
    fn new_partial_data() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());
        assert_eq!(
            EXPECTED_DEFAULT_INFECTION_TARGET,
            gossip_table.infection_target
        );
        assert_eq!(EXPECTED_DEFAULT_HOLDERS_LIMIT, gossip_table.holders_limit);

        // Check new partial data causes `GetRemainder` to be returned.
        let action = gossip_table.new_partial_data(&data_id, node_ids[0]);
        let expected = GossipAction::GetRemainder {
            holder: node_ids[0],
        };
        assert_eq!(expected, action);
        check_holders(&node_ids[..1], &gossip_table, &data_id);

        // Check same partial data from same source causes `AwaitingRemainder` to be returned.
        let action = gossip_table.new_partial_data(&data_id, node_ids[0]);
        assert_eq!(GossipAction::AwaitingRemainder, action);
        check_holders(&node_ids[..1], &gossip_table, &data_id);

        // Check same partial data from different source causes `AwaitingRemainder` to be returned
        // and holders updated.
        let action = gossip_table.new_partial_data(&data_id, node_ids[1]);
        assert_eq!(GossipAction::AwaitingRemainder, action);
        check_holders(&node_ids[..2], &gossip_table, &data_id);

        // Pause gossiping and check same partial data from third source causes `Noop` to be
        // returned and holders updated.
        gossip_table.pause(&data_id);
        let action = gossip_table.new_partial_data(&data_id, node_ids[2]);
        assert_eq!(GossipAction::Noop, action);
        check_holders(&node_ids[..3], &gossip_table, &data_id);

        // Reset the data and check same partial data from fourth source causes `AwaitingRemainder`
        // to be returned and holders updated.
        gossip_table.resume(&data_id).unwrap();
        let action = gossip_table.new_partial_data(&data_id, node_ids[3]);
        assert_eq!(GossipAction::AwaitingRemainder, action);
        check_holders(&node_ids[..4], &gossip_table, &data_id);

        // Finish the gossip by reporting three infections, then check same partial data causes
        // `Noop` to be returned and holders cleared.
        let _ = gossip_table.new_complete_data(&data_id, Some(node_ids[0]));
        let limit = 4 + EXPECTED_DEFAULT_INFECTION_TARGET;
        for node_id in &node_ids[4..limit] {
            let _ = gossip_table.we_infected(&data_id, *node_id);
        }
        let action = gossip_table.new_partial_data(&data_id, node_ids[limit]);
        assert_eq!(GossipAction::Noop, action);
        check_holders(&node_ids[..0], &gossip_table, &data_id);

        // Time the finished data out, then check same partial data causes `GetRemainder` to be
        // returned as per a completely new entry.
        Instant::advance_time(DEFAULT_FINISHED_ENTRY_DURATION_SECS * 1_000);
        let action = gossip_table.new_partial_data(&data_id, node_ids[0]);
        let expected = GossipAction::GetRemainder {
            holder: node_ids[0],
        };
        assert_eq!(expected, action);
        check_holders(&node_ids[..1], &gossip_table, &data_id);
    }

    #[test]
    fn should_noop_if_we_have_partial_data_and_get_gossip_response() {
        let mut rng = rand::thread_rng();
        let node_id: NodeId = rng.gen();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());

        let _ = gossip_table.new_partial_data(&data_id, node_id);

        let action = gossip_table.we_infected(&data_id, node_id);
        assert_eq!(GossipAction::Noop, action);

        let action = gossip_table.already_infected(&data_id, node_id);
        assert_eq!(GossipAction::Noop, action);
    }

    #[test]
    fn new_complete_data() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());

        // Check new complete data from us causes `ShouldGossip` to be returned.
        let action = gossip_table.new_complete_data(&data_id, None);
        let expected = Some(ShouldGossip {
            count: EXPECTED_DEFAULT_INFECTION_TARGET,
            exclude_peers: HashSet::new(),
        });
        assert_eq!(expected, action);
        check_holders(&node_ids[..0], &gossip_table, &data_id);

        // Check same complete data from other source causes `Noop` to be returned since we still
        // have all gossip requests in flight.  Check it updates holders.
        let action = gossip_table.new_complete_data(&data_id, Some(node_ids[0]));
        assert!(action.is_none());
        check_holders(&node_ids[..1], &gossip_table, &data_id);

        // Check receiving a gossip response, causes `ShouldGossip` to be returned and holders
        // updated.
        let action = gossip_table.already_infected(&data_id, node_ids[1]);
        let expected = GossipAction::ShouldGossip(ShouldGossip {
            count: 1,
            exclude_peers: node_ids[..2].iter().copied().collect(),
        });
        assert_eq!(expected, action);
        check_holders(&node_ids[..2], &gossip_table, &data_id);

        // Pause gossiping and check same complete data from third source causes `Noop` to be
        // returned and holders updated.
        gossip_table.pause(&data_id);
        let action = gossip_table.new_complete_data(&data_id, Some(node_ids[2]));
        assert!(action.is_none());
        check_holders(&node_ids[..3], &gossip_table, &data_id);

        // Reset the data and check same complete data from fourth source causes Noop` to be
        // returned since we still have all gossip requests in flight.  Check it updates holders.
        let action = gossip_table.resume(&data_id).unwrap();
        let expected = GossipAction::ShouldGossip(ShouldGossip {
            count: EXPECTED_DEFAULT_INFECTION_TARGET,
            exclude_peers: node_ids[..3].iter().copied().collect(),
        });
        assert_eq!(expected, action);

        let action = gossip_table.new_complete_data(&data_id, Some(node_ids[3]));
        assert!(action.is_none());
        check_holders(&node_ids[..4], &gossip_table, &data_id);

        // Finish the gossip by reporting enough non-infections, then check same complete data
        // causes `Noop` to be returned and holders cleared.
        let limit = 4 + EXPECTED_DEFAULT_INFECTION_TARGET;
        for node_id in &node_ids[4..limit] {
            let _ = gossip_table.we_infected(&data_id, *node_id);
        }
        let action = gossip_table.new_complete_data(&data_id, None);
        assert!(action.is_none());
        check_holders(&node_ids[..0], &gossip_table, &data_id);

        // Time the finished data out, then check same complete data causes `ShouldGossip` to be
        // returned as per a completely new entry.
        Instant::advance_time(DEFAULT_FINISHED_ENTRY_DURATION_SECS * 1_000);
        let action = gossip_table.new_complete_data(&data_id, Some(node_ids[0]));
        let expected = Some(ShouldGossip {
            count: EXPECTED_DEFAULT_INFECTION_TARGET,
            exclude_peers: node_ids[..1].iter().copied().collect(),
        });
        assert_eq!(expected, action);
        check_holders(&node_ids[..1], &gossip_table, &data_id);
    }

    #[test]
    fn should_terminate_via_infection_limit() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());

        // Add new complete data from us and check two infections doesn't cause us to stop
        // gossiping.
        let _ = gossip_table.new_complete_data(&data_id, None);
        let limit = EXPECTED_DEFAULT_INFECTION_TARGET - 1;
        for (index, node_id) in node_ids.iter().enumerate().take(limit) {
            let action = gossip_table.we_infected(&data_id, *node_id);
            let expected = GossipAction::ShouldGossip(ShouldGossip {
                count: 1,
                exclude_peers: node_ids[..(index + 1)].iter().copied().collect(),
            });
            assert_eq!(expected, action);
        }

        // Check recording an infection from an already-recorded infectee doesn't cause us to stop
        // gossiping.
        let action = gossip_table.we_infected(&data_id, node_ids[limit - 1]);
        let expected = GossipAction::ShouldGossip(ShouldGossip {
            count: 1,
            exclude_peers: node_ids[..limit].iter().copied().collect(),
        });
        assert_eq!(expected, action);

        // Check third new infection does cause us to stop gossiping.
        let action = gossip_table.we_infected(&data_id, node_ids[limit]);
        assert_eq!(GossipAction::Noop, action);
    }

    #[test]
    fn should_terminate_via_saturation() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());

        // Add new complete data with 14 non-infections and check this doesn't cause us to stop
        // gossiping.
        let _ = gossip_table.new_complete_data(&data_id, None);
        let limit = EXPECTED_DEFAULT_HOLDERS_LIMIT - 1;
        for (index, node_id) in node_ids.iter().enumerate().take(limit) {
            let action = gossip_table.already_infected(&data_id, *node_id);
            let expected = GossipAction::ShouldGossip(ShouldGossip {
                count: 1,
                exclude_peers: node_ids[..(index + 1)].iter().copied().collect(),
            });
            assert_eq!(expected, action);
        }

        // Check recording a non-infection from an already-recorded holder doesn't cause us to stop
        // gossiping.
        let action = gossip_table.already_infected(&data_id, node_ids[0]);
        let expected = GossipAction::ShouldGossip(ShouldGossip {
            count: 1,
            exclude_peers: node_ids[..limit].iter().copied().collect(),
        });
        assert_eq!(expected, action);

        // Check 15th non-infection does cause us to stop gossiping.
        let action = gossip_table.we_infected(&data_id, node_ids[limit]);
        assert_eq!(GossipAction::Noop, action);
    }

    #[test]
    fn should_not_terminate_below_infection_limit_and_saturation() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());

        // Add new complete data with 2 infections and 11 non-infections.
        let _ = gossip_table.new_complete_data(&data_id, None);
        let infection_limit = EXPECTED_DEFAULT_INFECTION_TARGET - 1;
        for node_id in &node_ids[0..infection_limit] {
            let _ = gossip_table.we_infected(&data_id, *node_id);
        }

        let holders_limit = EXPECTED_DEFAULT_HOLDERS_LIMIT - 2;
        for node_id in &node_ids[infection_limit..holders_limit] {
            let _ = gossip_table.already_infected(&data_id, *node_id);
        }

        // Check adding 12th non-infection doesn't cause us to stop gossiping.
        let action = gossip_table.already_infected(&data_id, node_ids[holders_limit]);
        let expected = GossipAction::ShouldGossip(ShouldGossip {
            count: 1,
            exclude_peers: node_ids[..(holders_limit + 1)].iter().copied().collect(),
        });
        assert_eq!(expected, action);
    }

    #[test]
    fn check_timeout_should_detect_holder() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());

        // Add new complete data and get a response from node 0 only.
        let _ = gossip_table.new_complete_data(&data_id, None);
        let _ = gossip_table.we_infected(&data_id, node_ids[0]);

        // check_timeout for node 0 should return Noop, and for node 1 it should represent a timed
        // out response and return ShouldGossip.
        let action = gossip_table.check_timeout(&data_id, node_ids[0]);
        assert_eq!(GossipAction::Noop, action);

        let action = gossip_table.check_timeout(&data_id, node_ids[1]);
        let expected = GossipAction::ShouldGossip(ShouldGossip {
            count: 1,
            exclude_peers: iter::once(node_ids[0]).collect(),
        });
        assert_eq!(expected, action);
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "shouldn't check timeout for a gossip response for partial data")
    )]
    fn check_timeout_should_panic_for_partial_copy() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());
        let _ = gossip_table.new_partial_data(&data_id, node_ids[0]);
        let _ = gossip_table.check_timeout(&data_id, node_ids[0]);
    }

    #[test]
    fn should_remove_holder_if_unresponsive() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());

        // Add new partial data from nodes 0 and 1.
        let _ = gossip_table.new_partial_data(&data_id, node_ids[0]);
        let _ = gossip_table.new_partial_data(&data_id, node_ids[1]);

        // Node 0 should be removed from the holders since it hasn't provided us with the full data,
        // and we should be told to get the remainder from node 1.
        let action = gossip_table.remove_holder_if_unresponsive(&data_id, node_ids[0]);
        let expected = GossipAction::GetRemainder {
            holder: node_ids[1],
        };
        assert_eq!(expected, action);
        check_holders(&node_ids[1..2], &gossip_table, &data_id);

        // Node 1 should be removed from the holders since it hasn't provided us with the full data,
        // and the entry should be removed since there are no more holders.
        let action = gossip_table.remove_holder_if_unresponsive(&data_id, node_ids[1]);
        assert_eq!(GossipAction::Noop, action);
        check_holders(&node_ids[..0], &gossip_table, &data_id);
        assert!(!gossip_table.current.contains_key(&data_id));
        assert!(!gossip_table.paused.contains_key(&data_id));

        // Add new partial data from node 2 and check gossiping has been resumed.
        let action = gossip_table.new_partial_data(&data_id, node_ids[2]);
        let expected = GossipAction::GetRemainder {
            holder: node_ids[2],
        };
        assert_eq!(expected, action);
        check_holders(&node_ids[2..3], &gossip_table, &data_id);

        // Node 2 should be removed from the holders since it hasn't provided us with the full data,
        // and the entry should be paused since there are no more holders.
        let action = gossip_table.remove_holder_if_unresponsive(&data_id, node_ids[2]);
        assert_eq!(GossipAction::Noop, action);
        check_holders(&node_ids[..0], &gossip_table, &data_id);
        assert!(!gossip_table.current.contains_key(&data_id));
        assert!(!gossip_table.paused.contains_key(&data_id));

        // Add new complete data from node 3 and check gossiping has been resumed.
        let action = gossip_table.new_complete_data(&data_id, Some(node_ids[3]));
        let expected = Some(ShouldGossip {
            count: EXPECTED_DEFAULT_INFECTION_TARGET,
            exclude_peers: iter::once(node_ids[3]).collect(),
        });
        assert_eq!(expected, action);
        check_holders(&node_ids[3..4], &gossip_table, &data_id);
    }

    #[test]
    fn should_not_remove_holder_if_responsive() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());

        // Add new partial data from node 0 and record that we have received the full data from it.
        let _ = gossip_table.new_partial_data(&data_id, node_ids[0]);
        let _ = gossip_table.new_complete_data(&data_id, Some(node_ids[0]));

        // Node 0 should remain as a holder since we now hold the complete data.
        let action = gossip_table.remove_holder_if_unresponsive(&data_id, node_ids[0]);
        assert_eq!(GossipAction::Noop, action); // Noop as all RPCs are still in-flight
        check_holders(&node_ids[..1], &gossip_table, &data_id);
        assert!(gossip_table.current.contains_key(&data_id));
        assert!(!gossip_table.paused.contains_key(&data_id));
    }

    #[test]
    fn should_not_auto_resume_manually_paused() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());

        // Add new partial data from node 0, manually pause gossiping, then record that node 0
        // failed to provide the full data.
        let _ = gossip_table.new_partial_data(&data_id, node_ids[0]);
        gossip_table.pause(&data_id);
        let action = gossip_table.remove_holder_if_unresponsive(&data_id, node_ids[0]);
        assert_eq!(GossipAction::Noop, action);
        check_holders(&node_ids[..0], &gossip_table, &data_id);

        // Add new partial data from node 1 and check gossiping has not been resumed.
        let action = gossip_table.new_partial_data(&data_id, node_ids[1]);
        assert_eq!(GossipAction::Noop, action);
        check_holders(&node_ids[1..2], &gossip_table, &data_id);
        assert!(!gossip_table.current.contains_key(&data_id));
        assert!(gossip_table.paused.contains_key(&data_id));
    }

    #[test]
    fn should_purge() {
        let node_ids = random_node_ids();

        let mut rng = rand::thread_rng();
        let data_id: u64 = rng.gen();

        let mut gossip_table = GossipTable::new(Config::default());

        // Add new complete data and finish via infection limit.
        let _ = gossip_table.new_complete_data(&data_id, None);
        for node_id in &node_ids[0..EXPECTED_DEFAULT_INFECTION_TARGET] {
            let _ = gossip_table.we_infected(&data_id, *node_id);
        }
        assert!(gossip_table.finished.contains_key(&data_id));

        // Time the finished data out and check it has been purged.
        Instant::advance_time(DEFAULT_FINISHED_ENTRY_DURATION_SECS * 1_000);
        gossip_table.purge_finished();
        assert!(!gossip_table.finished.contains_key(&data_id));

        // Add new complete data and pause.
        let _ = gossip_table.new_complete_data(&data_id, None);
        gossip_table.pause(&data_id);
        assert!(gossip_table.paused.contains_key(&data_id));

        // Time the paused data out and check it has been purged.
        Instant::advance_time(DEFAULT_FINISHED_ENTRY_DURATION_SECS * 1_000);
        gossip_table.purge_finished();
        assert!(!gossip_table.paused.contains_key(&data_id));
    }
}
