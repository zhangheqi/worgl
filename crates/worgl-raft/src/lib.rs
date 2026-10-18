//! Raft replication for a Worgl issuer: one mint's log kept by a group of
//! replicas, so the mint survives the loss of any minority of them.
//!
//! Each replica keeps its own copy of the mint's directory, in the layout a
//! [`FileStore`](worgl::FileStore) uses (`snapshot.bin`, `log.bin`,
//! `keys.bin` and `lock`), plus `vote.json`, the replica's Raft vote.
//! [openraft] elects one leader per group. The leader plans every change on
//! a copy of the issuer of its own, proposes the events as one log entry and
//! hands out what they produce only once a majority holds the entry on disk
//! and the leader has applied it. Every replica applies the same entries in
//! the same order onto the same snapshot, so all of them agree on every bit
//! of the ledger, and any of them can take over as leader.
//!
//! # Changes and reads
//!
//! [`Replica::batch`] starts a group of changes on the leader, as
//! [`worgl::Batch`] does on a store of its own: each change is planned at
//! once, and [`Batch::commit`] proposes all of them as one entry. An entry
//! names the issuer sequence number its first event takes, and a replica
//! applies it only if that is the next one, so a leader that planned on
//! stale state, such as one that lost and regained the lead while another
//! wrote, changes nothing; [`Batch::commit`] then reports
//! [`ReplicaError::Stale`].
//!
//! [`Replica::read`] reads the issuer once the leader has confirmed it still
//! leads and has applied everything committed before the read, so a read
//! never misses a change that was acknowledged. [`Replica::read_local`]
//! reads a replica's own state, which may trail the leader's.
//!
//! # Keys
//!
//! Every replica is given the issuer key when it opens. Generation keys are
//! made by whichever replica leads at a rotation, so the rotation's entry
//! carries the new key sealed by the group's key-encryption key (see
//! [`worgl::kek`]), and every replica unseals it as it applies the entry. A
//! snapshot sent to a replica that fell too far behind carries the live
//! generations' keys sealed the same way. A replica that cannot unseal a key
//! stops rather than apply the log differently from the others.
//!
//! # Starting a group
//!
//! Copy the mint's directory, whether new from
//! [`FileStore::create`](worgl::FileStore::create) or one a mint ran on
//! alone, to every member, so they all start from the same snapshot and key
//! ring. On each, run [`prepare`], open a [`Replica`] and, if `prepare` said
//! the store had never joined a group, call [`Replica::initialize`] with the
//! whole membership.
//!
//! # Registry
//!
//! A [`registry::Registry`] is a small group of its own that lists which
//! mints exist, each with a record its creator wrote, so that a mint
//! created on one member reaches every member, including one that was away
//! when it was created.
//!
//! # Transport
//!
//! The crate leaves the network to its user: [`Replica::open`] takes any
//! [`RaftNetworkFactory`](openraft::RaftNetworkFactory) for the RPCs a
//! replica sends, and the RPCs it receives go to [`Replica::raft`] (or
//! [`registry::Registry::raft`]). A [`SnapshotData`] travels as
//! [`SnapshotData::to_bytes`], a registry's snapshot as JSON.

mod log_store;
mod prepare;
pub mod registry;
mod replica;
mod state_machine;

use std::fmt;

use openraft::impls::OneshotResponder;
use openraft::{BasicNode, SnapshotPolicy, TokioRuntime};
use serde::{Deserialize, Serialize};
use worgl::{DecodeError, Event, PublicKey, SealedKey};

pub use log_store::LogStore;
pub use prepare::{has_joined, prepare};
pub use replica::{Batch, OpenError, Options, Replica, ReplicaError};
pub use state_machine::{SnapshotBuilder, StateMachine};

/// A replica's id within its group.
pub type NodeId = u64;

/// A replica's handle on openraft, through which the RPCs it receives reach
/// it.
pub type Raft = openraft::Raft<TypeConfig>;

openraft::declare_raft_types!(
    /// The types a mint's Raft group is built from.
    pub TypeConfig:
        D = Request,
        R = Response,
        NodeId = NodeId,
        Node = BasicNode,
        Entry = openraft::Entry<TypeConfig>,
        SnapshotData = SnapshotData,
        AsyncRuntime = TokioRuntime,
        Responder = OneshotResponder<TypeConfig>,
);

/// One log entry's worth of issuer changes, as the leader proposes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// The issuer sequence number the first event takes. A replica whose
    /// next number is another applies none of the events.
    pub first: u64,
    /// The events, at least one, in the order they were planned.
    pub events: Vec<Event>,
    /// The sealed keys of the generations the events' rotations make.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<SealedKey>,
}

/// What applying an entry did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// For an entry of events, whether each one applied. Empty for any
    /// other entry.
    pub applied: Vec<bool>,
}

/// A snapshot as it travels to a replica that fell behind: the issuer's
/// state and the keys of its live generations, sealed. Which log entry it
/// holds, and the membership as of that entry, travel beside it in
/// [`SnapshotMeta`](openraft::SnapshotMeta).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SnapshotData {
    /// The issuer's state, as [`Issuer::snapshot`](worgl::Issuer::snapshot)
    /// encodes it.
    pub issuer: Vec<u8>,
    /// The keys of the live generations, sealed by the group's
    /// key-encryption key.
    pub keys: Vec<SealedKey>,
}

impl SnapshotData {
    /// The encoding: `count: u32 ‖ (generation: u64 ‖ public key ‖
    /// sealed length: u32 ‖ sealed) × count ‖ issuer snapshot`. The issuer
    /// snapshot carries its own checksum and each sealed key its own tag.
    pub fn to_bytes(&self) -> Vec<u8> {
        let keys: usize = self.keys.iter().map(|k| 8 + 32 + 4 + k.sealed.len()).sum();
        let mut bytes = Vec::with_capacity(4 + keys + self.issuer.len());
        bytes.extend_from_slice(&(self.keys.len() as u32).to_be_bytes());
        for key in &self.keys {
            bytes.extend_from_slice(&key.generation.to_be_bytes());
            bytes.extend_from_slice(key.public.as_bytes());
            bytes.extend_from_slice(&(key.sealed.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&key.sealed);
        }
        bytes.extend_from_slice(&self.issuer);
        bytes
    }

    /// Decodes what [`to_bytes`](Self::to_bytes) wrote. The issuer snapshot
    /// is only checked when it is restored.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut rest = bytes;
        let mut take = |n: usize| -> Result<&[u8], DecodeError> {
            if rest.len() < n {
                return Err(DecodeError::Truncated);
            }
            let (head, tail) = rest.split_at(n);
            rest = tail;
            Ok(head)
        };
        let count = u32::from_be_bytes(take(4)?.try_into().expect("4 bytes"));
        let mut keys = Vec::new();
        for _ in 0..count {
            let generation = u64::from_be_bytes(take(8)?.try_into().expect("8 bytes"));
            let public = PublicKey::from_bytes(take(32)?.try_into().expect("32 bytes"))
                .map_err(|_| DecodeError::Invalid("sealed key's public key"))?;
            let len = u32::from_be_bytes(take(4)?.try_into().expect("4 bytes")) as usize;
            let sealed = take(len)?.to_vec();
            keys.push(SealedKey {
                generation,
                public,
                sealed,
            });
        }
        Ok(Self {
            issuer: rest.to_vec(),
            keys,
        })
    }
}

impl fmt::Debug for SnapshotData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotData")
            .field("issuer", &format_args!("{} bytes", self.issuer.len()))
            .field("keys", &self.keys)
            .finish()
    }
}

/// Timings and snapshot policy for a group on one network: heartbeats every
/// 250 ms, an election after 1 to 2 s without one, a minute to send a
/// snapshot, and a snapshot every 10,000 entries (and after every rotation,
/// which [`Replica`] asks for).
pub fn default_config() -> openraft::Config {
    openraft::Config {
        cluster_name: "worgl".into(),
        heartbeat_interval: 250,
        election_timeout_min: 1_000,
        election_timeout_max: 2_000,
        // A snapshot holds the whole ledger, which takes more than one
        // heartbeat to send and install.
        install_snapshot_timeout: 60_000,
        // Entries are group commits of up to 16 MiB; keep RPCs bounded.
        max_payload_entries: 64,
        snapshot_policy: SnapshotPolicy::LogsSinceLast(10_000),
        max_in_snapshot_log_to_keep: 1_000,
        ..openraft::Config::default()
    }
    .validate()
    .expect("the default configuration is valid")
}

/// Runs blocking file work without stalling the tasks that share the
/// thread, where the runtime allows it.
pub(crate) fn blocking<T>(work: impl FnOnce() -> T) -> T {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

#[cfg(test)]
mod tests {
    use worgl::{FileKek, GenerationKey, IssuerKey};

    use super::*;

    #[test]
    fn snapshot_data_round_trips_and_refuses_a_short_input() {
        let kek = FileKek::from_bytes(&[1; 32]);
        let issuer = IssuerKey::generate().public_key();
        let keys = (3..5)
            .map(|generation| {
                SealedKey::seal(&kek, &issuer, generation, &GenerationKey::generate()).unwrap()
            })
            .collect();
        let data = SnapshotData {
            issuer: b"issuer snapshot".to_vec(),
            keys,
        };
        let bytes = data.to_bytes();
        assert_eq!(SnapshotData::from_bytes(&bytes).unwrap(), data);
        assert_eq!(
            SnapshotData::from_bytes(&bytes[..4 + 8 + 32 + 4 + 10]).unwrap_err(),
            DecodeError::Truncated
        );
        assert_eq!(
            SnapshotData::from_bytes(&SnapshotData::default().to_bytes()).unwrap(),
            SnapshotData::default()
        );
        assert!(!format!("{data:?}").contains("issuer snapshot"));
    }

    #[test]
    fn the_default_config_is_valid() {
        let config = default_config();
        assert!(config.election_timeout_min > config.heartbeat_interval * 3);
    }
}
