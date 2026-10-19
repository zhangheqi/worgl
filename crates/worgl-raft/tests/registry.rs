//! A three-member registry on an in-process network.

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openraft::error::{
    Fatal, RPCError, RaftError, RemoteError, ReplicationClosed, StreamingError, Unreachable,
};
use openraft::network::RPCOption;
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, SnapshotResponse, VoteRequest, VoteResponse,
};
use openraft::{BasicNode, RaftNetwork, RaftNetworkFactory, Snapshot, SnapshotPolicy, Vote};
use serde_json::json;
use tempfile::TempDir;
use worgl::{IssuerKey, PublicKey, StoreError};
use worgl_raft::registry::{Records, Registry, RegistryConfig, RegistryRaft};
use worgl_raft::{NodeId, OpenError, ReplicaError, default_config};

const WAIT: Duration = Duration::from_secs(10);

/// Delivers RPCs between the members of one process.
#[derive(Clone, Default)]
struct Router {
    nodes: Arc<Mutex<BTreeMap<NodeId, RegistryRaft>>>,
}

impl Router {
    fn reach(&self, to: NodeId) -> Result<RegistryRaft, Unreachable> {
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
}

impl RaftNetworkFactory<RegistryConfig> for Network {
    type Network = Connection;

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> Connection {
        Connection {
            router: self.router.clone(),
            target,
        }
    }
}

struct Connection {
    router: Router,
    target: NodeId,
}

impl RaftNetwork<RegistryConfig> for Connection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<RegistryConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        let raft = self.router.reach(self.target)?;
        raft.append_entries(rpc)
            .await
            .map_err(|e| RemoteError::new(self.target, e).into())
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        let raft = self.router.reach(self.target)?;
        raft.vote(rpc)
            .await
            .map_err(|e| RemoteError::new(self.target, e).into())
    }

    async fn full_snapshot(
        &mut self,
        vote: Vote<NodeId>,
        snapshot: Snapshot<RegistryConfig>,
        _cancel: impl Future<Output = ReplicationClosed> + Send + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<NodeId>, StreamingError<RegistryConfig, Fatal<NodeId>>> {
        let raft = self.router.reach(self.target)?;
        // As it would cross the network.
        let bytes = serde_json::to_vec(&*snapshot.snapshot).unwrap();
        let records: Records = serde_json::from_slice(&bytes).unwrap();
        let snapshot = Snapshot {
            meta: snapshot.meta,
            snapshot: Box::new(records),
        };
        raft.install_full_snapshot(vote, snapshot)
            .await
            .map_err(|e| RemoteError::new(self.target, e).into())
    }
}

/// Three members, each with a directory of its own.
struct Cluster {
    router: Router,
    dirs: BTreeMap<NodeId, TempDir>,
    members: BTreeMap<NodeId, Registry>,
}

impl Cluster {
    async fn start() -> Self {
        let mut cluster = Self {
            router: Router::default(),
            dirs: (1..=3)
                .map(|id| (id, tempfile::tempdir().unwrap()))
                .collect(),
            members: BTreeMap::new(),
        };
        for id in 1..=3 {
            assert!(Registry::is_new(cluster.dirs[&id].path().join("registry")));
            cluster.open(id).await;
        }
        let nodes = (1..=3).map(|id| (id, BasicNode::new(""))).collect();
        cluster.members[&1].initialize(nodes).await.unwrap();
        cluster
    }

    async fn open(&mut self, id: NodeId) {
        let config = openraft::Config {
            heartbeat_interval: 100,
            election_timeout_min: 500,
            election_timeout_max: 1_000,
            snapshot_policy: SnapshotPolicy::LogsSinceLast(2),
            // Every entry a snapshot holds leaves the log, so a member that
            // fell behind needs the snapshot.
            max_in_snapshot_log_to_keep: 0,
            ..default_config()
        };
        let network = Network {
            router: self.router.clone(),
        };
        let registry = Registry::open(id, self.dir(id), config, network)
            .await
            .unwrap();
        self.router
            .nodes
            .lock()
            .unwrap()
            .insert(id, registry.raft().clone());
        self.members.insert(id, registry);
    }

    fn dir(&self, id: NodeId) -> std::path::PathBuf {
        self.dirs[&id].path().join("registry")
    }

    async fn stop(&mut self, id: NodeId) {
        self.router.nodes.lock().unwrap().remove(&id);
        let registry = self.members.remove(&id).unwrap();
        registry.shutdown().await.unwrap();
    }

    /// The member every running one agrees leads.
    async fn leader(&self) -> NodeId {
        let deadline = Instant::now() + WAIT;
        loop {
            let views: Vec<_> = self.members.values().map(Registry::leader).collect();
            if let Some(&Some(leader)) = views.first()
                && views.iter().all(|&view| view == Some(leader))
                && self.members.get(&leader).is_some_and(Registry::is_leader)
            {
                return leader;
            }
            assert!(Instant::now() < deadline, "no agreed leader: {views:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Inserts through the leader, finding it again if it changes.
    async fn insert(&self, mint: PublicKey, record: serde_json::Value) -> bool {
        let deadline = Instant::now() + WAIT;
        loop {
            let leader = self.leader().await;
            match self.members[&leader].insert(mint, record.clone()).await {
                Ok(added) => return added,
                Err(error) => {
                    assert!(Instant::now() < deadline, "{error}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    /// Waits until member `id` has applied records equal to `expected`.
    async fn holds(&self, id: NodeId, expected: &Records) {
        let mut records = self.members[&id].records();
        tokio::time::timeout(WAIT, records.wait_for(|records| records == expected))
            .await
            .unwrap_or_else(|_| panic!("member {id} never held {expected:?}"))
            .unwrap();
    }
}

fn mint() -> PublicKey {
    IssuerKey::generate().public_key()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_insert_reaches_every_member_once() {
    let cluster = Cluster::start().await;
    let leader = cluster.leader().await;
    let follower = (1..=3).find(|&id| id != leader).unwrap();
    let first = mint();
    assert!(cluster.insert(first, json!({"n": 1})).await);
    // A second insert of the same mint keeps the first record.
    assert!(!cluster.insert(first, json!({"n": 2})).await);
    let expected = Records::from([(first, json!({"n": 1}))]);
    for id in 1..=3 {
        cluster.holds(id, &expected).await;
        assert!(!Registry::is_new(cluster.dir(id)));
    }
    assert_eq!(
        cluster.members[&follower]
            .insert(mint(), json!({}))
            .await
            .unwrap_err(),
        ReplicaError::NotLeader {
            leader: Some(leader),
            node: Some(BasicNode::new("")),
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_that_missed_inserts_catches_up() {
    let mut cluster = Cluster::start().await;
    let leader = cluster.leader().await;
    let first = mint();
    assert!(cluster.insert(first, json!(1)).await);
    let mut expected = Records::from([(first, json!(1))]);
    for id in 1..=3 {
        cluster.holds(id, &expected).await;
    }

    // A follower stops; the other two keep a majority.
    let stopped = (1..=3).find(|&id| id != leader).unwrap();
    cluster.stop(stopped).await;
    // Its records stay on disk: it opens knowing them, before any leader.
    for n in 2..6 {
        let mint = mint();
        assert!(cluster.insert(mint, json!(n)).await);
        expected.insert(mint, json!(n));
    }
    cluster.open(stopped).await;
    assert_eq!(
        *cluster.members[&stopped].records().borrow(),
        Records::from([(first, json!(1))])
    );
    // The log it missed was purged into snapshots, so it gets one.
    cluster.holds(stopped, &expected).await;

    // Restarted, a member knows every record at once.
    cluster.stop(stopped).await;
    cluster.open(stopped).await;
    assert_eq!(*cluster.members[&stopped].records().borrow(), expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_leader_carries_on_and_an_open_registry_is_locked() {
    let mut cluster = Cluster::start().await;
    let first = cluster.leader().await;
    let one = mint();
    assert!(cluster.insert(one, json!(1)).await);
    cluster.stop(first).await;
    let two = mint();
    assert!(cluster.insert(two, json!(2)).await);
    assert_ne!(cluster.leader().await, first);
    let expected = Records::from([(one, json!(1)), (two, json!(2))]);
    for &id in cluster.members.keys() {
        cluster.holds(id, &expected).await;
    }

    let open = *cluster.members.keys().next().unwrap();
    let network = Network {
        router: cluster.router.clone(),
    };
    let error = Registry::open(open, cluster.dir(open), default_config(), network)
        .await
        .unwrap_err();
    assert!(
        matches!(error, OpenError::Store(StoreError::Locked)),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leader_that_restarts_alone_adds_nothing() {
    let mut cluster = Cluster::start().await;
    let leader = cluster.leader().await;
    for id in 1..=3 {
        cluster.stop(id).await;
    }

    // It takes up its term as leader again, but no majority confirms it, so
    // nothing reaches its log to commit once the others are back.
    cluster.open(leader).await;
    assert_eq!(cluster.leader().await, leader);
    let error = cluster.members[&leader]
        .insert(mint(), json!(1))
        .await
        .unwrap_err();
    assert_eq!(error, ReplicaError::Timeout);
    for id in (1..=3).filter(|&id| id != leader) {
        cluster.open(id).await;
    }
    let mint = mint();
    assert!(cluster.insert(mint, json!(2)).await);
    let expected = Records::from([(mint, json!(2))]);
    for id in 1..=3 {
        cluster.holds(id, &expected).await;
    }
}
