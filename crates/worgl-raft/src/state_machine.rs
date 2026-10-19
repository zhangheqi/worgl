//! The issuer as a Raft state machine, with snapshots in the mint's
//! `snapshot.bin` and `keys.bin`.
//!
//! Entries are applied in memory only; what reaches the disk is the
//! snapshot, written whenever openraft builds or installs one. A replica that
//! restarts restores the snapshot and applies the log's later entries again
//! as they commit. `keys.bin` always holds every key the snapshot on disk
//! needs: a new snapshot is written between two rewrites of it, the first
//! adding the new snapshot's keys and the second dropping the old one's.

use std::sync::{Arc, Mutex, RwLock};

use openraft::storage::RaftStateMachine;
use openraft::{
    AnyError, BasicNode, EntryPayload, Membership, RaftSnapshotBuilder, Snapshot, SnapshotMeta,
    StorageError, StorageIOError, StoredMembership,
};
use tokio::sync::{mpsc, oneshot, watch};
use worgl::{
    Event, Issuer, IssuerKey, Kek, KekError, KeyRing, Policy, SealedKey, SnapshotFile, StateFiles,
    StoreError, Verifier,
};

use crate::log_store::{from_id, to_id};
use crate::{NodeId, Request, Response, SnapshotData, TypeConfig, blocking};

/// What a replica shares between its state machine and its handle.
#[derive(Debug)]
pub(crate) struct Shared {
    /// The issuer as of the last entry applied.
    pub(crate) issuer: RwLock<Issuer>,
    /// A verifier for the issuer's keys, replaced after each rotation.
    pub(crate) verifier: watch::Sender<Verifier>,
}

/// A replica's issuer, applying committed entries, building and installing
/// snapshots.
///
/// Entries are applied in memory only; what reaches the disk is the
/// snapshot, whenever openraft builds or installs one. A replica that
/// restarts restores the snapshot and applies the log's later entries again
/// as they commit.
pub struct StateMachine {
    shared: Arc<Shared>,
    disk: Arc<Mutex<Disk>>,
    applied: Option<openraft::LogId<NodeId>>,
    membership: StoredMembership<NodeId, BasicNode>,
    key: IssuerKey,
    policy: Policy,
    kek: Arc<dyn Kek>,
    /// Asks for a snapshot, so the keys of dropped generations leave the
    /// disk soon after a rotation.
    snapshots: mpsc::UnboundedSender<()>,
}

/// The snapshot on disk and the key ring that goes with it.
#[derive(Debug)]
struct Disk {
    files: StateFiles,
    /// Dropped with the directory's lock, to tell whoever waits for it.
    _released: oneshot::Sender<()>,
    /// What `keys.bin` holds.
    saved: KeyRing,
    /// The snapshot in `snapshot.bin`, if it holds any entry, with its keys
    /// sealed for sending.
    current: Option<(SnapshotMeta<NodeId, BasicNode>, Vec<SealedKey>)>,
}

impl StateMachine {
    /// Restores the issuer from the snapshot in the directory `files`
    /// locked. The receiver it returns closes once the lock is released.
    #[allow(clippy::type_complexity)]
    pub(crate) fn open(
        files: StateFiles,
        key: IssuerKey,
        policy: Policy,
        kek: Arc<dyn Kek>,
        snapshots: mpsc::UnboundedSender<()>,
    ) -> Result<(Self, Arc<Shared>, oneshot::Receiver<()>), StoreError> {
        let saved = files.keys(&key)?;
        let snapshot = files.snapshot()?;
        let issuer = Issuer::restore(key.clone(), saved.clone(), &snapshot.issuer, policy.clone())?;
        let applied = snapshot.applied.map(from_id);
        let membership = decode_membership(snapshot.membership.as_ref())?;
        let current = match applied {
            Some(_) => {
                let sealed = issuer
                    .seal_keys(&*kek)
                    .map_err(|error| StoreError::Io(std::io::Error::other(error)))?;
                Some((meta(applied, membership.clone()), sealed))
            }
            None => None,
        };
        let (released, on_release) = oneshot::channel();
        let shared = Arc::new(Shared {
            verifier: watch::Sender::new(issuer.verifier()),
            issuer: RwLock::new(issuer),
        });
        let machine = Self {
            shared: shared.clone(),
            disk: Arc::new(Mutex::new(Disk {
                files,
                _released: released,
                saved,
                current,
            })),
            applied,
            membership,
            key,
            policy,
            kek,
            snapshots,
        };
        Ok((machine, shared, on_release))
    }

    /// The id of the last entry applied.
    pub(crate) fn applied(&self) -> Option<openraft::LogId<NodeId>> {
        self.applied
    }

    fn write_issuer(&self) -> std::sync::RwLockWriteGuard<'_, Issuer> {
        self.shared
            .issuer
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Applies a batch of events onto `issuer`, unsealing the keys its rotations
/// make first, and returns which events applied.
///
/// Nothing applies unless the batch starts at the issuer's next sequence
/// number. An event that does not apply changes nothing, and the events
/// after it are still tried, so every replica ends in the same state.
pub(crate) fn apply_request(
    issuer: &mut Issuer,
    request: &Request,
    kek: &dyn Kek,
) -> Result<Vec<bool>, (u64, KekError)> {
    if request.first != issuer.sequence() + 1 {
        return Ok(vec![false; request.events.len()]);
    }
    for sealed in &request.keys {
        let key = sealed
            .unseal(kek, issuer.public_key())
            .map_err(|error| (sealed.generation, error))?;
        issuer.add_key(key);
    }
    let applied = request
        .events
        .iter()
        .map(|event| issuer.apply(event).is_ok())
        .collect();
    if !request.keys.is_empty() {
        // The key of a rotation that did not apply signs nothing.
        issuer.retire_keys();
    }
    Ok(applied)
}

impl RaftStateMachine<TypeConfig> for StateMachine {
    type SnapshotBuilder = SnapshotBuilder;

    async fn applied_state(
        &mut self,
    ) -> Result<
        (
            Option<openraft::LogId<NodeId>>,
            StoredMembership<NodeId, BasicNode>,
        ),
        StorageError<NodeId>,
    > {
        Ok((self.applied, self.membership.clone()))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<Response>, StorageError<NodeId>>
    where
        I: IntoIterator<Item = openraft::Entry<TypeConfig>> + openraft::OptionalSend,
        I::IntoIter: openraft::OptionalSend,
    {
        let mut responses = Vec::new();
        let mut applied_id = self.applied;
        let mut membership = None;
        let mut rotated = false;
        {
            let mut issuer = self.write_issuer();
            for entry in entries {
                applied_id = Some(entry.log_id);
                let response = match entry.payload {
                    EntryPayload::Blank => Response::default(),
                    EntryPayload::Membership(m) => {
                        membership = Some(StoredMembership::new(Some(entry.log_id), m));
                        Response::default()
                    }
                    EntryPayload::Normal(request) => {
                        let applied = apply_request(&mut issuer, &request, &*self.kek).map_err(
                            |(generation, error)| {
                                StorageIOError::apply(
                                    entry.log_id,
                                    AnyError::error(format!(
                                        "the key of generation {generation} cannot be unsealed: {error}"
                                    )),
                                )
                            },
                        )?;
                        rotated |=
                            request.events.iter().zip(&applied).any(|(event, applied)| {
                                *applied && matches!(event, Event::Rotate(_))
                            });
                        Response { applied }
                    }
                };
                responses.push(response);
            }
            if rotated {
                self.shared.verifier.send_replace(issuer.verifier());
            }
        }
        self.applied = applied_id;
        if let Some(membership) = membership {
            self.membership = membership;
        }
        if rotated {
            // Nobody listens once the replica has shut down.
            let _ = self.snapshots.send(());
        }
        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        let issuer = self
            .shared
            .issuer
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        SnapshotBuilder {
            disk: self.disk.clone(),
            meta: meta(self.applied, self.membership.clone()),
            issuer: issuer.snapshot(),
            ring: issuer.key_ring().clone(),
            sealed: issuer.seal_keys(&*self.kek),
        }
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<SnapshotData>, StorageError<NodeId>> {
        // Snapshots arrive whole, through `Raft::install_full_snapshot`.
        Ok(Box::default())
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<NodeId, BasicNode>,
        snapshot: Box<SnapshotData>,
    ) -> Result<(), StorageError<NodeId>> {
        let SnapshotData { issuer, keys } = *snapshot;
        let refuse = |error: String| {
            StorageError::from(StorageIOError::write_snapshot(
                Some(meta.signature()),
                AnyError::error(error),
            ))
        };
        let mut ring = KeyRing::new(self.key.public_key());
        for sealed in &keys {
            let key = sealed
                .unseal(&*self.kek, &self.key.public_key())
                .map_err(|error| {
                    refuse(format!(
                        "the key of generation {} cannot be unsealed: {error}",
                        sealed.generation
                    ))
                })?;
            ring.insert(key);
        }
        let restored =
            Issuer::restore(self.key.clone(), ring.clone(), &issuer, self.policy.clone())
                .map_err(|error| refuse(format!("the snapshot does not restore: {error}")))?;
        let file = SnapshotFile {
            applied: meta.last_log_id.as_ref().map(to_id),
            membership: encode_membership(&meta.last_membership)
                .map_err(|e| refuse(e.to_string()))?,
            issuer,
        };
        blocking(|| {
            let mut disk = self
                .disk
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            disk.save(meta.clone(), &file, &ring, keys)
        })
        .map_err(|e| StorageIOError::write_snapshot(Some(meta.signature()), &e))?;
        let verifier = restored.verifier();
        *self.write_issuer() = restored;
        self.shared.verifier.send_replace(verifier);
        self.applied = meta.last_log_id;
        self.membership = meta.last_membership.clone();
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        let disk = self
            .disk
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some((meta, keys)) = disk.current.clone() else {
            return Ok(None);
        };
        let file = blocking(|| disk.files.snapshot()).map_err(|e| {
            StorageIOError::read_snapshot(Some(meta.signature()), AnyError::error(e.to_string()))
        })?;
        Ok(Some(Snapshot {
            meta,
            snapshot: Box::new(SnapshotData {
                issuer: file.issuer,
                keys,
            }),
        }))
    }
}

/// The state as of one entry, taken when openraft asks for a snapshot and
/// written while the state machine goes on applying entries.
pub struct SnapshotBuilder {
    disk: Arc<Mutex<Disk>>,
    meta: SnapshotMeta<NodeId, BasicNode>,
    issuer: Vec<u8>,
    ring: KeyRing,
    sealed: Result<Vec<SealedKey>, KekError>,
}

impl RaftSnapshotBuilder<TypeConfig> for SnapshotBuilder {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        let signature = Some(self.meta.signature());
        let keys = self
            .sealed
            .clone()
            .map_err(|e| StorageIOError::write_snapshot(signature.clone(), &e))?;
        let file = SnapshotFile {
            applied: self.meta.last_log_id.as_ref().map(to_id),
            membership: encode_membership(&self.meta.last_membership)
                .map_err(|e| StorageIOError::write_snapshot(signature.clone(), &e))?,
            issuer: std::mem::take(&mut self.issuer),
        };
        blocking(|| {
            let mut disk = self
                .disk
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            disk.save(self.meta.clone(), &file, &self.ring, keys.clone())
        })
        .map_err(|e| StorageIOError::write_snapshot(signature, &e))?;
        Ok(Snapshot {
            meta: self.meta.clone(),
            snapshot: Box::new(SnapshotData {
                issuer: file.issuer,
                keys,
            }),
        })
    }
}

impl Disk {
    /// Makes `file` the snapshot on disk, with `ring` its keys, unless the
    /// snapshot there already holds as much.
    fn save(
        &mut self,
        meta: SnapshotMeta<NodeId, BasicNode>,
        file: &SnapshotFile,
        ring: &KeyRing,
        sealed: Vec<SealedKey>,
    ) -> std::io::Result<()> {
        if let Some((current, _)) = &self.current
            && current.last_log_id >= meta.last_log_id
        {
            return Ok(());
        }
        // Both snapshots' keys, until the new one is in place.
        let mut both = self.saved.clone();
        both.extend(ring);
        self.files.save_keys(&both)?;
        self.files.save_snapshot(file)?;
        self.files.save_keys(ring)?;
        self.saved = ring.clone();
        self.current = Some((meta, sealed));
        Ok(())
    }
}

fn meta(
    last_log_id: Option<openraft::LogId<NodeId>>,
    last_membership: StoredMembership<NodeId, BasicNode>,
) -> SnapshotMeta<NodeId, BasicNode> {
    let snapshot_id = match &last_log_id {
        Some(id) => format!(
            "{}-{}-{}",
            id.leader_id.term, id.leader_id.node_id, id.index
        ),
        None => "empty".into(),
    };
    SnapshotMeta {
        last_log_id,
        last_membership,
        snapshot_id,
    }
}

fn encode_membership(
    membership: &StoredMembership<NodeId, BasicNode>,
) -> Result<Option<(worgl::LogId, Vec<u8>)>, serde_json::Error> {
    match membership.log_id() {
        Some(id) => Ok(Some((
            to_id(id),
            serde_json::to_vec(membership.membership())?,
        ))),
        None => Ok(None),
    }
}

fn decode_membership(
    membership: Option<&(worgl::LogId, Vec<u8>)>,
) -> Result<StoredMembership<NodeId, BasicNode>, StoreError> {
    match membership {
        Some((id, bytes)) => {
            let membership: Membership<NodeId, BasicNode> =
                serde_json::from_slice(bytes).map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("the snapshot's membership is unreadable: {error}"),
                    )
                })?;
            Ok(StoredMembership::new(Some(from_id(*id)), membership))
        }
        None => Ok(StoredMembership::default()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use openraft::CommittedLeaderId;
    use worgl::{
        FileKek, FileStore, GenerationKey, HolderKey, LedgerConfig, Output, Plan, TransferRequest,
    };

    use super::*;

    const LEDGER: LedgerConfig = LedgerConfig {
        layers: 4,
        layer_size: 64,
    };

    fn issuer() -> Issuer {
        Issuer::new(IssuerKey::generate(), LEDGER, Policy::cash_like()).unwrap()
    }

    /// Plans a rotation on `leader` as a replica's batch does, with its key
    /// sealed.
    fn rotation(leader: &mut Issuer, kek: &dyn Kek) -> (Event, SealedKey) {
        let next = GenerationKey::generate();
        let generation = leader.ledger().newest_generation() + 1;
        let sealed = SealedKey::seal(kek, leader.public_key(), generation, &next).unwrap();
        let event = leader.plan_rotate(next).unwrap();
        leader.apply(&event).unwrap();
        (event, sealed)
    }

    #[test]
    fn a_batch_applies_only_at_the_issuers_next_sequence_number() {
        let kek = FileKek::from_bytes(&[1; 32]);
        let mut replica = issuer();
        let event = replica
            .plan_mint(8, HolderKey::generate().public_key())
            .unwrap();
        let late = Request {
            first: replica.sequence() + 2,
            events: vec![event.clone()],
            keys: Vec::new(),
        };
        assert_eq!(apply_request(&mut replica, &late, &kek).unwrap(), [false]);
        assert_eq!(replica.sequence(), 0);
        let next = Request { first: 1, ..late };
        assert_eq!(apply_request(&mut replica, &next, &kek).unwrap(), [true]);
        assert_eq!(replica.sequence(), 1);
        // The same batch again is stale now.
        assert_eq!(apply_request(&mut replica, &next, &kek).unwrap(), [false]);
    }

    #[test]
    fn a_rotations_key_is_unsealed_from_its_entry() {
        let kek = FileKek::from_bytes(&[1; 32]);
        let mut follower = issuer();
        let mut leader = follower.clone();
        let (event, sealed) = rotation(&mut leader, &kek);
        let request = Request {
            first: 1,
            events: vec![event],
            keys: vec![sealed.clone()],
        };
        let wrong = FileKek::from_bytes(&[2; 32]);
        assert_eq!(
            apply_request(&mut follower.clone(), &request, &wrong).unwrap_err(),
            (sealed.generation, KekError::Rejected)
        );
        assert_eq!(
            apply_request(&mut follower, &request, &kek).unwrap(),
            [true]
        );
        assert_eq!(follower.snapshot(), leader.snapshot());
        // The follower signs the new generation's notes as the leader does:
        // a payment lands one layer on from the mint layer, in it.
        let holder = HolderKey::generate();
        let note = leader.mint(8, holder.public_key()).unwrap();
        let output = Output {
            amount: 8,
            holder: HolderKey::generate().public_key(),
        };
        let request = TransferRequest::single(note, &holder, output).unwrap();
        let verified = leader.verifier().verify(&request).unwrap();
        let Plan::Apply(payment) = leader.plan_transfer(&verified).unwrap() else {
            panic!("a fresh transfer")
        };
        let paid = leader.notes(&payment);
        assert_eq!(
            paid[0].serial().generation,
            leader.ledger().newest_generation()
        );
        assert_eq!(follower.notes(&payment), paid);
    }

    #[test]
    fn a_rotation_that_does_not_apply_leaves_no_key() {
        let kek = FileKek::from_bytes(&[1; 32]);
        let mut follower = issuer();
        let mut leader = follower.clone();
        let keys = follower.key_ring().len();
        let (event, sealed) = rotation(&mut leader, &kek);
        // Paused first, the follower refuses the rotation.
        let request = Request {
            first: 1,
            events: vec![Event::Pause, event],
            keys: vec![sealed],
        };
        assert_eq!(
            apply_request(&mut follower, &request, &kek).unwrap(),
            [true, false]
        );
        assert!(follower.is_paused());
        assert_eq!(follower.key_ring().len(), keys);
    }

    fn seed(dirs: &[&Path], key: &IssuerKey) -> Issuer {
        let issuer = Issuer::new(key.clone(), LEDGER, Policy::cash_like()).unwrap();
        drop(FileStore::create(dirs[0], &issuer).unwrap());
        for dir in &dirs[1..] {
            for name in ["snapshot.bin", "keys.bin", "log.bin"] {
                std::fs::copy(dirs[0].join(name), dir.join(name)).unwrap();
            }
        }
        issuer
    }

    fn open(dir: &Path, key: &IssuerKey, kek: &Arc<FileKek>) -> (StateMachine, Arc<Shared>) {
        let files = StateFiles::open(dir).unwrap();
        let (wanted, _) = mpsc::unbounded_channel();
        let (machine, shared, _) =
            StateMachine::open(files, key.clone(), Policy::cash_like(), kek.clone(), wanted)
                .unwrap();
        (machine, shared)
    }

    fn id(term: u64, index: u64) -> openraft::LogId<NodeId> {
        openraft::LogId::new(CommittedLeaderId::new(term, 1), index)
    }

    #[tokio::test]
    async fn a_snapshot_built_on_one_replica_installs_on_another() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let key = IssuerKey::generate();
        seed(&[a.path(), b.path()], &key);
        let kek = Arc::new(FileKek::from_bytes(&[5; 32]));
        let (mut first, shared) = open(a.path(), &key, &kek);
        let mut planner = shared.issuer.read().unwrap().clone();
        let mint = planner
            .plan_mint(8, HolderKey::generate().public_key())
            .unwrap();
        planner.apply(&mint).unwrap();
        let (rotate, sealed) = rotation(&mut planner, &*kek);
        let members = BTreeMap::from([(1, BasicNode::new("a")), (2, BasicNode::new("b"))]);
        let membership = Membership::new(vec![members.keys().copied().collect()], members);
        let entries = vec![
            openraft::Entry {
                log_id: openraft::LogId::default(),
                payload: EntryPayload::Membership(membership),
            },
            openraft::Entry {
                log_id: id(1, 1),
                payload: EntryPayload::Blank,
            },
            openraft::Entry {
                log_id: id(1, 2),
                payload: EntryPayload::Normal(Request {
                    first: 1,
                    events: vec![mint, rotate],
                    keys: vec![sealed],
                }),
            },
        ];
        let responses = first.apply(entries).await.unwrap();
        assert_eq!(responses[2].applied, [true, true]);
        assert_eq!(shared.issuer.read().unwrap().snapshot(), planner.snapshot());
        assert!(first.get_current_snapshot().await.unwrap().is_none());

        let built = first
            .get_snapshot_builder()
            .await
            .build_snapshot()
            .await
            .unwrap();
        assert_eq!(built.meta.last_log_id, Some(id(1, 2)));
        {
            let disk = first.disk.lock().unwrap();
            let file = disk.files.snapshot().unwrap();
            assert_eq!(file.applied, Some(to_id(&id(1, 2))));
            assert_eq!(file.issuer, planner.snapshot());
            assert_eq!(
                disk.files.keys(&key).unwrap().to_bytes(),
                planner.key_ring().to_bytes()
            );
        }

        // Sent across the network and installed.
        let (mut second, other) = open(b.path(), &key, &kek);
        let data = SnapshotData::from_bytes(&built.snapshot.to_bytes()).unwrap();
        second
            .install_snapshot(&built.meta, Box::new(data.clone()))
            .await
            .unwrap();
        assert_eq!(other.issuer.read().unwrap().snapshot(), planner.snapshot());
        assert_eq!(
            second.applied_state().await.unwrap(),
            first.applied_state().await.unwrap()
        );
        let current = second.get_current_snapshot().await.unwrap().unwrap();
        assert_eq!(current.meta, built.meta);
        assert_eq!(current.snapshot.issuer, planner.snapshot());

        // And it is what the second replica restores after a restart.
        drop((second, other));
        let (mut second, other) = open(b.path(), &key, &kek);
        assert_eq!(second.applied(), Some(id(1, 2)));
        assert_eq!(other.issuer.read().unwrap().snapshot(), planner.snapshot());
        assert_eq!(
            second.applied_state().await.unwrap(),
            first.applied_state().await.unwrap()
        );

        // A replica under another KEK cannot install it.
        let c = tempfile::tempdir().unwrap();
        for name in ["snapshot.bin", "keys.bin", "log.bin"] {
            std::fs::copy(b.path().join(name), c.path().join(name)).unwrap();
        }
        let (mut third, _) = open(c.path(), &key, &Arc::new(FileKek::from_bytes(&[6; 32])));
        assert!(
            third
                .install_snapshot(&built.meta, Box::new(data))
                .await
                .is_err()
        );
    }
}
