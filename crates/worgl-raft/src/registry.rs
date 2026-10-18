//! A cluster's registry: which mints exist, each with a record its creator
//! wrote, kept by a Raft group of every member so that a mint created on one
//! member is known to all of them.
//!
//! A registry lives in a directory of its own:
//!
//! - `log.json`, the Raft log not yet covered by the snapshot,
//! - `snapshot.json`, the records as of the last entry applied, with the
//!   membership as of then,
//! - `vote.json`, the member's Raft vote, and
//! - `lock`, held while the registry is open.
//!
//! Mints are created rarely, so every change rewrites the file it touches
//! whole, through a temporary file and a rename, and the records are saved
//! after every entry applied. A registry that restarts knows every mint it
//! had applied before it hears from the others.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Debug;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::ops::RangeBounds;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use openraft::error::{ClientWriteError, InitializeError, RaftError};
use openraft::impls::OneshotResponder;
use openraft::storage::{LogFlushed, RaftLogStorage, RaftStateMachine};
use openraft::{
    BasicNode, Entry, EntryPayload, LogId, LogState, RaftLogReader, RaftNetworkFactory,
    RaftSnapshotBuilder, ServerState, Snapshot, SnapshotMeta, StorageError, StorageIOError,
    StoredMembership, TokioRuntime, Vote,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};
use worgl::{PublicKey, StoreError};

use crate::log_store::{VOTE, write_atomically};
use crate::{NodeId, OpenError, ReplicaError, blocking};

const LOG: &str = "log.json";
const SNAPSHOT: &str = "snapshot.json";
const LOCK: &str = "lock";

/// How long an insert waits for the group before giving up.
const TIMEOUT: Duration = Duration::from_secs(10);

openraft::declare_raft_types!(
    /// The types a registry's Raft group is built from.
    pub RegistryConfig:
        D = Insert,
        R = Inserted,
        NodeId = NodeId,
        Node = BasicNode,
        Entry = openraft::Entry<RegistryConfig>,
        SnapshotData = Records,
        AsyncRuntime = TokioRuntime,
        Responder = OneshotResponder<RegistryConfig>,
);

/// A registry member's handle on openraft, through which the RPCs it
/// receives reach it.
pub type RegistryRaft = openraft::Raft<RegistryConfig>;

/// Every mint a registry holds, by issuer key, with its record. A snapshot
/// of the registry is its records.
pub type Records = BTreeMap<PublicKey, serde_json::Value>;

/// Adds a mint to the registry, unless it holds that mint already.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Insert {
    /// The mint's issuer key.
    pub mint: PublicKey,
    /// What the registry keeps about it, for its user to read.
    pub record: serde_json::Value,
}

/// What applying an entry did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inserted {
    /// Whether the entry added a mint.
    pub added: bool,
}

/// One member's copy of the registry. See the [module documentation](self).
pub struct Registry {
    id: NodeId,
    raft: RegistryRaft,
    records: watch::Receiver<Records>,
    /// Closes once the state machine has released the directory.
    released: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
}

impl Registry {
    /// Whether the registry in `dir` has never been part of a group: it
    /// keeps a vote from the first election it takes part in. Its group
    /// still has to be formed, with [`initialize`](Self::initialize) on one
    /// member once it is open.
    pub fn is_new(dir: impl AsRef<Path>) -> bool {
        !dir.as_ref().join(VOTE).exists()
    }

    /// Opens the registry in `dir`, creating an empty one if there is none,
    /// as member `node`, and starts its Raft node, which sends its RPCs
    /// through `network`.
    pub async fn open<N>(
        node: NodeId,
        dir: impl Into<PathBuf>,
        config: openraft::Config,
        network: N,
    ) -> Result<Self, OpenError>
    where
        N: RaftNetworkFactory<RegistryConfig>,
    {
        let dir = dir.into();
        let config = config
            .validate()
            .map_err(|error| OpenError::Config(Box::new(error)))?;
        fs::create_dir_all(&dir).map_err(StoreError::from)?;
        let lock = lock(&dir)?;
        let (released, on_release) = oneshot::channel();
        let log = Log::open(&dir)?;
        let machine = Machine::open(&dir, lock, released)?;
        if let Some(purged) = log.purged()
            && machine
                .state
                .applied
                .is_none_or(|applied| purged.index > applied.index)
        {
            return Err(StoreError::Corrupt {
                offset: 0,
                reason: format!(
                    "registry entry {} was purged but {} lacks it",
                    purged.index,
                    dir.join(SNAPSHOT).display()
                ),
            }
            .into());
        }
        let records = machine.records.subscribe();
        let raft = RegistryRaft::new(node, Arc::new(config), network, log, machine)
            .await
            .map_err(|fatal| OpenError::Raft(Box::new(fatal)))?;
        Ok(Self {
            id: node,
            raft,
            records,
            released: tokio::sync::Mutex::new(Some(on_release)),
        })
    }

    /// Forms the group with `members`, which must include this member, and
    /// starts an election. Call it on one member of a new group only.
    pub async fn initialize(
        &self,
        members: BTreeMap<NodeId, BasicNode>,
    ) -> Result<(), Box<RaftError<NodeId, InitializeError<NodeId, BasicNode>>>> {
        self.raft.initialize(members).await.map_err(Box::new)
    }

    /// The member's Raft node: give it the RPCs the other members send.
    pub fn raft(&self) -> &RegistryRaft {
        &self.raft
    }

    /// The records as this member has applied them, which change as it
    /// applies more.
    pub fn records(&self) -> watch::Receiver<Records> {
        self.records.clone()
    }

    /// Whether this member leads the registry's group, as far as it knows.
    pub fn is_leader(&self) -> bool {
        self.raft.metrics().borrow().state == ServerState::Leader
    }

    /// The leader this member knows of, which may be itself.
    pub fn leader(&self) -> Option<NodeId> {
        self.raft.metrics().borrow().current_leader
    }

    /// Adds `mint` with `record`, if this member leads, once a majority of
    /// the members hold the entry. Returns whether the mint was added: a
    /// registry that holds it already keeps the record it has.
    pub async fn insert(
        &self,
        mint: PublicKey,
        record: serde_json::Value,
    ) -> Result<bool, ReplicaError> {
        {
            let metrics = self.raft.metrics();
            let metrics = metrics.borrow();
            if metrics.state != ServerState::Leader {
                let leader = metrics.current_leader.filter(|&leader| leader != self.id);
                let node = leader.and_then(|leader| {
                    metrics
                        .membership_config
                        .membership()
                        .get_node(&leader)
                        .cloned()
                });
                return Err(ReplicaError::NotLeader { leader, node });
            }
        }
        let written =
            tokio::time::timeout(TIMEOUT, self.raft.client_write(Insert { mint, record })).await;
        match written {
            Err(_) => Err(ReplicaError::Timeout),
            Ok(Ok(response)) => Ok(response.data.added),
            Ok(Err(RaftError::APIError(ClientWriteError::ForwardToLeader(forward)))) => {
                Err(ReplicaError::NotLeader {
                    leader: forward.leader_id,
                    node: forward.leader_node,
                })
            }
            Ok(Err(RaftError::APIError(error))) => Err(ReplicaError::Stopped(error.to_string())),
            Ok(Err(RaftError::Fatal(fatal))) => Err(ReplicaError::Stopped(fatal.to_string())),
        }
    }

    /// Stops the Raft node and waits until the directory is unlocked, so it
    /// can be opened again.
    pub async fn shutdown(&self) -> Result<(), ReplicaError> {
        self.raft
            .shutdown()
            .await
            .map_err(|error| ReplicaError::Stopped(error.to_string()))?;
        if let Some(released) = self.released.lock().await.take() {
            // Closed, never sent on, once the state machine is gone.
            let _ = released.await;
        }
        Ok(())
    }
}

impl Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

fn lock(dir: &Path) -> Result<File, StoreError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(StoreError::Locked),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Reads the JSON file `name` in `dir`, or `None` if there is none.
fn read_json<T: DeserializeOwned>(dir: &Path, name: &str) -> Result<Option<T>, StoreError> {
    match fs::read(dir.join(name)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is unreadable: {error}", dir.join(name).display()),
            )
            .into()
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_json(dir: &Path, name: &str, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    blocking(|| write_atomically(dir, name, &bytes))
}

/// What `log.json` holds.
#[derive(Debug, Default, Serialize, Deserialize)]
struct LogFile {
    /// The last entry dropped from the front of the log.
    purged: Option<LogId<NodeId>>,
    /// The entries after it, in order.
    entries: VecDeque<Entry<RegistryConfig>>,
}

/// The registry's log and the member's vote. Clones share them.
#[derive(Debug, Clone)]
struct Log {
    inner: Arc<Mutex<LogInner>>,
}

#[derive(Debug)]
struct LogInner {
    dir: PathBuf,
    file: LogFile,
    vote: Option<Vote<NodeId>>,
}

impl Log {
    fn open(dir: &Path) -> Result<Self, StoreError> {
        let file = read_json(dir, LOG)?.unwrap_or_default();
        let vote = read_json(dir, VOTE)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(LogInner {
                dir: dir.to_owned(),
                file,
                vote,
            })),
        })
    }

    fn purged(&self) -> Option<LogId<NodeId>> {
        self.inner_lock().file.purged
    }

    /// The log, for one call. Every change is written before it is made in
    /// memory, so a panic leaves both as they were.
    fn inner_lock(&self) -> MutexGuard<'_, LogInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Writes the log as `change` leaves it, then keeps the change.
    fn change(&self, change: impl FnOnce(&mut LogFile)) -> io::Result<()> {
        let mut inner = self.inner_lock();
        let mut file = LogFile {
            purged: inner.file.purged,
            entries: inner.file.entries.clone(),
        };
        change(&mut file);
        write_json(&inner.dir, LOG, &file)?;
        inner.file = file;
        Ok(())
    }
}

impl RaftLogReader<RegistryConfig> for Log {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + Send>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<RegistryConfig>>, StorageError<NodeId>> {
        let inner = self.inner_lock();
        Ok(inner
            .file
            .entries
            .iter()
            .filter(|entry| range.contains(&entry.log_id.index))
            .cloned()
            .collect())
    }
}

impl RaftLogStorage<RegistryConfig> for Log {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<RegistryConfig>, StorageError<NodeId>> {
        let inner = self.inner_lock();
        let purged = inner.file.purged;
        let last = inner.file.entries.back().map(|entry| entry.log_id);
        Ok(LogState {
            last_purged_log_id: purged,
            last_log_id: last.or(purged),
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        let mut inner = self.inner_lock();
        write_json(&inner.dir, VOTE, vote).map_err(|e| StorageIOError::write_vote(&e))?;
        inner.vote = Some(*vote);
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        Ok(self.inner_lock().vote)
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<RegistryConfig>,
    ) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<RegistryConfig>> + Send,
        I::IntoIter: Send,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        self.change(|file| file.entries.extend(entries))
            .map_err(|e| StorageIOError::write_logs(&e))?;
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.change(|file| {
            file.entries
                .retain(|entry| entry.log_id.index < log_id.index)
        })
        .map_err(|e| StorageIOError::write_logs(&e).into())
    }

    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.change(|file| {
            file.entries
                .retain(|entry| entry.log_id.index > log_id.index);
            file.purged = Some(log_id);
        })
        .map_err(|e| StorageIOError::write_logs(&e).into())
    }
}

/// What `snapshot.json` holds: the registry as of the last entry applied.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    applied: Option<LogId<NodeId>>,
    membership: StoredMembership<NodeId, BasicNode>,
    records: Records,
}

impl State {
    fn snapshot(&self) -> Snapshot<RegistryConfig> {
        let snapshot_id = match &self.applied {
            Some(id) => format!(
                "{}-{}-{}",
                id.leader_id.term, id.leader_id.node_id, id.index
            ),
            None => "empty".into(),
        };
        Snapshot {
            meta: SnapshotMeta {
                last_log_id: self.applied,
                last_membership: self.membership.clone(),
                snapshot_id,
            },
            snapshot: Box::new(self.records.clone()),
        }
    }
}

/// The registry's records, applying committed entries and saving them
/// after each batch it applies.
struct Machine {
    dir: PathBuf,
    state: State,
    records: watch::Sender<Records>,
    /// The directory's lock, and whoever waits for its release.
    _lock: (File, oneshot::Sender<()>),
}

impl Machine {
    fn open(dir: &Path, lock: File, released: oneshot::Sender<()>) -> Result<Self, StoreError> {
        let state: State = read_json(dir, SNAPSHOT)?.unwrap_or_default();
        let (records, _) = watch::channel(state.records.clone());
        Ok(Self {
            dir: dir.to_owned(),
            state,
            records,
            _lock: (lock, released),
        })
    }

    /// Saves `state` and makes it the registry's.
    fn save(&mut self, state: State) -> io::Result<()> {
        write_json(&self.dir, SNAPSHOT, &state)?;
        if state.records != self.state.records {
            self.records.send_replace(state.records.clone());
        }
        self.state = state;
        Ok(())
    }
}

impl RaftStateMachine<RegistryConfig> for Machine {
    type SnapshotBuilder = State;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<NodeId>>, StoredMembership<NodeId, BasicNode>), StorageError<NodeId>>
    {
        Ok((self.state.applied, self.state.membership.clone()))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<Inserted>, StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<RegistryConfig>> + Send,
        I::IntoIter: Send,
    {
        let mut state = self.state.clone();
        let mut responses = Vec::new();
        for entry in entries {
            state.applied = Some(entry.log_id);
            let mut added = false;
            match entry.payload {
                EntryPayload::Blank => {}
                EntryPayload::Normal(Insert { mint, record }) => {
                    if let std::collections::btree_map::Entry::Vacant(vacant) =
                        state.records.entry(mint)
                    {
                        vacant.insert(record);
                        added = true;
                    }
                }
                EntryPayload::Membership(membership) => {
                    state.membership = StoredMembership::new(Some(entry.log_id), membership);
                }
            }
            responses.push(Inserted { added });
        }
        self.save(state)
            .map_err(|e| StorageIOError::write_state_machine(&e))?;
        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.state.clone()
    }

    async fn begin_receiving_snapshot(&mut self) -> Result<Box<Records>, StorageError<NodeId>> {
        Ok(Box::default())
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<NodeId, BasicNode>,
        snapshot: Box<Records>,
    ) -> Result<(), StorageError<NodeId>> {
        let state = State {
            applied: meta.last_log_id,
            membership: meta.last_membership.clone(),
            records: *snapshot,
        };
        self.save(state)
            .map_err(|e| StorageIOError::write_snapshot(Some(meta.signature()), &e).into())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<RegistryConfig>>, StorageError<NodeId>> {
        Ok(self.state.applied.is_some().then(|| self.state.snapshot()))
    }
}

/// The state is saved after every entry, so a snapshot of it is already on
/// disk.
impl RaftSnapshotBuilder<RegistryConfig> for State {
    async fn build_snapshot(&mut self) -> Result<Snapshot<RegistryConfig>, StorageError<NodeId>> {
        Ok(self.snapshot())
    }
}
