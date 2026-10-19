//! Three-member groups on an in-process network that can cut replicas off.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openraft::error::{
    Fatal, RPCError, RaftError, RemoteError, ReplicationClosed, StreamingError, Unreachable,
};
use openraft::network::RPCOption;
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, SnapshotResponse, VoteRequest, VoteResponse,
};
use openraft::{
    BasicNode, RaftNetwork, RaftNetworkFactory, ServerState, Snapshot, SnapshotPolicy, Vote,
};
use tempfile::TempDir;
use worgl::{
    FileKek, FileStore, HolderKey, Issuer, IssuerKey, LedgerConfig, Note, NoteStatus, Output,
    PersistentIssuer, Policy, StoreError, TransferRequest,
};
use worgl_raft::{
    NodeId, Options, Raft, Replica, ReplicaError, SnapshotData, TypeConfig, default_config,
};

const LEDGER: LedgerConfig = LedgerConfig {
    layers: 4,
    layer_size: 128,
};
const WAIT: Duration = Duration::from_secs(10);
/// How long a replica waits for its group, unless a test says otherwise.
const TIMEOUT: Duration = Duration::from_secs(2);

/// Delivers RPCs between the replicas of one process.
#[derive(Clone, Default)]
struct Router {
    nodes: Arc<Mutex<BTreeMap<NodeId, Raft>>>,
    /// Replicas that can neither send nor receive.
    isolated: Arc<Mutex<BTreeSet<NodeId>>>,
}

impl Router {
    fn reach(&self, from: NodeId, to: NodeId) -> Result<Raft, Unreachable> {
        let isolated = self.isolated.lock().unwrap();
        if isolated.contains(&from) || isolated.contains(&to) {
            return Err(Unreachable::new(&io::Error::other("cut off")));
        }
        self.nodes
            .lock()
            .unwrap()
            .get(&to)
            .cloned()
            .ok_or_else(|| Unreachable::new(&io::Error::other("down")))
    }
}

struct Network {
    router: Router,
    from: NodeId,
}

impl RaftNetworkFactory<TypeConfig> for Network {
    type Network = Connection;

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> Connection {
        Connection {
            router: self.router.clone(),
            from: self.from,
            target,
        }
    }
}

struct Connection {
    router: Router,
    from: NodeId,
    target: NodeId,
}

impl RaftNetwork<TypeConfig> for Connection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        let raft = self.router.reach(self.from, self.target)?;
        raft.append_entries(rpc)
            .await
            .map_err(|e| RemoteError::new(self.target, e).into())
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        let raft = self.router.reach(self.from, self.target)?;
        raft.vote(rpc)
            .await
            .map_err(|e| RemoteError::new(self.target, e).into())
    }

    async fn full_snapshot(
        &mut self,
        vote: Vote<NodeId>,
        snapshot: Snapshot<TypeConfig>,
        _cancel: impl Future<Output = ReplicationClosed> + Send + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<NodeId>, StreamingError<TypeConfig, Fatal<NodeId>>> {
        let raft = self.router.reach(self.from, self.target)?;
        // As it would cross the network.
        let data = SnapshotData::from_bytes(&snapshot.snapshot.to_bytes()).unwrap();
        let snapshot = Snapshot {
            meta: snapshot.meta,
            snapshot: Box::new(data),
        };
        raft.install_full_snapshot(vote, snapshot)
            .await
            .map_err(|e| RemoteError::new(self.target, e).into())
    }
}

/// A group of three replicas, each in a directory of its own.
struct Cluster {
    router: Router,
    dirs: BTreeMap<NodeId, TempDir>,
    key: IssuerKey,
    kek: Arc<FileKek>,
    timeout: Duration,
    replicas: BTreeMap<NodeId, Replica>,
}

impl Cluster {
    /// A group formed from three copies of a new mint.
    async fn start() -> Self {
        Self::start_with(TIMEOUT).await
    }

    /// A group formed from three copies of a new mint, whose replicas wait
    /// `timeout` for the group.
    async fn start_with(timeout: Duration) -> Self {
        let key = IssuerKey::generate();
        let issuer = Issuer::new(key.clone(), LEDGER, Policy::cash_like()).unwrap();
        let seed = tempfile::tempdir().unwrap();
        drop(FileStore::create(seed.path(), &issuer).unwrap());
        Self::from_copies(seed.path(), key, timeout).await
    }

    /// Readies three copies of the mint in `seed`, opens a replica on each
    /// and forms the group from the first.
    async fn from_copies(seed: &Path, key: IssuerKey, timeout: Duration) -> Self {
        let dirs: BTreeMap<_, _> = (1..=3)
            .map(|id| (id, tempfile::tempdir().unwrap()))
            .collect();
        let mut cluster = Self {
            router: Router::default(),
            dirs,
            key,
            kek: Arc::new(FileKek::from_bytes(&[7; 32])),
            timeout,
            replicas: BTreeMap::new(),
        };
        for id in 1..=3 {
            let dir = cluster.dirs[&id].path();
            copy_store(seed, dir);
            let ready =
                worgl_raft::prepare(dir, cluster.key.clone(), Policy::cash_like(), &*cluster.kek);
            assert!(
                ready.unwrap(),
                "a copy of a new store has never joined a group"
            );
            cluster.open(id).await;
        }
        let members = (1..=3)
            .map(|id| (id, BasicNode::new(format!("replica-{id}"))))
            .collect();
        cluster.replicas[&1].initialize(members).await.unwrap();
        cluster
    }

    async fn open(&mut self, id: NodeId) {
        let mut options = Options::new(
            id,
            self.dirs[&id].path(),
            self.key.clone(),
            Policy::cash_like(),
            self.kek.clone(),
        );
        options.raft = openraft::Config {
            heartbeat_interval: 100,
            election_timeout_min: 500,
            election_timeout_max: 1_000,
            snapshot_policy: SnapshotPolicy::LogsSinceLast(1_000),
            // Every entry a snapshot holds leaves the log, so a replica that
            // fell behind needs the snapshot.
            max_in_snapshot_log_to_keep: 0,
            ..default_config()
        };
        options.timeout = self.timeout;
        let network = Network {
            router: self.router.clone(),
            from: id,
        };
        let replica = Replica::open(options, network).await.unwrap();
        self.router
            .nodes
            .lock()
            .unwrap()
            .insert(id, replica.raft().clone());
        self.replicas.insert(id, replica);
    }

    async fn stop(&mut self, id: NodeId) {
        self.router.nodes.lock().unwrap().remove(&id);
        let replica = self.replicas.remove(&id).unwrap();
        replica.shutdown().await.unwrap();
    }

    fn isolate(&self, id: NodeId) {
        self.router.isolated.lock().unwrap().insert(id);
    }

    fn heal(&self, id: NodeId) {
        self.router.isolated.lock().unwrap().remove(&id);
    }

    /// Waits for a leader among the replicas that are running and not cut
    /// off, and returns its id.
    async fn leader(&self) -> NodeId {
        let isolated = self.router.isolated.lock().unwrap().clone();
        let deadline = Instant::now() + WAIT;
        loop {
            for (id, replica) in &self.replicas {
                if !isolated.contains(id)
                    && replica.raft().metrics().borrow().state == ServerState::Leader
                {
                    return *id;
                }
            }
            assert!(Instant::now() < deadline, "no leader was elected");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until replica `id` has applied every event replica `of` has.
    /// (A replica's metrics can trail what it applied, so they cannot say.)
    async fn caught_up(&self, id: NodeId, of: NodeId) {
        let sequence = self.replicas[&of].read_local(Issuer::sequence);
        let deadline = Instant::now() + WAIT;
        while self.replicas[&id].read_local(Issuer::sequence) < sequence {
            assert!(Instant::now() < deadline, "replica {id} did not catch up");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn replica(&self, id: NodeId) -> &Replica {
        &self.replicas[&id]
    }

    async fn shutdown(mut self) {
        for id in self.replicas.keys().copied().collect::<Vec<_>>() {
            self.stop(id).await;
        }
    }
}

fn copy_store(from: &Path, to: &Path) {
    for name in ["snapshot.bin", "keys.bin", "log.bin"] {
        std::fs::copy(from.join(name), to.join(name)).unwrap();
    }
}

async fn mint(replica: &Replica, amount: u64, holder: &HolderKey) -> Note {
    let mut batch = replica.batch().await.unwrap();
    let note = batch.mint(amount, holder.public_key()).unwrap();
    batch.commit().await.unwrap();
    note
}

/// Pays `note` on to a fresh holder through `replica`.
async fn pay(replica: &Replica, note: Note, holder: &HolderKey) -> (Note, HolderKey) {
    let next = HolderKey::generate();
    let output = Output {
        amount: note.amount(),
        holder: next.public_key(),
    };
    let request = TransferRequest::single(note, holder, output).unwrap();
    let verified = replica.verifier().borrow().verify(&request).unwrap();
    let mut batch = replica.batch().await.unwrap();
    let paid = batch.transfer(&verified).unwrap().remove(0);
    batch.commit().await.unwrap();
    (paid, next)
}

fn status(replica: &Replica, note: &Note) -> NoteStatus {
    replica.read_local(|issuer| issuer.status(note).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_member_applies_what_the_leader_commits() {
    let cluster = Cluster::start().await;
    let leader = cluster.leader().await;
    let alice = HolderKey::generate();
    let note = mint(cluster.replica(leader), 8, &alice).await;
    let (paid, _) = pay(cluster.replica(leader), note, &alice).await;
    assert_eq!(
        cluster
            .replica(leader)
            .read(|issuer| issuer.status(&paid).unwrap())
            .await
            .unwrap(),
        status(cluster.replica(leader), &paid)
    );

    for id in (1..=3).filter(|&id| id != leader) {
        cluster.caught_up(id, leader).await;
        assert_eq!(status(cluster.replica(id), &note), NoteStatus::Spent);
        assert!(matches!(
            status(cluster.replica(id), &paid),
            NoteStatus::Live { .. }
        ));
        assert_eq!(
            cluster.replica(id).read_local(Issuer::snapshot),
            cluster.replica(leader).read_local(Issuer::snapshot)
        );
        // Followers send changes and reads to the leader.
        let refused = cluster.replica(id).batch().await.unwrap_err();
        assert_eq!(
            refused,
            ReplicaError::NotLeader {
                leader: Some(leader),
                node: Some(BasicNode::new(format!("replica-{leader}"))),
            }
        );
        assert!(matches!(
            cluster.replica(id).read(|_| ()).await,
            Err(ReplicaError::NotLeader { leader: Some(l), .. }) if l == leader
        ));
    }
    cluster.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_leader_takes_over_when_the_old_one_is_cut_off() {
    let cluster = Cluster::start().await;
    let old = cluster.leader().await;
    let alice = HolderKey::generate();
    let note = mint(cluster.replica(old), 8, &alice).await;

    cluster.isolate(old);
    let new = cluster.leader().await;
    assert_ne!(new, old);
    // The note the old leader minted pays on through the new one.
    let (paid, _) = pay(cluster.replica(new), note, &alice).await;
    assert_eq!(status(cluster.replica(new), &note), NoteStatus::Spent);

    // Cut off, the old leader stops counting itself the leader once it has
    // gone a lease without hearing from the others, and then refuses
    // changes and reads at once.
    let deadline = Instant::now() + WAIT;
    while cluster.replica(old).is_leader() {
        assert!(
            Instant::now() < deadline,
            "the old leader still thinks it leads"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(matches!(
        cluster.replica(old).batch().await,
        Err(ReplicaError::NotLeader { leader: None, .. })
    ));
    assert!(cluster.replica(old).read(|_| ()).await.is_err());

    // Back on the network, it follows the new leader, and the mint it could
    // not commit is gone.
    cluster.heal(old);
    let carol = mint(cluster.replica(new), 4, &HolderKey::generate()).await;
    cluster.caught_up(old, new).await;
    assert_eq!(
        cluster.replica(old).read_local(Issuer::snapshot),
        cluster.replica(new).read_local(Issuer::snapshot)
    );
    assert!(matches!(
        cluster.replica(old).batch().await,
        Err(ReplicaError::NotLeader { leader: Some(l), .. }) if l == new
    ));
    let supply = cluster.replica(new).read(|i| i.supply()).await.unwrap();
    assert_eq!((supply.minted, supply.outstanding), (12, 12));
    assert!(matches!(
        status(cluster.replica(old), &paid),
        NoteStatus::Live { .. }
    ));
    assert!(matches!(
        status(cluster.replica(old), &carol),
        NoteStatus::Live { .. }
    ));
    cluster.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leader_cut_off_mid_commit_gives_up_once_its_lease_lapses() {
    // A timeout far past the lease, so only the lease can end the wait.
    let cluster = Cluster::start_with(Duration::from_secs(60)).await;
    let old = cluster.leader().await;
    let alice = HolderKey::generate();
    mint(cluster.replica(old), 8, &alice).await;

    let mut batch = cluster.replica(old).batch().await.unwrap();
    batch.mint(1, alice.public_key()).unwrap();
    cluster.isolate(old);
    let cut = Instant::now();
    assert_eq!(batch.commit().await, Err(ReplicaError::Timeout));
    let waited = cut.elapsed();
    assert!(
        waited < WAIT,
        "waited {waited:?} for a group it cannot reach"
    );

    // Nor does it take new changes or reads.
    assert!(!cluster.replica(old).is_leader());
    let asked = Instant::now();
    assert!(matches!(
        cluster.replica(old).batch().await,
        Err(ReplicaError::NotLeader { leader: None, .. })
    ));
    assert!(matches!(
        cluster.replica(old).read(|_| ()).await,
        Err(ReplicaError::NotLeader { .. })
    ));
    assert!(asked.elapsed() < Duration::from_secs(5));

    // Back on the network, it follows the leader the others elected, and
    // the mint it gave up on never reached them.
    let new = cluster.leader().await;
    assert_ne!(new, old);
    cluster.heal(old);
    mint(cluster.replica(new), 4, &HolderKey::generate()).await;
    cluster.caught_up(old, new).await;
    assert_eq!(
        cluster.replica(old).read_local(Issuer::snapshot),
        cluster.replica(new).read_local(Issuer::snapshot)
    );
    let supply = cluster.replica(new).read(|i| i.supply()).await.unwrap();
    assert_eq!(supply.minted, 12);
    cluster.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_leader_signs_with_the_keys_the_old_one_made() {
    let cluster = Cluster::start().await;
    let old = cluster.leader().await;
    let mut batch = cluster.replica(old).batch().await.unwrap();
    let first = batch.rotate().unwrap();
    let second = batch.rotate().unwrap();
    batch.commit().await.unwrap();
    let verifier = cluster.replica(old).verifier().borrow().clone();

    cluster.isolate(old);
    let new = cluster.leader().await;
    cluster.caught_up(new, old).await;
    let keys = cluster.replica(new).read_local(|i| i.keys().clone());
    assert_eq!(
        keys.signer(first.created),
        first.key.map(|c| c.key).as_ref()
    );
    assert_eq!(
        keys.signer(second.created),
        second.key.map(|c| c.key).as_ref()
    );

    // A mint lands in the first new generation and a payment in the second;
    // both are signed by keys only the old leader ever held in the clear.
    let alice = HolderKey::generate();
    let note = mint(cluster.replica(new), 8, &alice).await;
    assert_eq!(note.serial().generation, first.created);
    let (paid, _) = pay(cluster.replica(new), note, &alice).await;
    assert_eq!(paid.serial().generation, second.created);
    assert!(matches!(
        cluster
            .replica(new)
            .read(|issuer| issuer.status(&paid).unwrap())
            .await
            .unwrap(),
        NoteStatus::Live { .. }
    ));
    assert_eq!(verifier.keys(), &keys);
    cluster.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replica_that_fell_behind_catches_up_from_a_snapshot() {
    let mut cluster = Cluster::start().await;
    let leader = cluster.leader().await;
    let behind = (1..=3).find(|&id| id != leader).unwrap();
    cluster.caught_up(behind, leader).await;
    cluster.stop(behind).await;

    // A rotation makes the leader snapshot, and the snapshot takes the log
    // with it.
    let alice = HolderKey::generate();
    let note = mint(cluster.replica(leader), 8, &alice).await;
    let mut batch = cluster.replica(leader).batch().await.unwrap();
    let rotation = batch.rotate().unwrap();
    batch.commit().await.unwrap();
    let rotated = cluster
        .replica(leader)
        .raft()
        .metrics()
        .borrow()
        .last_applied;
    cluster
        .replica(leader)
        .raft()
        .wait(Some(WAIT))
        .metrics(|m| m.purged >= rotated, "the log is purged")
        .await
        .unwrap();
    let (paid, _) = pay(cluster.replica(leader), note, &alice).await;

    cluster.open(behind).await;
    cluster.caught_up(behind, leader).await;
    let metrics = cluster.replica(behind).raft().metrics().borrow().clone();
    assert!(metrics.snapshot >= rotated, "no snapshot was installed");
    assert_eq!(
        cluster.replica(behind).read_local(Issuer::snapshot),
        cluster.replica(leader).read_local(Issuer::snapshot)
    );
    assert!(matches!(
        status(cluster.replica(behind), &paid),
        NoteStatus::Live { .. }
    ));
    // The snapshot brought the new generation's key, so the replica signs
    // in it as the leader does.
    let holder = HolderKey::generate().public_key();
    let sign = |replica: &Replica| {
        replica.read_local(|issuer| {
            let event = issuer.plan_mint(8, holder).unwrap();
            issuer.notes(&event)
        })
    };
    let notes = sign(cluster.replica(behind));
    assert_eq!(notes, sign(cluster.replica(leader)));
    assert_eq!(notes[0].serial().generation, rotation.created - 1);
    cluster.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_restarted_whole_restores_what_it_committed() {
    let mut cluster = Cluster::start().await;
    let leader = cluster.leader().await;
    let alice = HolderKey::generate();
    let note = mint(cluster.replica(leader), 8, &alice).await;
    let mut batch = cluster.replica(leader).batch().await.unwrap();
    batch.rotate().unwrap();
    batch.commit().await.unwrap();
    let (paid, bob) = pay(cluster.replica(leader), note, &alice).await;
    let expected = cluster.replica(leader).read_local(Issuer::snapshot);
    for id in 1..=3 {
        cluster.stop(id).await;
    }

    for id in 1..=3 {
        cluster.open(id).await;
    }
    let leader = cluster.leader().await;
    assert_eq!(
        cluster
            .replica(leader)
            .read(Issuer::snapshot)
            .await
            .unwrap(),
        expected
    );
    // A resent payment is answered with the same note, not paid twice.
    let output = Output {
        amount: 8,
        holder: *paid.holder(),
    };
    let request = TransferRequest::single(note, &alice, output).unwrap();
    let verified = cluster.replica(leader).verifier().borrow().verify(&request);
    let mut batch = cluster.replica(leader).batch().await.unwrap();
    assert_eq!(batch.transfer(&verified.unwrap()).unwrap(), [paid]);
    assert!(batch.is_empty());
    batch.commit().await.unwrap();
    pay(cluster.replica(leader), paid, &bob).await;
    cluster.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_member_verifies_a_payment_resent_after_its_note_was_dropped() {
    let mut cluster = Cluster::start().await;
    let leader = cluster.leader().await;
    let alice = HolderKey::generate();
    let note = mint(cluster.replica(leader), 8, &alice).await;
    let (paid, _) = pay(cluster.replica(leader), note, &alice).await;
    // The answer is lost, and rotations drop the spent note's generation
    // before the payer resends.
    let mut batch = cluster.replica(leader).batch().await.unwrap();
    for _ in 0..3 {
        batch.rotate().unwrap();
    }
    batch.commit().await.unwrap();
    assert_eq!(status(cluster.replica(leader), &note), NoteStatus::Expired);
    assert!(matches!(
        status(cluster.replica(leader), &paid),
        NoteStatus::Live { .. }
    ));

    // One follower restarts from the snapshot it took after the rotations.
    let rotated = cluster
        .replica(leader)
        .raft()
        .metrics()
        .borrow()
        .last_applied;
    let restarted = (1..=3).find(|&id| id != leader).unwrap();
    cluster
        .replica(restarted)
        .raft()
        .wait(Some(WAIT))
        .metrics(|m| m.snapshot >= rotated, "the follower snapshots")
        .await
        .unwrap();
    cluster.stop(restarted).await;
    cluster.open(restarted).await;

    let output = Output {
        amount: 8,
        holder: *paid.holder(),
    };
    let request = TransferRequest::single(note, &alice, output).unwrap();
    for id in 1..=3 {
        cluster.caught_up(id, leader).await;
        let verified = cluster.replica(id).verifier().borrow().verify(&request);
        let mut batch = cluster.replica(leader).batch().await.unwrap();
        assert_eq!(batch.transfer(&verified.unwrap()).unwrap(), [paid]);
        assert!(batch.is_empty());
        batch.commit().await.unwrap();
    }
    cluster.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mint_that_ran_alone_carries_on_as_a_group() {
    let key = IssuerKey::generate();
    let issuer = Issuer::new(key.clone(), LEDGER, Policy::cash_like()).unwrap();
    let seed = tempfile::tempdir().unwrap();
    let alice = HolderKey::generate();
    let (note, alone) = {
        let mut store = PersistentIssuer::create(seed.path(), issuer).unwrap();
        let note = store.mint(8, alice.public_key()).unwrap();
        store.rotate().unwrap();
        (note, store.issuer().snapshot())
    };

    let mut cluster = Cluster::from_copies(seed.path(), key, TIMEOUT).await;
    let leader = cluster.leader().await;
    assert_eq!(
        cluster
            .replica(leader)
            .read(Issuer::snapshot)
            .await
            .unwrap(),
        alone
    );
    let (paid, _) = pay(cluster.replica(leader), note, &alice).await;

    // Readying a member that has joined leaves it as it is.
    let member = (1..=3).find(|&id| id != leader).unwrap();
    cluster.caught_up(member, leader).await;
    cluster.stop(member).await;
    let dir = cluster.dirs[&member].path().to_owned();
    let ready = |dir: &Path| {
        worgl_raft::prepare(dir, cluster.key.clone(), Policy::cash_like(), &*cluster.kek)
    };
    assert!(!ready(&dir).unwrap());
    // A copy of its store without its vote is refused, not re-based.
    let copy = tempfile::tempdir().unwrap();
    copy_store(&dir, copy.path());
    assert!(matches!(
        ready(copy.path()),
        Err(StoreError::Corrupt { .. })
    ));

    cluster.open(member).await;
    cluster.caught_up(member, leader).await;
    assert!(matches!(
        status(cluster.replica(member), &paid),
        NoteStatus::Live { .. }
    ));
    cluster.shutdown().await;
}
