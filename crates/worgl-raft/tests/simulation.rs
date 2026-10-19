//! Seeded fault simulations of one mint's three-replica group.
//!
//! Each run starts a group in one process on a simulated clock and an
//! in-process network, and lets three clients mint, pay and rotate through
//! whichever replica leads while a seeded fault schedule cuts replicas off,
//! splits the group, crashes and restarts replicas, and drops and delays
//! messages. A client that gets no clear answer sends the same transfer
//! again until it does, as a real one would. Once the faults stop and the
//! group settles, the run checks that
//!
//! - every replica holds one history, before and after a restart of all,
//! - every answered mint, transfer and rotation is in it,
//! - no note was spent twice, and no note a client holds was spent by
//!   anyone else, and
//! - supply balances: what was minted is what is live plus what rotations
//!   destroyed, and live notes are exactly the clients' plus at most the
//!   mints that got no clear answer.
//!
//! `WORGL_SIM_SEEDS=n` runs seeds 0 to n − 1 (32 by default), and
//! `WORGL_SIM_SEED=s` runs seed `s` alone. The fault schedule follows from
//! the seed; openraft's election timeouts do not, so a failing seed may take
//! a few tries to fail again.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openraft::error::{
    Fatal, NetworkError, RPCError, RaftError, RemoteError, ReplicationClosed, StreamingError,
    Unreachable,
};
use openraft::network::RPCOption;
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, SnapshotResponse, VoteRequest, VoteResponse,
};
use openraft::{BasicNode, RaftNetwork, RaftNetworkFactory, Snapshot, SnapshotPolicy, Vote};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};
use worgl::{
    Error, FileKek, FileStore, HolderKey, Issuer, IssuerKey, LedgerConfig, Note, NoteStatus,
    Output, Policy, Serial, TransferRequest,
};
use worgl_raft::{
    NodeId, Options, Raft, Replica, ReplicaError, SnapshotData, TypeConfig, default_config,
};

const LEDGER: LedgerConfig = LedgerConfig {
    layers: 4,
    layer_size: 1024,
};
/// Every note is worth the same, so a count of live notes is their value.
const AMOUNT: u64 = 8;
/// Operations each client carries out.
const OPS: usize = 40;
/// Fault-schedule steps in a run.
const STEPS: usize = 40;
/// How long, in simulated time, a client keeps trying one operation and the
/// group may take to settle once the faults stop.
const PATIENCE: Duration = Duration::from_secs(300);
const DEFAULT_SEEDS: u64 = 32;

#[test]
fn seeded_fault_runs_keep_every_invariant() {
    let seeds: Vec<u64> = match std::env::var("WORGL_SIM_SEED") {
        Ok(seed) => vec![seed.parse().expect("WORGL_SIM_SEED is a number")],
        Err(_) => {
            let count = std::env::var("WORGL_SIM_SEEDS")
                .map(|count| count.parse().expect("WORGL_SIM_SEEDS is a number"))
                .unwrap_or(DEFAULT_SEEDS);
            (0..count).collect()
        }
    };
    let next = AtomicU64::new(0);
    let failures = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&seed) = seeds.get(index as usize) else {
                        return;
                    };
                    if let Err(failure) = simulate(seed) {
                        failures.lock().unwrap().push(failure);
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} seeds failed:\n\n{}",
        failures.len(),
        seeds.len(),
        failures.join("\n\n")
    );
}

/// Runs seed `seed` on a runtime of its own, whose clock only moves while
/// every task waits, and says what went wrong if anything did.
fn simulate(seed: u64) -> Result<(), String> {
    let log = Log::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(run(seed, log.clone()))
    }));
    drop(runtime);
    let error = match outcome {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(error)) => error,
        Err(panic) => panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "a panic".into()),
    };
    Err(format!(
        "seed {seed}: {error}\nlast faults:\n{}\nrun again with WORGL_SIM_SEED={seed}",
        log.tail(30)
    ))
}

/// What the fault schedule did, for the report of a failed run.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn note(&self, line: String) {
        if std::env::var_os("WORGL_SIM_VERBOSE").is_some() {
            eprintln!("{line}");
        }
        self.0.lock().unwrap().push(line);
    }

    fn tail(&self, lines: usize) -> String {
        let log = self.0.lock().unwrap();
        log[log.len().saturating_sub(lines)..].join("\n")
    }
}

/// SplitMix64: small, seedable and good enough to pick faults.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// True with probability `percent` / 100.
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// The network's faults at the moment.
struct Faults {
    rng: Rng,
    /// Which side of a partition each replica is on; replicas on different
    /// sides cannot reach each other.
    side: BTreeMap<NodeId, u8>,
    /// Chance in percent that a request, or its answer, is lost.
    drop: u64,
    /// Longest delay, in milliseconds, before a request or its answer.
    delay: u64,
}

/// Delivers RPCs between the replicas, through the faults.
#[derive(Clone)]
struct Router {
    nodes: Arc<Mutex<BTreeMap<NodeId, Raft>>>,
    faults: Arc<Mutex<Faults>>,
}

/// Why a message did not arrive.
enum Lost {
    Unreachable,
    Dropped,
}

impl Router {
    fn new(seed: u64) -> Self {
        Self {
            nodes: Arc::default(),
            faults: Arc::new(Mutex::new(Faults {
                rng: Rng(seed ^ 0x5eed),
                side: (1..=3).map(|id| (id, 0)).collect(),
                drop: 0,
                delay: 0,
            })),
        }
    }

    /// Waits out a message's delay, then says whether it gets through.
    async fn hop(&self, from: NodeId, to: NodeId) -> Result<(), Lost> {
        let (delay, dropped) = {
            let mut faults = self.faults.lock().unwrap();
            let delay = match faults.delay {
                0 => 0,
                max => faults.rng.below(max + 1),
            };
            let drop = faults.drop;
            (delay, faults.rng.chance(drop))
        };
        sleep(Duration::from_millis(delay)).await;
        let faults = self.faults.lock().unwrap();
        if faults.side[&from] != faults.side[&to] {
            return Err(Lost::Unreachable);
        }
        if dropped {
            return Err(Lost::Dropped);
        }
        Ok(())
    }

    fn node(&self, to: NodeId) -> Result<Raft, Lost> {
        self.nodes
            .lock()
            .unwrap()
            .get(&to)
            .cloned()
            .ok_or(Lost::Unreachable)
    }

    /// Carries a request from `from` to `to` and its answer back, through
    /// the faults either way.
    async fn deliver<T, E, F>(
        &self,
        from: NodeId,
        to: NodeId,
        send: impl FnOnce(Raft) -> F,
    ) -> Result<Result<T, E>, Lost>
    where
        F: Future<Output = Result<T, E>>,
    {
        self.hop(from, to).await?;
        let answer = send(self.node(to)?).await;
        self.hop(to, from).await?;
        Ok(answer)
    }
}

impl From<Lost> for RPCError<NodeId, BasicNode, RaftError<NodeId>> {
    fn from(lost: Lost) -> Self {
        match lost {
            Lost::Unreachable => Unreachable::new(&io::Error::other("cut off")).into(),
            Lost::Dropped => NetworkError::new(&io::Error::other("dropped")).into(),
        }
    }
}

impl From<Lost> for StreamingError<TypeConfig, Fatal<NodeId>> {
    fn from(lost: Lost) -> Self {
        match lost {
            Lost::Unreachable => Unreachable::new(&io::Error::other("cut off")).into(),
            Lost::Dropped => NetworkError::new(&io::Error::other("dropped")).into(),
        }
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
        self.router
            .deliver(self.from, self.target, |raft| async move {
                raft.append_entries(rpc).await
            })
            .await?
            .map_err(|e| RemoteError::new(self.target, e).into())
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        self.router
            .deliver(self.from, self.target, |raft| async move {
                raft.vote(rpc).await
            })
            .await?
            .map_err(|e| RemoteError::new(self.target, e).into())
    }

    async fn full_snapshot(
        &mut self,
        vote: Vote<NodeId>,
        snapshot: Snapshot<TypeConfig>,
        _cancel: impl Future<Output = ReplicationClosed> + Send + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<NodeId>, StreamingError<TypeConfig, Fatal<NodeId>>> {
        // As it would cross the network.
        let data = SnapshotData::from_bytes(&snapshot.snapshot.to_bytes()).unwrap();
        let snapshot = Snapshot {
            meta: snapshot.meta,
            snapshot: Box::new(data),
        };
        self.router
            .deliver(self.from, self.target, |raft| async move {
                raft.install_full_snapshot(vote, snapshot).await
            })
            .await?
            .map_err(|e| RemoteError::new(self.target, e).into())
    }
}

/// A group of three replicas, each in a directory of its own, any of which
/// may be down.
struct Group {
    router: Router,
    dirs: BTreeMap<NodeId, TempDir>,
    key: IssuerKey,
    kek: Arc<FileKek>,
    running: Mutex<BTreeMap<NodeId, Arc<Replica>>>,
    log: Log,
    started: Instant,
}

impl Group {
    async fn start(seed: u64, log: Log) -> Self {
        let key = IssuerKey::generate();
        let issuer = Issuer::new(key.clone(), LEDGER, Policy::cash_like()).unwrap();
        let first = tempfile::tempdir().unwrap();
        drop(FileStore::create(first.path(), &issuer).unwrap());
        let mut dirs = BTreeMap::from([(1, first)]);
        for id in 2..=3 {
            let dir = tempfile::tempdir().unwrap();
            for name in ["snapshot.bin", "keys.bin", "log.bin"] {
                std::fs::copy(dirs[&1].path().join(name), dir.path().join(name)).unwrap();
            }
            dirs.insert(id, dir);
        }
        let group = Self {
            router: Router::new(seed),
            dirs,
            key,
            kek: Arc::new(FileKek::from_bytes(&[7; 32])),
            running: Mutex::default(),
            log,
            started: Instant::now(),
        };
        for id in 1..=3 {
            let ready = worgl_raft::prepare(
                group.dirs[&id].path(),
                group.key.clone(),
                Policy::cash_like(),
                &*group.kek,
            );
            assert!(ready.unwrap());
            group.open(id).await;
        }
        let members = (1..=3)
            .map(|id| (id, BasicNode::new(format!("replica-{id}"))))
            .collect();
        group.replica(1).unwrap().initialize(members).await.unwrap();
        group
    }

    async fn open(&self, id: NodeId) {
        let mut options = Options::new(
            id,
            self.dirs[&id].path(),
            self.key.clone(),
            Policy::cash_like(),
            self.kek.clone(),
        );
        options.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 300,
            election_timeout_max: 600,
            // Snapshots often, keeping little log, so replicas that fall
            // behind are sent snapshots.
            snapshot_policy: SnapshotPolicy::LogsSinceLast(16),
            max_in_snapshot_log_to_keep: 4,
            ..default_config()
        };
        options.timeout = Duration::from_secs(2);
        let network = Network {
            router: self.router.clone(),
            from: id,
        };
        let replica = Arc::new(Replica::open(options, network).await.unwrap());
        self.router
            .nodes
            .lock()
            .unwrap()
            .insert(id, replica.raft().clone());
        self.running.lock().unwrap().insert(id, replica);
    }

    /// Stops replica `id` as a crash would, as far as one process can: it
    /// stops answering at once, then its node stops.
    async fn crash(&self, id: NodeId) {
        self.router.nodes.lock().unwrap().remove(&id);
        let replica = self.running.lock().unwrap().remove(&id);
        if let Some(replica) = replica {
            // A client may be using it, and will see it stop.
            let _ = replica.shutdown().await;
        }
    }

    fn replica(&self, id: NodeId) -> Option<Arc<Replica>> {
        self.running.lock().unwrap().get(&id).cloned()
    }

    fn up(&self) -> Vec<NodeId> {
        self.running.lock().unwrap().keys().copied().collect()
    }

    fn down(&self) -> Vec<NodeId> {
        let up = self.up();
        (1..=3).filter(|id| !up.contains(id)).collect()
    }

    /// The replica to ask: `hint` if it runs, or else any that does.
    fn pick(&self, hint: Option<NodeId>, rng: &mut Rng) -> Option<Arc<Replica>> {
        if let Some(replica) = hint.and_then(|id| self.replica(id)) {
            return Some(replica);
        }
        let up = self.up();
        let id = *up.get(rng.below(up.len().max(1) as u64) as usize)?;
        self.replica(id)
    }

    fn set_sides(&self, sides: [u8; 3]) {
        let mut faults = self.router.faults.lock().unwrap();
        for (id, side) in (1..=3).zip(sides) {
            faults.side.insert(id, side);
        }
    }

    fn set_noise(&self, drop: u64, delay: u64) {
        let mut faults = self.router.faults.lock().unwrap();
        faults.drop = drop;
        faults.delay = delay;
    }

    /// Logs `what`, at the simulated time since the run started.
    fn note(&self, what: String) {
        let at = self.started.elapsed().as_secs_f64();
        self.log.note(format!("{at:>8.3} s  {what}"));
    }

    /// Ends every fault: heals the network and restarts every replica.
    async fn heal(&self) {
        self.set_sides([0, 0, 0]);
        self.set_noise(0, 0);
        for id in self.down() {
            self.open(id).await;
        }
        self.note("healed and restarted all".into());
    }

    /// Heals the network, restarts every replica and waits until all three
    /// have applied what the leader has. Returns the leader.
    async fn settle(&self) -> Result<Arc<Replica>, String> {
        self.heal().await;
        let deadline = Instant::now() + PATIENCE;
        loop {
            if Instant::now() > deadline {
                return Err(format!(
                    "the group did not settle once the faults stopped: {}",
                    self.describe()
                ));
            }
            let running: Vec<_> = self.running.lock().unwrap().values().cloned().collect();
            for replica in running {
                if !replica.is_leader() {
                    continue;
                }
                let Ok(sequence) = replica.read(Issuer::sequence).await else {
                    continue;
                };
                let all = self.running.lock().unwrap().clone();
                if all
                    .values()
                    .all(|other| other.read_local(Issuer::sequence) == sequence)
                {
                    return Ok(replica);
                }
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    /// Each replica's state, for a report.
    fn describe(&self) -> String {
        let running = self.running.lock().unwrap().clone();
        (1..=3)
            .map(|id| match running.get(&id) {
                None => format!("replica {id} down"),
                Some(replica) => {
                    let metrics = replica.raft().metrics().borrow().clone();
                    format!(
                        "replica {id} {:?} in term {}, applied {:?}, sequence {}",
                        metrics.state,
                        metrics.current_term,
                        metrics.last_applied,
                        replica.read_local(Issuer::sequence)
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    async fn shutdown(&self) {
        for id in self.up() {
            self.crash(id).await;
        }
    }
}

/// What a client did, as it was answered.
#[derive(Default)]
struct Outcomes {
    /// Mints the group answered.
    minted: Vec<Note>,
    /// Mints that got no clear answer, which may or may not have been made.
    maybe_minted: Vec<Note>,
    /// Answered transfers: the note spent and the note paid.
    paid: Vec<(Note, Note)>,
    /// The notes the client holds, with their keys.
    held: Vec<(Note, HolderKey)>,
    /// Notes the client found expired.
    expired: Vec<Note>,
    /// The generations answered rotations created.
    rotations: Vec<u64>,
    /// Rotations that got no clear answer.
    maybe_rotations: usize,
}

/// How a commit ended, for the client deciding what to do next.
enum Answer {
    Done,
    /// Nothing was made; plan it again.
    Retry(Option<NodeId>),
    /// It may have been made.
    Unclear,
}

fn answer(result: Result<(), ReplicaError>) -> Answer {
    match result {
        Ok(()) => Answer::Done,
        Err(ReplicaError::NotLeader { leader, .. }) => Answer::Retry(leader),
        Err(ReplicaError::Stale) => Answer::Retry(None),
        Err(ReplicaError::Timeout | ReplicaError::Stopped(_)) => Answer::Unclear,
    }
}

async fn client(group: &Group, seed: u64, id: u64) -> Result<Outcomes, String> {
    let mut rng = Rng(seed.wrapping_mul(31).wrapping_add(id));
    let mut out = Outcomes::default();
    for _ in 0..OPS {
        let roll = rng.below(100);
        if roll < 8 {
            rotate(group, &mut rng, &mut out).await?;
        } else if out.held.is_empty() || roll < 40 {
            mint(group, &mut rng, &mut out).await?;
        } else {
            let index = rng.below(out.held.len() as u64) as usize;
            let (note, key) = out.held.swap_remove(index);
            pay(group, &mut rng, &mut out, note, key).await?;
        }
        sleep(Duration::from_millis(rng.below(200))).await;
    }
    Ok(out)
}

/// The next replica to ask, or `None` after a pause if none runs; an error
/// once the operation has waited too long.
async fn next_replica(
    group: &Group,
    rng: &mut Rng,
    hint: &mut Option<NodeId>,
    deadline: Instant,
    what: &str,
) -> Result<Option<Arc<Replica>>, String> {
    if Instant::now() > deadline {
        return Err(format!("{what} never went through"));
    }
    let replica = group.pick(hint.take(), rng);
    if replica.is_none() {
        sleep(Duration::from_millis(50)).await;
    }
    Ok(replica)
}

/// Waits a little after a refusal, and follows its hint next time.
async fn refused(error: ReplicaError, hint: &mut Option<NodeId>) {
    if let ReplicaError::NotLeader { leader, .. } = error {
        *hint = leader;
    }
    sleep(Duration::from_millis(20)).await;
}

async fn mint(group: &Group, rng: &mut Rng, out: &mut Outcomes) -> Result<(), String> {
    let holder = HolderKey::generate();
    let deadline = Instant::now() + PATIENCE;
    let mut hint = None;
    loop {
        let Some(replica) = next_replica(group, rng, &mut hint, deadline, "a mint").await? else {
            continue;
        };
        let mut batch = match replica.batch().await {
            Ok(batch) => batch,
            Err(error) => {
                refused(error, &mut hint).await;
                continue;
            }
        };
        let note = match batch.mint(AMOUNT, holder.public_key()) {
            Ok(note) => note,
            Err(error) => return Err(format!("a mint was refused: {error}")),
        };
        match answer(batch.commit().await) {
            Answer::Done => {
                out.minted.push(note);
                out.held.push((note, holder));
                return Ok(());
            }
            Answer::Retry(leader) => hint = leader,
            // A mint has nothing to resend it by here; the server's mint
            // requests do.
            Answer::Unclear => {
                out.maybe_minted.push(note);
                return Ok(());
            }
        }
    }
}

async fn rotate(group: &Group, rng: &mut Rng, out: &mut Outcomes) -> Result<(), String> {
    let deadline = Instant::now() + PATIENCE;
    let mut hint = None;
    loop {
        let Some(replica) = next_replica(group, rng, &mut hint, deadline, "a rotation").await?
        else {
            continue;
        };
        let mut batch = match replica.batch().await {
            Ok(batch) => batch,
            Err(error) => {
                refused(error, &mut hint).await;
                continue;
            }
        };
        let report = match batch.rotate() {
            Ok(report) => report,
            Err(error) => return Err(format!("a rotation was refused: {error}")),
        };
        match answer(batch.commit().await) {
            Answer::Done => {
                out.rotations.push(report.created);
                return Ok(());
            }
            Answer::Retry(leader) => hint = leader,
            Answer::Unclear => {
                out.maybe_rotations += 1;
                return Ok(());
            }
        }
    }
}

/// Pays `note` on to a fresh key of the client's, sending the same request
/// until the group answers it.
async fn pay(
    group: &Group,
    rng: &mut Rng,
    out: &mut Outcomes,
    note: Note,
    key: HolderKey,
) -> Result<(), String> {
    let next = HolderKey::generate();
    let output = Output {
        amount: note.amount(),
        holder: next.public_key(),
    };
    let request = TransferRequest::single(note, &key, output).unwrap();
    let deadline = Instant::now() + PATIENCE;
    let mut hint = None;
    loop {
        let what = format!("paying note {}", note.serial());
        let Some(replica) = next_replica(group, rng, &mut hint, deadline, &what).await? else {
            continue;
        };
        let verified = replica.verifier().borrow().verify(&request);
        let verified = match verified {
            Ok(verified) => verified,
            // Its generation's key is gone, or this replica has yet to
            // learn of it.
            Err(_) => {
                if let Ok(NoteStatus::Expired) = replica.read_local(|i| i.status(&note)) {
                    out.expired.push(note);
                    return Ok(());
                }
                sleep(Duration::from_millis(20)).await;
                continue;
            }
        };
        let mut batch = match replica.batch().await {
            Ok(batch) => batch,
            Err(error) => {
                refused(error, &mut hint).await;
                continue;
            }
        };
        let paid = match batch.transfer(&verified) {
            Ok(mut notes) => notes.remove(0),
            // In the last layer: kept until a rotation frees it.
            Err(Error::Frozen(_)) => {
                drop(batch);
                out.held.push((note, key));
                return Ok(());
            }
            Err(Error::Expired(_)) => {
                drop(batch);
                out.expired.push(note);
                return Ok(());
            }
            Err(error) => {
                return Err(format!(
                    "the leader refused to pay on note {}, which the client holds: {error}",
                    note.serial()
                ));
            }
        };
        match answer(batch.commit().await) {
            Answer::Done => {
                out.paid.push((note, paid));
                out.held.push((paid, next));
                return Ok(());
            }
            Answer::Retry(leader) => hint = leader,
            Answer::Unclear => {}
        }
    }
}

/// Changes the network and the replicas, step by step, as the seed says.
async fn inject(group: &Group, seed: u64) {
    let mut rng = Rng(seed);
    for _ in 0..STEPS {
        sleep(Duration::from_millis(100 + rng.below(900))).await;
        let pick = 1 + rng.below(3);
        match rng.below(100) {
            0..=24 => {
                let mut sides = [0; 3];
                sides[pick as usize - 1] = 1;
                group.set_sides(sides);
                group.note(format!("cut off replica {pick}"));
            }
            25..=44 => {
                group.set_sides([0, 0, 0]);
                group.note("healed the network".into());
            }
            45..=49 => {
                group.set_sides([0, 1, 2]);
                group.note("split all three apart".into());
            }
            50..=64 => {
                if group.down().is_empty() {
                    group.crash(pick).await;
                    group.note(format!("crashed replica {pick}"));
                }
            }
            65..=79 => {
                for id in group.down() {
                    group.open(id).await;
                    group.note(format!("restarted replica {id}"));
                }
            }
            80..=94 => {
                let (drop, delay) = (rng.below(30), rng.below(60));
                group.set_noise(drop, delay);
                group.note(format!("lose {drop}% of messages, delay up to {delay} ms"));
            }
            _ => {
                group.set_noise(0, 0);
                group.note("stopped losing and delaying messages".into());
            }
        }
    }
    // The faults stop, so clients still waiting on an operation get it done.
    group.heal().await;
}

async fn run(seed: u64, log: Log) -> Result<(), String> {
    let group = Group::start(seed, log).await;
    let first_generation = group
        .replica(1)
        .unwrap()
        .read_local(|i| i.ledger().newest_generation());
    let (_, a, b, c) = tokio::join!(
        inject(&group, seed),
        client(&group, seed, 0),
        client(&group, seed, 1),
        client(&group, seed, 2),
    );
    let clients = [a?, b?, c?];
    let leader = group.settle().await?;
    let total = |f: fn(&Outcomes) -> usize| clients.iter().map(f).sum::<usize>();
    group.note(format!(
        "settled: {} mints answered and {} unclear, {} payments, {} rotations answered and {} \
         unclear, {} notes expired",
        total(|out| out.minted.len()),
        total(|out| out.maybe_minted.len()),
        total(|out| out.paid.len()),
        total(|out| out.rotations.len()),
        total(|out| out.maybe_rotations),
        total(|out| out.expired.len()),
    ));
    check(&group, &leader, &clients, first_generation)?;

    // Restarted whole, the group comes back with the same history.
    let before = leader.read_local(Issuer::snapshot);
    drop(leader);
    group.shutdown().await;
    group.note("stopped all".into());
    let leader = group.settle().await?;
    if leader.read_local(Issuer::snapshot) != before {
        return Err("the group restarted whole with another history".into());
    }
    drop(leader);
    group.shutdown().await;
    Ok(())
}

/// Checks the settled group against what the clients were answered.
fn check(
    group: &Group,
    leader: &Replica,
    clients: &[Outcomes],
    first_generation: u64,
) -> Result<(), String> {
    let snapshot = leader.read_local(Issuer::snapshot);
    for id in group.up() {
        if group.replica(id).unwrap().read_local(Issuer::snapshot) != snapshot {
            return Err(format!("replica {id} holds another history"));
        }
    }
    let status = |note: &Note| {
        leader
            .read_local(|i| i.status(note))
            .map_err(|error| format!("note {}: {error}", note.serial()))
    };

    let mut spent_by: BTreeMap<Serial, Serial> = BTreeMap::new();
    for out in clients {
        for note in &out.minted {
            if status(note)? == NoteStatus::Unissued {
                return Err(format!("answered mint {} is missing", note.serial()));
            }
        }
        for (input, paid) in &out.paid {
            let spent = status(input)?;
            if !matches!(spent, NoteStatus::Spent | NoteStatus::Expired) {
                return Err(format!(
                    "note {} was paid on but is {spent:?}",
                    input.serial()
                ));
            }
            if status(paid)? == NoteStatus::Unissued {
                return Err(format!("answered payment {} is missing", paid.serial()));
            }
            if let Some(other) = spent_by.insert(input.serial(), paid.serial())
                && other != paid.serial()
            {
                return Err(format!(
                    "note {} was spent twice, into {other} and {}",
                    input.serial(),
                    paid.serial()
                ));
            }
        }
        for (note, _) in &out.held {
            let held = status(note)?;
            if !matches!(held, NoteStatus::Live { .. } | NoteStatus::Expired) {
                return Err(format!("note {} a client holds is {held:?}", note.serial()));
            }
        }
    }

    let supply = leader.read_local(Issuer::supply);
    let layers = leader.read_local(Issuer::layers);
    let live: u128 = layers.iter().map(|layer| u128::from(layer.live)).sum();
    let value: u128 = layers.iter().map(|layer| layer.value).sum();
    if supply.minted != supply.destroyed + supply.burned + supply.outstanding
        || supply.outstanding != value
        || value != live * u128::from(AMOUNT)
    {
        return Err(format!(
            "supply does not balance: {supply:?}, {live} live notes worth {value}"
        ));
    }
    let count = |f: fn(&Outcomes) -> usize| clients.iter().map(f).sum::<usize>() as u128;
    let minted = count(|out| out.minted.len());
    let maybe = count(|out| out.maybe_minted.len());
    if !(minted..=minted + maybe).contains(&(supply.minted / u128::from(AMOUNT))) {
        return Err(format!(
            "{} notes were minted, but {minted} mints were answered and {maybe} unclear",
            supply.minted / u128::from(AMOUNT)
        ));
    }
    let mut held_live = 0;
    for out in clients {
        for (note, _) in &out.held {
            if matches!(status(note)?, NoteStatus::Live { .. }) {
                held_live += 1;
            }
        }
    }
    if !(held_live..=held_live + maybe).contains(&live) {
        return Err(format!(
            "{live} notes are live, but clients hold {held_live} and {maybe} mints were unclear"
        ));
    }

    let rotated = leader.read_local(|i| i.ledger().newest_generation()) - first_generation;
    let answered = count(|out| out.rotations.len()) as u64;
    let unclear = count(|out| out.maybe_rotations) as u64;
    if !(answered..=answered + unclear).contains(&rotated) {
        return Err(format!(
            "{rotated} rotations happened, but {answered} were answered and {unclear} unclear"
        ));
    }
    let newest = clients.iter().flat_map(|out| &out.rotations).max();
    let known: BTreeSet<_> = clients.iter().flat_map(|out| &out.rotations).collect();
    if known.len() != answered as usize || newest.is_some_and(|&g| g > rotated + first_generation) {
        return Err("answered rotations do not match the generations made".into());
    }
    Ok(())
}
