//! A replica of one mint: its Raft node, its issuer and, while it leads,
//! the copy of the issuer it plans changes on.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, RwLockReadGuard};
use std::time::Duration;

use openraft::error::{CheckIsLeaderError, ClientWriteError, Fatal, InitializeError, RaftError};
use openraft::{BasicNode, RaftMetrics, RaftNetworkFactory, ServerState};
use thiserror::Error;
use tokio::sync::{Mutex, MutexGuard, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use worgl::{
    CommitError, Error, Event, GenerationKey, Issuer, IssuerKey, Kek, Note, Plan, Policy,
    PublicKey, RotationReport, SealedKey, StateFiles, StoreError, VerifiedBurn, VerifiedMint,
    VerifiedTransfer, Verifier,
};

use crate::log_store::LogStore;
use crate::state_machine::{Shared, StateMachine};
use crate::{NodeId, Raft, Request, TypeConfig, default_config};

/// What a replica needs to open.
#[derive(Debug, Clone)]
pub struct Options {
    /// The replica's id within its group.
    pub node: NodeId,
    /// The mint's directory on this replica.
    pub dir: PathBuf,
    /// The issuer key.
    pub key: IssuerKey,
    /// The policy the issuer enforces.
    pub policy: Policy,
    /// The group's key-encryption key, which seals the generation keys the
    /// log and snapshots carry.
    pub kek: Arc<dyn Kek>,
    /// Timings and snapshot policy; see [`default_config`].
    pub raft: openraft::Config,
    /// How long a commit or a read waits for the group before giving up.
    pub timeout: Duration,
}

impl Options {
    /// Options with [`default_config`] and a ten-second timeout.
    pub fn new(
        node: NodeId,
        dir: impl Into<PathBuf>,
        key: IssuerKey,
        policy: Policy,
        kek: Arc<dyn Kek>,
    ) -> Self {
        Self {
            node,
            dir: dir.into(),
            key,
            policy,
            kek,
            raft: default_config(),
            timeout: Duration::from_secs(10),
        }
    }
}

/// A replica could not open.
#[derive(Debug, Error)]
pub enum OpenError {
    /// The mint's directory could not be read.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The Raft configuration is invalid.
    #[error("the raft configuration is invalid: {0}")]
    Config(Box<openraft::ConfigError>),
    /// Raft could not start.
    #[error("raft did not start: {0}")]
    Raft(Box<Fatal<NodeId>>),
}

/// A change or read the group did not carry out, or whose outcome is not
/// known.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReplicaError {
    /// This replica does not lead its group. Nothing was changed. Send the
    /// request to the leader, if one is known.
    #[error("this replica does not lead its group")]
    NotLeader {
        /// The leader this replica knows of.
        leader: Option<NodeId>,
        /// Where to reach it.
        node: Option<BasicNode>,
    },
    /// The group did not answer in time, as when a leader is cut off from
    /// the rest of it. A batch may still commit, so send the same requests
    /// again: a resent transfer, burn or authorized mint is carried out
    /// once.
    #[error("the group did not answer in time")]
    Timeout,
    /// The batch reached the log but did not apply as planned, since it was
    /// planned on state the log had moved past. Nothing it returned may be
    /// handed out; the requests can be tried again.
    #[error("the batch was planned on stale state and did not apply")]
    Stale,
    /// The replica has stopped, after a storage failure or a shutdown.
    #[error("the replica has stopped: {0}")]
    Stopped(String),
}

/// A replica of one mint. See the [crate documentation](crate).
///
/// Its Raft node runs on tasks of its own; [`shutdown`](Self::shutdown)
/// stops them and unlocks the directory. Dropping the replica without that
/// leaves the node running for as long as anything else holds its
/// [`raft`](Self::raft) handle.
pub struct Replica {
    id: NodeId,
    raft: Raft,
    shared: Arc<Shared>,
    /// While this replica leads: the issuer with every change it committed
    /// in this term, and the term.
    planner: Mutex<Option<Planner>>,
    kek: Arc<dyn Kek>,
    timeout: Duration,
    heartbeat: Duration,
    /// Asks openraft for the snapshots the state machine wants.
    snapshots: JoinHandle<()>,
    /// Closes once the state machine has released the directory.
    released: Mutex<Option<oneshot::Receiver<()>>>,
}

#[derive(Debug)]
struct Planner {
    issuer: Issuer,
    term: u64,
}

impl Replica {
    /// Opens the replica whose copy of the mint is in `options.dir` and
    /// starts its Raft node, which sends its RPCs through `network`.
    ///
    /// The directory must hold a store in the current format, such as
    /// [`FileStore::create`](worgl::FileStore::create) writes.
    pub async fn open<N>(options: Options, network: N) -> Result<Self, OpenError>
    where
        N: RaftNetworkFactory<TypeConfig>,
    {
        let Options {
            node,
            dir,
            key,
            policy,
            kek,
            raft,
            timeout,
        } = options;
        let config = raft
            .validate()
            .map_err(|error| OpenError::Config(Box::new(error)))?;
        let config = Arc::new(config);
        let heartbeat = Duration::from_millis(config.heartbeat_interval);
        let files = StateFiles::open(&dir)?;
        let public = key.public_key();
        let (wanted, mut snapshots) = mpsc::unbounded_channel();
        let (machine, shared, released) =
            StateMachine::open(files, key, policy, kek.clone(), wanted)?;
        let log = LogStore::open(&dir, &public)?;
        if let Some(purged) = log.purged()
            && machine
                .applied()
                .is_none_or(|applied| purged.index > applied.index)
        {
            return Err(StoreError::Corrupt {
                offset: 0,
                reason: format!(
                    "entry {} was purged but the snapshot lacks it",
                    purged.index
                ),
            }
            .into());
        }
        let raft = Raft::new(node, config, network, log, machine)
            .await
            .map_err(|fatal| OpenError::Raft(Box::new(fatal)))?;
        let handle = raft.clone();
        let snapshots = tokio::spawn(async move {
            while snapshots.recv().await.is_some() {
                // Rotations in quick succession need one snapshot.
                while snapshots.try_recv().is_ok() {}
                if handle.trigger().snapshot().await.is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            id: node,
            raft,
            shared,
            planner: Mutex::new(None),
            kek,
            timeout,
            heartbeat,
            snapshots,
            released: Mutex::new(Some(released)),
        })
    }

    /// Forms the group with `members`, which must include this replica, and
    /// starts an election. Call it on one member of a new group only; every
    /// member's directory must start from the same store.
    pub async fn initialize(
        &self,
        members: BTreeMap<NodeId, BasicNode>,
    ) -> Result<(), Box<RaftError<NodeId, InitializeError<NodeId, BasicNode>>>> {
        self.raft.initialize(members).await.map_err(Box::new)
    }

    /// The replica's id within its group.
    pub fn id(&self) -> NodeId {
        self.id
    }

    /// The replica's Raft node: give it the RPCs other replicas send, and
    /// read its metrics.
    pub fn raft(&self) -> &Raft {
        &self.raft
    }

    /// The issuer key's public half.
    pub fn public_key(&self) -> PublicKey {
        *self.issuer().public_key()
    }

    /// A verifier for the issuer's live keys, which changes after each
    /// rotation this replica applies.
    pub fn verifier(&self) -> watch::Receiver<Verifier> {
        self.shared.verifier.subscribe()
    }

    /// Starts a group of changes, if this replica leads. See [`Batch`].
    ///
    /// Batches run one at a time; this waits for the one before to commit.
    pub async fn batch(&self) -> Result<Batch<'_>, ReplicaError> {
        let mut planner = self.planner.lock().await;
        let term = self.leading()?;
        if planner.as_ref().is_none_or(|p| p.term != term) {
            // A new term: wait for everything committed before it, then
            // plan from there.
            *planner = None;
            self.linearize().await?;
            let term = self.leading()?;
            let issuer = self.issuer().clone();
            *planner = Some(Planner { issuer, term });
        }
        let first = planner
            .as_ref()
            .expect("a planner was just ensured")
            .issuer
            .sequence()
            + 1;
        Ok(Batch {
            replica: self,
            planner,
            first,
            events: Vec::new(),
            keys: Vec::new(),
            bytes: 0,
        })
    }

    /// Reads the issuer once this replica has confirmed it leads and has
    /// applied every change committed before the call.
    pub async fn read<T>(&self, read: impl FnOnce(&Issuer) -> T) -> Result<T, ReplicaError> {
        self.linearize().await?;
        Ok(read(&self.issuer()))
    }

    /// Reads this replica's own issuer, which may trail the leader's.
    pub fn read_local<T>(&self, read: impl FnOnce(&Issuer) -> T) -> T {
        read(&self.issuer())
    }

    /// Stops the Raft node and waits until the directory is unlocked, so it
    /// can be opened again.
    pub async fn shutdown(&self) -> Result<(), ReplicaError> {
        self.snapshots.abort();
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

    fn issuer(&self) -> RwLockReadGuard<'_, Issuer> {
        self.shared
            .issuer
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The current term, if this replica leads in it as far as it knows.
    fn leading(&self) -> Result<u64, ReplicaError> {
        let metrics = self.raft.metrics();
        let metrics = metrics.borrow();
        if metrics.state == ServerState::Leader {
            return Ok(metrics.current_term);
        }
        Err(self.not_leader(&metrics))
    }

    fn not_leader(&self, metrics: &RaftMetrics<NodeId, BasicNode>) -> ReplicaError {
        let leader = metrics.current_leader.filter(|&leader| leader != self.id);
        let node = leader.and_then(|leader| {
            metrics
                .membership_config
                .membership()
                .get_node(&leader)
                .cloned()
        });
        ReplicaError::NotLeader { leader, node }
    }

    /// Confirms this replica leads and waits until it has applied
    /// everything committed so far.
    async fn linearize(&self) -> Result<(), ReplicaError> {
        tokio::time::timeout(self.timeout, self.confirm_and_catch_up())
            .await
            .map_err(|_| ReplicaError::Timeout)?
    }

    async fn confirm_and_catch_up(&self) -> Result<(), ReplicaError> {
        loop {
            match self.raft.ensure_linearizable().await {
                Ok(_) => break,
                Err(RaftError::APIError(CheckIsLeaderError::ForwardToLeader(forward))) => {
                    return Err(ReplicaError::NotLeader {
                        leader: forward.leader_id,
                        node: forward.leader_node,
                    });
                }
                Err(RaftError::APIError(CheckIsLeaderError::QuorumNotEnough(_))) => {
                    // Too few members answered within one heartbeat interval,
                    // which a member busy syncing its disk is enough for. Ask
                    // again while this replica still leads; one cut off from
                    // the group asks until the caller's timeout.
                    self.leading()?;
                    tokio::time::sleep(self.heartbeat).await;
                }
                Err(RaftError::Fatal(fatal)) => {
                    return Err(ReplicaError::Stopped(fatal.to_string()));
                }
            }
        }
        // A leader that restarts takes up its term again without knowing how
        // far it had committed, so openraft's read index can trail what it
        // acknowledged. Everything in a leader's log commits in its term,
        // though, so wait for all of it.
        let last = self.raft.metrics().borrow().last_log_index;
        self.raft
            .wait(None)
            .applied_index_at_least(last, "the leader's log is applied")
            .await
            .map(|_| ())
            .map_err(|error| ReplicaError::Stopped(error.to_string()))
    }
}

impl Drop for Replica {
    fn drop(&mut self) {
        self.snapshots.abort();
    }
}

impl std::fmt::Debug for Replica {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Replica")
            .field("id", &self.id)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Changes planned on the leader that reach the log together, as one entry,
/// when [`commit`](Self::commit)ted.
///
/// Each change is planned and applied to the leader's planning copy of the
/// issuer at once, so later changes in the batch see earlier ones. Notes
/// and reports a batch returns must not leave the process until the commit
/// succeeds. A batch dropped without committing, or whose commit fails,
/// makes the next batch plan afresh from the replica's applied state.
#[derive(Debug)]
pub struct Batch<'a> {
    replica: &'a Replica,
    planner: MutexGuard<'a, Option<Planner>>,
    first: u64,
    events: Vec<Event>,
    keys: Vec<SealedKey>,
    bytes: usize,
}

impl Batch<'_> {
    /// The issuer as the batch has left it so far.
    pub fn issuer(&self) -> &Issuer {
        &self
            .planner
            .as_ref()
            .expect("a batch holds a planner")
            .issuer
    }

    /// Mints a note.
    pub fn mint(&mut self, amount: u64, holder: PublicKey) -> Result<Note, Error> {
        let event = self.planned().plan_mint(amount, holder)?;
        Ok(self.stage(event).remove(0))
    }

    /// Mints at a verified mint authority's request. Resending the
    /// authority's last request returns the same note and adds nothing to
    /// the batch.
    pub fn authorized_mint(&mut self, mint: &VerifiedMint) -> Result<Note, Error> {
        let mut notes = match self.planned().plan_authorized_mint(mint)? {
            Plan::Replay(notes) => notes,
            Plan::Apply(event) => self.stage(event),
        };
        Ok(notes.remove(0))
    }

    /// Carries out a verified transfer. A resent request that was already
    /// applied returns the same notes and adds nothing to the batch.
    pub fn transfer(&mut self, transfer: &VerifiedTransfer) -> Result<Vec<Note>, Error> {
        match self.planned().plan_transfer(transfer)? {
            Plan::Replay(notes) => Ok(notes),
            Plan::Apply(event) => Ok(self.stage(event)),
        }
    }

    /// Carries out a verified burn and returns the value destroyed. A resent
    /// burn that was already applied adds nothing to the batch.
    pub fn burn(&mut self, burn: &VerifiedBurn) -> Result<u128, Error> {
        if let Plan::Apply(event) = self.planned().plan_burn(burn)? {
            self.stage(event);
        }
        Ok(burn.value())
    }

    /// Rotates, with a fresh generation key sealed into the entry for the
    /// other replicas.
    pub fn rotate(&mut self) -> Result<RotationReport, CommitError> {
        self.rotate_due(None)
    }

    /// Rotates for a schedule's period that was due at `due` seconds since
    /// the Unix epoch, which must be after the last one logged. See
    /// [`Issuer::plan_scheduled_rotation`].
    pub fn rotate_scheduled(&mut self, due: u64) -> Result<RotationReport, CommitError> {
        self.rotate_due(Some(due))
    }

    /// Pauses the issuer, or resumes it if `paused` is false. Stages nothing
    /// if it already is. Changes after a pause in the same batch are refused.
    pub fn set_paused(&mut self, paused: bool) {
        if let Some(event) = self.planned().plan_pause(paused) {
            self.stage(event);
        }
    }

    /// Events staged so far.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether nothing has been staged.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Encoded size of the events and sealed keys staged so far. Commit
    /// before it would pass [`MAX_BATCH_BYTES`](worgl::store::MAX_BATCH_BYTES).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Proposes every staged event as one entry and waits until a majority
    /// holds it and this replica has applied it. Only after this succeeds
    /// may the batch's results be handed out. A batch with nothing staged
    /// commits at once.
    pub async fn commit(mut self) -> Result<(), ReplicaError> {
        let events = std::mem::take(&mut self.events);
        if events.is_empty() {
            return Ok(());
        }
        let count = events.len();
        let request = Request {
            first: self.first,
            events,
            keys: std::mem::take(&mut self.keys),
        };
        let replica = self.replica;
        let written =
            tokio::time::timeout(replica.timeout, replica.raft.client_write(request)).await;
        let outcome = match written {
            Err(_) => Err(ReplicaError::Timeout),
            Ok(Ok(response))
                if response.data.applied.len() == count
                    && response.data.applied.iter().all(|&applied| applied) =>
            {
                Ok(())
            }
            Ok(Ok(_)) => Err(ReplicaError::Stale),
            // The entry never reached the log, or a new leader removed it.
            Ok(Err(RaftError::APIError(ClientWriteError::ForwardToLeader(forward)))) => {
                Err(ReplicaError::NotLeader {
                    leader: forward.leader_id,
                    node: forward.leader_node,
                })
            }
            Ok(Err(RaftError::APIError(error))) => Err(ReplicaError::Stopped(error.to_string())),
            Ok(Err(RaftError::Fatal(fatal))) => Err(ReplicaError::Stopped(fatal.to_string())),
        };
        if outcome.is_err() {
            *self.planner = None;
        }
        outcome
    }

    fn planned(&mut self) -> &mut Issuer {
        &mut self
            .planner
            .as_mut()
            .expect("a batch holds a planner")
            .issuer
    }

    fn rotate_due(&mut self, due: Option<u64>) -> Result<RotationReport, CommitError> {
        let next = GenerationKey::try_generate().map_err(io::Error::other)?;
        let kek = self.replica.kek.clone();
        let issuer = self.planned();
        let generation = issuer.ledger().newest_generation() + 1;
        let sealed = SealedKey::seal(&*kek, issuer.public_key(), generation, &next)
            .map_err(io::Error::other)?;
        let event = match due {
            Some(due) => issuer.plan_scheduled_rotation(next, due)?,
            None => issuer.plan_rotate(next)?,
        };
        let Event::Rotate(report) = event else {
            unreachable!("a rotation is planned as one")
        };
        debug_assert_eq!(report.created, generation);
        // The key count once, then generation, public key, length and the
        // sealed bytes, as the log entry frames them.
        if self.keys.is_empty() {
            self.bytes += 4;
        }
        self.bytes += 8 + 32 + 4 + sealed.sealed.len();
        self.keys.push(sealed);
        self.stage(Event::Rotate(report));
        Ok(report)
    }

    fn stage(&mut self, event: Event) -> Vec<Note> {
        let issuer = self.planned();
        issuer
            .apply(&event)
            .expect("an event planned under the same borrow applies");
        let notes = issuer.notes(&event);
        // Each event is framed by a 4-byte length in the entry.
        self.bytes += 4 + event.to_bytes().len();
        self.events.push(event);
        notes
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        if !self.events.is_empty() {
            // The planning copy holds changes the log never will.
            *self.planner = None;
        }
    }
}
