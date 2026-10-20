//! End-to-end tests of servers running as a cluster: three members in this
//! process, each with its own state directory and ports, talking HTTP.

use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use worgl::{
    HolderKey, LedgerConfig, MintKey, MintRequest, Note, Output, Policy, PublicKey, TransferRequest,
};
use worgl_server::{
    AppState, ClusterConfig, Member, MintSpec, Verified, create_mint, init_state, join_cluster,
    raft_router, router,
};

const LEDGER: LedgerConfig = LedgerConfig {
    layers: 4,
    layer_size: 1024,
};

/// How long a test waits for the members to agree, or for a mint to have a
/// leader again.
const PATIENCE: Duration = Duration::from_secs(30);

/// A mint the tests created, with the keys to run it.
struct TestMint {
    key: PublicKey,
    token: String,
    authority: MintKey,
    sequence: AtomicU64,
}

impl TestMint {
    /// A mint request signed with the next sequence number.
    fn request(&self, amount: u64, holder: &HolderKey) -> MintRequest {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        MintRequest::sign(
            &self.authority,
            &self.key,
            sequence,
            amount,
            holder.public_key(),
        )
    }

    fn path(&self, route: &str) -> String {
        format!("/v1/mints/{}/{route}", self.key)
    }
}

/// A server state directory with one mint, made offline.
fn seed(rotate_every: Option<u64>) -> (tempfile::TempDir, TestMint) {
    seed_with(Policy::cash_like(), rotate_every)
}

/// A server state directory with one mint of `policy`, made offline.
fn seed_with(mut policy: Policy, rotate_every: Option<u64>) -> (tempfile::TempDir, TestMint) {
    let dir = tempfile::tempdir().unwrap();
    init_state(dir.path()).unwrap();
    let authority = MintKey::generate();
    policy.mint_authorities.push(authority.public_key());
    let spec = MintSpec {
        ledger: LEDGER,
        policy,
        rotate_every,
    };
    let created = create_mint(dir.path(), &spec).unwrap();
    let mint = TestMint {
        key: created.mint,
        token: created.token,
        authority,
        sequence: AtomicU64::new(0),
    };
    (dir, mint)
}

/// Serves `app` on `listener` until `stop` fires.
fn serve(
    listener: TcpListener,
    app: axum::Router,
    stop: oneshot::Receiver<()>,
) -> JoinHandle<std::io::Result<()>> {
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop.await;
            })
            .await
    })
}

/// One member: where it keeps its state and listens, and while it runs,
/// what serves it.
struct Server {
    dir: PathBuf,
    api: SocketAddr,
    raft: SocketAddr,
    running: Option<Running>,
}

struct Running {
    state: AppState,
    stop_api: oneshot::Sender<()>,
    stop_raft: oneshot::Sender<()>,
    api: JoinHandle<std::io::Result<()>>,
    raft: JoinHandle<std::io::Result<()>>,
}

impl Server {
    async fn start_on(&mut self, api: TcpListener, raft: TcpListener) {
        let state = AppState::open_member(&self.dir, 1000).await.unwrap();
        let (stop_api, api_stopped) = oneshot::channel();
        let (stop_raft, raft_stopped) = oneshot::channel();
        self.running = Some(Running {
            api: serve(api, router(state.clone()), api_stopped),
            raft: serve(raft, raft_router(state.clone()), raft_stopped),
            state,
            stop_api,
            stop_raft,
        });
    }

    /// Starts the member again on the ports it had.
    async fn restart(&mut self) {
        let api = TcpListener::bind(self.api).await.unwrap();
        let raft = TcpListener::bind(self.raft).await.unwrap();
        self.start_on(api, raft).await;
    }

    /// Stops the member as `worgl-server serve` does: its public port, then
    /// its replicas, then its Raft port.
    async fn stop(&mut self) {
        let running = self.running.take().expect("the member is running");
        let _ = running.stop_api.send(());
        tokio::time::timeout(PATIENCE, running.api)
            .await
            .expect("the public port closes")
            .unwrap()
            .unwrap();
        running.state.shutdown().await;
        let _ = running.stop_raft.send(());
        tokio::time::timeout(PATIENCE, running.raft)
            .await
            .expect("the Raft port closes")
            .unwrap()
            .unwrap();
    }

    fn state(&self) -> Option<&AppState> {
        self.running.as_ref().map(|running| &running.state)
    }
}

/// Three members, started from one server state directory.
struct Cluster {
    servers: Vec<Server>,
    operator: String,
    client: reqwest::Client,
    _seed: tempfile::TempDir,
    _copies: tempfile::TempDir,
}

impl Cluster {
    /// Makes the server state in `seed` the first member, as an operator
    /// would: join it to the cluster, then copy it to the other members.
    async fn from_seed(seed: tempfile::TempDir) -> Self {
        let mut listeners = Vec::new();
        let mut members = BTreeMap::new();
        for id in 1..=3 {
            let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let raft = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let member = Member {
                api: api.local_addr().unwrap().to_string(),
                raft: raft.local_addr().unwrap().to_string(),
            };
            members.insert(id, member);
            listeners.push((id, api, raft));
        }
        let config = |node| ClusterConfig {
            node,
            members: members.clone(),
        };
        join_cluster(seed.path(), &config(1)).unwrap();
        // Every copy is made before any member starts.
        let copies = tempfile::tempdir().unwrap();
        let mut servers = Vec::new();
        for (id, api, raft) in &listeners {
            let dir = if *id == 1 {
                seed.path().to_owned()
            } else {
                let dir = copies.path().join(id.to_string());
                copy_dir(seed.path(), &dir);
                join_cluster(&dir, &config(*id)).unwrap();
                dir
            };
            servers.push(Server {
                dir,
                api: api.local_addr().unwrap(),
                raft: raft.local_addr().unwrap(),
                running: None,
            });
        }
        for (server, (_, api, raft)) in servers.iter_mut().zip(listeners) {
            server.start_on(api, raft).await;
        }
        let operator = read_token(&seed.path().join("operator.token"));
        Self {
            servers,
            operator,
            client: reqwest::Client::new(),
            _seed: seed,
            _copies: copies,
        }
    }

    /// The member leading `mint`, once every running member knows it.
    async fn leader(&self, mint: &PublicKey) -> usize {
        self.agreed(|state| state.leader(mint)).await
    }

    /// The member leading the registry, once every running member knows it.
    async fn registry_leader(&self) -> usize {
        self.agreed(AppState::registry_leader).await
    }

    /// The leader every running member names in `view`.
    async fn agreed(&self, view: impl Fn(&AppState) -> Option<u64>) -> usize {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let views: Vec<_> = self
                .servers
                .iter()
                .filter_map(Server::state)
                .map(&view)
                .collect();
            if let Some(&Some(leader)) = views.first()
                && views.iter().all(|&view| view == Some(leader))
            {
                let index = usize::try_from(leader - 1).unwrap();
                if self.servers[index].running.is_some() {
                    return index;
                }
            }
            assert!(Instant::now() < deadline, "no agreed leader: {views:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Creates a mint through `server`, sending the request again only
    /// while it surely made nothing.
    async fn create(&self, server: usize) -> TestMint {
        let authority = MintKey::generate();
        let spec = json!({
            "ledger": LEDGER,
            "policy": {"mint_authorities": [authority.public_key()]},
        });
        let deadline = Instant::now() + PATIENCE;
        let answer = loop {
            let (status, answer) = self
                .call_once(
                    server,
                    Method::POST,
                    "/v1/admin/mints",
                    Some(&self.operator),
                    &spec,
                )
                .await;
            if status == StatusCode::SERVICE_UNAVAILABLE
                && answer["error"] == "no_leader"
                && Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            assert_eq!(status, StatusCode::OK, "{answer}");
            break answer;
        };
        TestMint {
            key: answer["mint"].as_str().unwrap().parse().unwrap(),
            token: answer["token"].as_str().unwrap().to_owned(),
            authority,
            sequence: AtomicU64::new(0),
        }
    }

    /// Waits until every running member serves `mint`, and checks that each
    /// wrote it with the token its creator was given.
    async fn hosts(&self, mint: &TestMint) {
        let deadline = Instant::now() + PATIENCE;
        for server in self.running() {
            let state = self.servers[server].state().unwrap();
            while !state.mints().contains(&mint.key) {
                assert!(Instant::now() < deadline, "member {server} lacks the mint");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let dir = self.servers[server].dir.join("mints");
            let token = read_token(&dir.join(mint.key.to_string()).join("admin.token"));
            assert_eq!(token, mint.token);
        }
    }

    /// The running members.
    fn running(&self) -> Vec<usize> {
        (0..self.servers.len())
            .filter(|&index| self.servers[index].running.is_some())
            .collect()
    }

    /// Sends one request to a member's public API, sending it again while
    /// the mint has no leader, and parses the JSON answer.
    async fn call(
        &self,
        server: usize,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let (status, answer) = self
                .call_once(server, method.clone(), path, token, &body)
                .await;
            let leaderless = status == StatusCode::SERVICE_UNAVAILABLE
                && matches!(answer["error"].as_str(), Some("no_leader" | "timeout"));
            if !leaderless || Instant::now() > deadline {
                return (status, answer);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn call_once(
        &self,
        server: usize,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: &Value,
    ) -> (StatusCode, Value) {
        let url = format!("http://{}{path}", self.servers[server].api);
        let mut request = self.client.request(method, url);
        if !body.is_null() {
            request = request.json(body);
        }
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let answer = request.send().await.unwrap();
        let status = answer.status();
        (status, answer.json().await.unwrap_or(Value::Null))
    }

    async fn mint(&self, server: usize, mint: &TestMint, amount: u64, holder: &HolderKey) -> Note {
        let body = serde_json::to_value(mint.request(amount, holder)).unwrap();
        let (status, answer) = self
            .call(server, Method::POST, &mint.path("mint"), None, body)
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        serde_json::from_value(answer["note"].clone()).unwrap()
    }

    async fn transfer(&self, server: usize, request: &TransferRequest) -> (StatusCode, Value) {
        let body = serde_json::to_value(request).unwrap();
        self.call(server, Method::POST, "/v1/transfer", None, body)
            .await
    }

    /// Pays `note` on to a new holder through `server`.
    async fn pay(&self, server: usize, note: &Note, key: &HolderKey) -> (Note, HolderKey) {
        let next = HolderKey::generate();
        let output = Output {
            amount: note.amount(),
            holder: next.public_key(),
        };
        let request = TransferRequest::single(*note, key, output).unwrap();
        let (status, answer) = self.transfer(server, &request).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let note = serde_json::from_value(answer["notes"][0].clone()).unwrap();
        (note, next)
    }

    /// The state of `note` as `server` reports it.
    async fn state_of(&self, server: usize, note: &Note) -> String {
        let (status, answer) = self
            .call(
                server,
                Method::POST,
                "/v1/verify",
                None,
                json!({"note": note}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let verified: Verified = serde_json::from_value(answer.clone()).unwrap();
        assert_eq!(verified.amount, note.amount());
        answer["state"].as_str().unwrap().to_owned()
    }

    async fn info(&self, server: usize, mint: &TestMint) -> Value {
        let (status, answer) = self
            .call(server, Method::GET, &mint.path("info"), None, Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        answer
    }

    /// Stops every member that runs.
    async fn stop(mut self) {
        for server in &mut self.servers {
            if server.running.is_some() {
                server.stop().await;
            }
        }
    }
}

fn read_token(path: &Path) -> String {
    fs::read_to_string(path).unwrap().trim().to_owned()
}

/// Copies a directory tree, file modes included.
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The generation of a mint's last layer, from its `info`.
fn last_generation(info: &Value) -> u64 {
    info["layers"].as_array().unwrap().last().unwrap()["generation"]
        .as_u64()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn any_member_answers_for_the_leader() {
    let (seed, mint) = seed(None);
    let cluster = Cluster::from_seed(seed).await;
    let leader = cluster.leader(&mint.key).await;
    let followers: Vec<_> = (0..3).filter(|&index| index != leader).collect();

    // Followers pass mints, transfers, reads and admin requests on.
    let alice = HolderKey::generate();
    let note = cluster.mint(followers[0], &mint, 8, &alice).await;
    for server in 0..3 {
        assert_eq!(cluster.state_of(server, &note).await, "live");
    }
    let next = HolderKey::generate();
    let output = Output {
        amount: 8,
        holder: next.public_key(),
    };
    let request = TransferRequest::single(note, &alice, output).unwrap();
    let (status, paid) = cluster.transfer(followers[1], &request).await;
    assert_eq!(status, StatusCode::OK, "{paid}");
    // Resent through another member, it is answered as the first time.
    let (status, again) = cluster.transfer(leader, &request).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again, paid);
    let paid: Note = serde_json::from_value(paid["notes"][0].clone()).unwrap();
    for server in 0..3 {
        assert_eq!(cluster.state_of(server, &note).await, "spent");
        assert_eq!(cluster.state_of(server, &paid).await, "live");
    }
    let generation = last_generation(&cluster.info(leader, &mint).await);
    let (status, report) = cluster
        .call(
            followers[0],
            Method::POST,
            &mint.path("rotate"),
            Some(&mint.token),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let info = cluster.info(leader, &mint).await;
    assert_eq!(last_generation(&info), generation + 1);
    for &server in &followers {
        assert_eq!(cluster.info(server, &mint).await, info);
    }

    // A pause holds on every member until the mint resumes.
    for (route, paused) in [("pause", true), ("resume", false)] {
        let (status, answer) = cluster
            .call(
                followers[1],
                Method::POST,
                &mint.path(route),
                Some(&mint.token),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        for server in 0..3 {
            assert_eq!(cluster.info(server, &mint).await["paused"], paused);
        }
        if paused {
            let body = serde_json::to_value(mint.request(8, &alice)).unwrap();
            let (status, answer) = cluster
                .call(followers[0], Method::POST, &mint.path("mint"), None, body)
                .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
            assert_eq!(answer["error"], "paused");
        }
    }
    cluster.mint(followers[0], &mint, 8, &alice).await;

    // A request one member passed on is not passed on again.
    let (status, answer) = {
        let url = format!(
            "http://{}{}",
            cluster.servers[followers[0]].api,
            mint.path("info")
        );
        let answer = cluster
            .client
            .get(url)
            .header("x-worgl-forwarded-by", "9")
            .send()
            .await
            .unwrap();
        (answer.status(), answer.json::<Value>().await.unwrap())
    };
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answer["error"], "no_leader");

    // The Raft port takes the cluster token only.
    let vote = format!(
        "http://{}/raft/{}/vote",
        cluster.servers[leader].raft, mint.key
    );
    for token in [
        None,
        Some(cluster.operator.as_str()),
        Some(mint.token.as_str()),
    ] {
        let mut request = cluster.client.post(&vote).json(&json!({}));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let answer = request.send().await.unwrap();
        assert_eq!(answer.status(), StatusCode::UNAUTHORIZED, "{token:?}");
    }

    // A member's directory does not open as a server on its own.
    let error = AppState::open(&cluster.servers[1].dir, 1000).err().unwrap();
    assert!(error.to_string().contains("open_member"), "{error}");
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_follower_relays_a_busy_answer_with_its_retry_after() {
    // One receipt, kept an hour: longer than the test takes.
    let policy = Policy {
        receipt_capacity: 1,
        receipt_min_age_secs: 3600,
        ..Policy::cash_like()
    };
    let (seed, mint) = seed_with(policy, None);
    let cluster = Cluster::from_seed(seed).await;
    let leader = cluster.leader(&mint.key).await;
    let follower = (leader + 1) % 3;
    let alice = HolderKey::generate();
    let first = cluster.mint(leader, &mint, 8, &alice).await;
    let second = cluster.mint(leader, &mint, 8, &alice).await;
    cluster.pay(leader, &first, &alice).await;

    let output = Output {
        amount: 8,
        holder: HolderKey::generate().public_key(),
    };
    let request = TransferRequest::single(second, &alice, output).unwrap();
    for server in [leader, follower] {
        let url = format!("http://{}/v1/transfer", cluster.servers[server].api);
        let answer = cluster
            .client
            .post(url)
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(answer.status(), StatusCode::SERVICE_UNAVAILABLE);
        let retry_after: u64 = answer.headers()[reqwest::header::RETRY_AFTER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((3500..=3600).contains(&retry_after), "{retry_after}");
        assert_eq!(answer.json::<Value>().await.unwrap()["error"], "busy");
    }
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_leader_takes_over_from_a_stopped_one() {
    let (seed, mint) = seed(None);
    let mut cluster = Cluster::from_seed(seed).await;
    let first = cluster.leader(&mint.key).await;
    let alice = HolderKey::generate();
    let note = cluster.mint(first, &mint, 8, &alice).await;

    cluster.servers[first].stop().await;
    let other = (first + 1) % 3;
    let (paid, bob) = cluster.pay(other, &note, &alice).await;
    let second = cluster.leader(&mint.key).await;
    assert_ne!(second, first);
    // A rotation lets the paid note, now in the last layer, move again.
    let (status, report) = cluster
        .call(
            other,
            Method::POST,
            &mint.path("rotate"),
            Some(&mint.token),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");

    // The stopped member catches up when it starts again: with the second
    // leader stopped too, the group needs it for a majority.
    cluster.servers[first].restart().await;
    cluster.leader(&mint.key).await;
    cluster.servers[second].stop().await;
    let (last, _) = cluster.pay(first, &paid, &bob).await;
    let third = cluster.leader(&mint.key).await;
    assert_ne!(third, second);
    for server in cluster.running() {
        assert_eq!(cluster.state_of(server, &note).await, "spent");
        assert_eq!(cluster.state_of(server, &paid).await, "spent");
        assert_eq!(cluster.state_of(server, &last).await, "live");
    }
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn readiness_names_a_mint_whose_copy_on_a_member_has_stopped() {
    let (seed, mint) = seed(None);
    let cluster = Cluster::from_seed(seed).await;
    cluster.leader(&mint.key).await;
    for server in 0..3 {
        let (status, _) = cluster
            .call_once(server, Method::GET, "/readyz", None, &Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    // The member's replicas stop, as a storage failure stops one, while its
    // public port still answers.
    cluster.servers[0].state().unwrap().shutdown().await;
    let (status, answer) = cluster
        .call_once(0, Method::GET, "/readyz", None, &Value::Null)
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(answer["error"], "storage");
    assert_eq!(
        answer["mints"],
        json!([{"mint": mint.key, "state": "stopped"}])
    );
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mint_that_ran_alone_carries_on_in_a_cluster() {
    let (seed, mint) = seed(None);
    let alice = HolderKey::generate();
    let bob = HolderKey::generate();
    let (note, paid, before) = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let server = serve(
            listener,
            router(AppState::open(seed.path(), 1000).unwrap()),
            stopped,
        );
        let client = reqwest::Client::new();
        let post = |path: String, body: Value| {
            let request = client.post(format!("http://{address}{path}")).json(&body);
            async move {
                let answer = request.send().await.unwrap();
                assert_eq!(answer.status(), StatusCode::OK);
                answer.json::<Value>().await.unwrap()
            }
        };
        let body = serde_json::to_value(mint.request(8, &alice)).unwrap();
        let note: Note =
            serde_json::from_value(post(mint.path("mint"), body).await["note"].clone()).unwrap();
        let rotated = client
            .post(format!("http://{address}{}", mint.path("rotate")))
            .bearer_auth(&mint.token)
            .send()
            .await
            .unwrap();
        assert_eq!(rotated.status(), StatusCode::OK);
        let output = Output {
            amount: 8,
            holder: bob.public_key(),
        };
        let request = TransferRequest::single(note, &alice, output).unwrap();
        let answer = post(
            "/v1/transfer".into(),
            serde_json::to_value(request).unwrap(),
        )
        .await;
        let paid: Note = serde_json::from_value(answer["notes"][0].clone()).unwrap();
        let info = client
            .get(format!("http://{address}{}", mint.path("info")))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        let _ = stop.send(());
        server.await.unwrap().unwrap();
        (note, paid, info)
    };
    released(&seed.path().join("mints").join(mint.key.to_string())).await;

    let cluster = Cluster::from_seed(seed).await;
    let leader = cluster.leader(&mint.key).await;
    assert_eq!(cluster.info(leader, &mint).await, before);
    assert_eq!(cluster.state_of(leader, &note).await, "spent");
    assert_eq!(cluster.state_of(leader, &paid).await, "live");
    // The next request after the last one it answered alone goes through.
    let follower = (leader + 1) % 3;
    let carol = HolderKey::generate();
    cluster.mint(follower, &mint, 4, &carol).await;
    let (last, _) = cluster.pay(follower, &paid, &bob).await;
    assert_eq!(cluster.state_of(follower, &last).await, "live");
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mint_created_through_any_member_is_served_by_all() {
    let (seed, _) = seed(None);
    let cluster = Cluster::from_seed(seed).await;
    let leader = cluster.registry_leader().await;
    let follower = (leader + 1) % 3;
    let other = (leader + 2) % 3;

    // A follower passes the request on to the registry's leader, and every
    // member writes the mint from the registry.
    let mint = cluster.create(follower).await;
    cluster.hosts(&mint).await;
    for server in 0..3 {
        let (status, listed) = cluster
            .call(
                server,
                Method::GET,
                "/v1/admin/mints",
                Some(&cluster.operator),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        assert_eq!(listed["mints"].as_array().unwrap().len(), 2, "{listed}");
    }
    let alice = HolderKey::generate();
    let note = cluster.mint(other, &mint, 8, &alice).await;
    let (paid, _) = cluster.pay(follower, &note, &alice).await;
    for server in 0..3 {
        assert_eq!(cluster.state_of(server, &note).await, "spent");
        assert_eq!(cluster.state_of(server, &paid).await, "live");
    }
    let (status, report) = cluster
        .call(
            other,
            Method::POST,
            &mint.path("rotate"),
            Some(&mint.token),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let info = cluster.info(leader, &mint).await;
    for server in 0..3 {
        assert_eq!(cluster.info(server, &mint).await, info);
    }

    // A creation one member passed on is not passed on again, and makes
    // nothing.
    let answer = cluster
        .client
        .post(format!("http://{}/v1/admin/mints", cluster.servers[follower].api))
        .bearer_auth(&cluster.operator)
        .header("x-worgl-forwarded-by", "9")
        .json(&json!({"ledger": LEDGER, "policy": {"mint_authorities": [mint.authority.public_key()]}}))
        .send()
        .await
        .unwrap();
    assert_eq!(answer.status(), StatusCode::SERVICE_UNAVAILABLE);
    let answer: Value = answer.json().await.unwrap();
    assert_eq!(answer["error"], "no_leader");
    assert_eq!(cluster.servers[leader].state().unwrap().mints().len(), 2);
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_away_while_a_mint_is_created_serves_it_when_back() {
    let (seed, _) = seed(None);
    let mut cluster = Cluster::from_seed(seed).await;
    cluster.registry_leader().await;
    // The first member, which forms the groups of the mints the members
    // start with, is away: the member that creates a mint forms its group.
    cluster.servers[0].stop().await;
    let mint = cluster.create(1).await;
    cluster.hosts(&mint).await;
    let alice = HolderKey::generate();
    let note = cluster.mint(2, &mint, 8, &alice).await;
    let (paid, bob) = cluster.pay(1, &note, &alice).await;
    let (status, report) = cluster
        .call(
            2,
            Method::POST,
            &mint.path("rotate"),
            Some(&mint.token),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let away = cluster.servers[0]
        .dir
        .join("mints")
        .join(mint.key.to_string());
    assert!(!away.exists());

    // Back, it writes the mint, joins its group and catches up: with
    // another member stopped, the group needs it for a majority.
    cluster.servers[0].restart().await;
    cluster.hosts(&mint).await;
    cluster.leader(&mint.key).await;
    cluster.servers[1].stop().await;
    let (last, _) = cluster.pay(0, &paid, &bob).await;
    for server in cluster.running() {
        assert_eq!(cluster.state_of(server, &note).await, "spent");
        assert_eq!(cluster.state_of(server, &paid).await, "spent");
        assert_eq!(cluster.state_of(server, &last).await, "live");
    }
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_registry_leader_creates_mints() {
    let (seed, _) = seed(None);
    let mut cluster = Cluster::from_seed(seed).await;
    let first = cluster.registry_leader().await;
    cluster.servers[first].stop().await;
    let other = (first + 1) % 3;
    let mint = cluster.create(other).await;
    assert_ne!(cluster.registry_leader().await, first);
    cluster.hosts(&mint).await;
    let alice = HolderKey::generate();
    let note = cluster.mint(other, &mint, 8, &alice).await;

    // The member that led learns of the mint from the new leader.
    cluster.servers[first].restart().await;
    cluster.hosts(&mint).await;
    cluster.leader(&mint.key).await;
    for server in 0..3 {
        assert_eq!(cluster.state_of(server, &note).await, "live");
    }
    cluster.stop().await;
}

/// Waits until no store holds the mint directory `dir` open.
async fn released(dir: &Path) {
    let deadline = Instant::now() + PATIENCE;
    let lock = fs::File::open(dir.join("lock")).unwrap();
    while lock.try_lock().is_err() {
        assert!(Instant::now() < deadline, "{} stays locked", dir.display());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Waits until the mint's last layer is of generation `target` or later,
/// and returns that generation.
async fn rotated_to(cluster: &Cluster, mint: &TestMint, target: u64) -> u64 {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let server = cluster.running()[0];
        let now = last_generation(&cluster.info(server, mint).await);
        if now >= target {
            return now;
        }
        assert!(Instant::now() < deadline, "stuck at generation {now}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_leader_rotates_on_schedule_and_its_successor_carries_on() {
    let (seed, mint) = seed(Some(1));
    let mut cluster = Cluster::from_seed(seed).await;
    let leader = cluster.leader(&mint.key).await;
    let start = Instant::now();
    let generation = last_generation(&cluster.info(leader, &mint).await);

    // One rotation a second, whichever members are up, not one per member.
    rotated_to(&cluster, &mint, generation + 2).await;
    cluster.servers[leader].stop().await;
    let other = cluster.running()[0];
    let stopped_at = last_generation(&cluster.info(other, &mint).await);
    let reached = rotated_to(&cluster, &mint, stopped_at + 2).await;
    let elapsed = start.elapsed().as_secs();
    assert!(
        reached - generation <= elapsed + 2,
        "{} rotations in {elapsed} s",
        reached - generation
    );
    cluster.stop().await;
}
