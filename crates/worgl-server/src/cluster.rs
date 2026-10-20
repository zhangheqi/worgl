//! Running a server as one member of a cluster, which replicates every mint
//! across its members with Raft (see [`worgl_raft`]).
//!
//! A member's state directory holds, beside what any server's does (see
//! [`mints`](crate::mints)),
//!
//! - `cluster.json`, this member's id and every member's addresses, as
//!   [`ClusterConfig`], and
//! - `cluster.token`, which members show each other on their Raft ports,
//!   mode 0600, and
//! - `registry/`, this member's copy of the cluster's registry (see
//!   [`worgl_raft::registry`]).
//!
//! Every member hosts every mint, and each mint is a Raft group of all the
//! members, so each mint has a leader of its own. The members' state
//! directories start as copies of one, with the same mints, operator token,
//! key-encryption key and cluster token, and differ only in `node`. A mint
//! that ran on a server alone carries on from where it was.
//!
//! The registry, a Raft group of all the members too, lists the mints
//! created while the cluster runs. Its leader creates them: it adds a
//! mint's record, which holds the mint's secrets sealed by the
//! key-encryption key and its first state, and every member writes the
//! mint's directory from the record when it applies it, or when it starts
//! again if it was away, and serves the mint. The registry's leader forms
//! the mint's group and answers once the group has a leader, or after a
//! while if it has none yet.
//!
//! A mint's `policy.json` and `mint.json` are not replicated. Each member
//! reads its own when it starts serving the mint, and nothing compares them:
//! the leader checks requests against its policy and makes scheduled
//! rotations on its period, and the others apply what it logs. Each member
//! also remembers as many recent transfers and burns as its own
//! [`receipt_capacity`](worgl::Policy::receipt_capacity) says, and the
//! leader answers a resent one from what it remembers. The rules in force
//! are therefore the leader's: change them alike on every member and
//! restart each in turn, with the mint paused meanwhile. A member that
//! writes a mint's directory from the registry gets the policy and period
//! the mint was created with, not later edits.
//!
//! Members send each other Raft's RPCs as HTTP requests to their Raft ports,
//! with the cluster token. A mint applies whatever its group's log holds,
//! and whoever shows the token, with a member's seal of its key-encryption
//! key (see below), can append to that log as if they led, so the token is
//! as strong as the mints' issuer keys. Members dial each other in plain
//! HTTP only and nothing between them is encrypted yet, so the token and
//! the seals cross in the clear, and every member's addresses must be
//! reachable only by the members and a proxy in front of them, as over
//! WireGuard between their hosts.
//!
//! A member dials the others at the addresses its `cluster.json` held when
//! it started, for every group alike, not at those a group's membership
//! kept from when it formed. So a member moves to new addresses once every
//! member's `cluster.json` gives them and every member has restarted;
//! [`join_cluster`] says which members' addresses it changes. A member's
//! Raft port refuses, as `421 misdirected`, an RPC that names that member
//! as its sender, so one whose `cluster.json` gives another member an
//! address that leads back to it, under whatever spelling, finds that
//! member unreachable instead of counting its own votes and appends as
//! that member's.
//!
//! Every member must hold the same key-encryption key, since each unseals
//! what the others seal: the keys of their rotations and the secrets in the
//! registry's records. So each RPC also names the member that sent it and
//! carries that member's seal of a fixed value, made once when it starts.
//! The receiver unseals a member's seal once, and remembers whether it
//! opened until that member sends another. A member whose seal does not
//! open has its RPCs refused with `403 kek_mismatch` ("kek.key differs from
//! member N's") before they reach Raft, and both members log an error.
//! Neither applies an entry or snapshot from the other, so no replica stops
//! on a key it cannot unseal: to each other the two members look cut off,
//! and the members that agree carry on while they are a majority. Nor does
//! a member they refuse put a record or rotation sealed under its key into
//! its own log, to commit once they hear from it, even if it led before it
//! restarted: the registry and each mint propose nothing until a majority
//! has answered their leader.
//!
//! A member that does not lead a mint passes the requests for it on to the
//! leader's [`api`](Member::api) address, `Authorization` headers included,
//! and relays the answer. If it knows of no leader, or cannot connect to the
//! one it knows of within a second, it answers `503 no_leader`; if the
//! leader does not answer, or the connection to it goes three seconds
//! without a sign of life, `503 timeout`. A leader cut off from the other
//! members answers `503 no_leader` too once its lease lapses (see
//! [`worgl_raft::default_config`]), and gives up waiting on requests it took
//! before as `503 timeout`, so no client waits on a member that cannot reach
//! the others. Either way the client sends the same request again: a mint,
//! transfer or burn the mint already carried out is answered as it was the
//! first time. A rotation is not, and one answered `503 timeout` may still
//! be made after a read that does not show it, so before rotating again,
//! read the mint's info once it has carried out a change sent after that
//! answer. A request to create a mint goes to the registry's leader the
//! same way, and is not answered as before if sent again either: after a
//! `503 timeout`, list the mints before creating another.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use openraft::error::{
    Fatal, InitializeError, NetworkError, RPCError, RaftError, RemoteError, ReplicationClosed,
    StreamingError, Unreachable,
};
use openraft::network::RPCOption;
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, SnapshotResponse, VoteRequest, VoteResponse,
};
use openraft::{
    BasicNode, RaftNetwork, RaftNetworkFactory, RaftTypeConfig, Snapshot, SnapshotMeta,
    SnapshotPolicy, Vote,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use worgl::{Kek, KekError, Note, PublicKey};
use worgl_raft::registry::{Records, Registry, RegistryConfig};
use worgl_raft::{NodeId, SnapshotData, TypeConfig, default_config};

use crate::files::{self, read_json, read_secret, write_secret};
use crate::{ApiError, AppState, BODY_LIMIT, MIN_ADMIN_TOKEN_LEN, check_bearer};

pub(crate) const CLUSTER_FILE: &str = "cluster.json";
const TOKEN_FILE: &str = "cluster.token";
/// The directory, in a member's state directory, of its copy of the
/// registry.
const REGISTRY_DIR: &str = "registry";

/// Marks a request one member passed on to another, so it is passed on at
/// most once.
const FORWARDED: HeaderName = HeaderName::from_static("x-worgl-forwarded-by");

/// Names the member that sent a Raft request.
const MEMBER: HeaderName = HeaderName::from_static("x-worgl-member");

/// Carries the sender's seal of [`KEK_CHECK`], in hex, which the receiver
/// opens only if the two hold the same key-encryption key.
const KEK_CHECK_HEADER: HeaderName = HeaderName::from_static("x-worgl-kek-check");

/// What each member seals, under [`KEK_CHECK_CONTEXT`], for the others to
/// unseal.
const KEK_CHECK: [u8; 32] = [0; 32];
const KEK_CHECK_CONTEXT: &[u8] = b"worgl/cluster/kek-check/v1";

/// The code of a Raft request refused for its sender's key-encryption key.
const KEK_MISMATCH: &str = "kek_mismatch";

/// The code of a Raft request refused because the receiver could not use
/// its key-encryption key to check the sender's.
const KEK_UNAVAILABLE: &str = "kek_unavailable";

/// How long a member waits for the leader to answer a request it passed on:
/// longer than the leader waits for its group.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a member tries to connect to the leader before it answers
/// `503 no_leader`.
const FORWARD_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// About how long a connection to the leader may go without a sign of life
/// before a member gives the request up as `503 timeout`, as when the
/// network between them is cut. A sign of life is an acknowledgement of
/// what the member sent or of a keepalive probe; probes go a second apart,
/// so a connection waiting on a slow answer stays alive.
const FORWARD_SILENCE: Duration = Duration::from_secs(3);

/// How long a member other than the first waits for the first to form a new
/// mint's group before forming it itself.
const FORM_AFTER: Duration = Duration::from_secs(10);

/// Largest Raft request a member accepts, in bytes: a snapshot holds a whole
/// ledger.
pub const RAFT_BODY_LIMIT: usize = 256 << 20;

/// What `cluster.json` holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// This member's id.
    pub node: NodeId,
    /// Every member, this one included, by id. No two may share an API or a
    /// Raft address.
    pub members: BTreeMap<NodeId, Member>,
}

/// Where the other members reach a member, each as `host:port`, in plain
/// HTTP, so both must be on a network only the members and a proxy in front
/// of them can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// The address other members pass API requests on to, `Authorization`
    /// headers included, so a private one. Clients reach the API at an
    /// address of their own, such as a proxy's in front of every member.
    pub api: String,
    /// Its Raft port, where the other members send the RPCs of the
    /// registry's group and every mint's, whatever address the group formed
    /// with.
    pub raft: String,
}

impl ClusterConfig {
    /// Reads the `cluster.json` in the server state `state`.
    pub fn read(state: &Path) -> Result<Self> {
        let config: Self = read_json(&state.join(CLUSTER_FILE))?;
        config
            .check()
            .with_context(|| format!("in {}", state.join(CLUSTER_FILE).display()))?;
        Ok(config)
    }

    fn check(&self) -> Result<()> {
        if !self.members.contains_key(&self.node) {
            bail!("the members do not include this one, {}", self.node);
        }
        // Members dial each other by these addresses, and Raft's RPCs do not
        // say which member they are for: a member that dialed itself at
        // another's Raft address would count its own votes and appends as
        // that member's, and commit alone. Its Raft port refuses them (see
        // `from_another`) however the address is spelled; this says why,
        // before it starts, when two are spelled alike.
        let mut seen = BTreeMap::new();
        for (&id, member) in &self.members {
            for (kind, address) in [("API", &member.api), ("Raft", &member.raft)] {
                if let Some(other) = seen.insert((kind, address), id) {
                    bail!("members {other} and {id} have the same {kind} address, {address}");
                }
            }
        }
        Ok(())
    }
}

/// Makes the server state in `state` a cluster member with `config`,
/// creating a cluster token unless it has one, and returns the ids of the
/// members whose addresses differ from those in the `cluster.json` it
/// replaces: none if there was no `cluster.json`, or one that does not
/// read. The other members need a copy of the token, and of the rest of the
/// directory, before they first start. Changed addresses take effect once
/// every member's `cluster.json` gives the same ones and every member has
/// restarted. Fails, and writes nothing, if `state` has no readable
/// `kek.key`.
pub fn join_cluster(state: &Path, config: &ClusterConfig) -> Result<Vec<NodeId>> {
    config.check()?;
    if !state.join(crate::mints::OPERATOR_TOKEN_FILE).exists() {
        bail!("{} holds no server; run `init` first", state.display());
    }
    crate::mints::open_kek(state)?;
    if !state.join(TOKEN_FILE).exists() {
        write_secret(&state.join(TOKEN_FILE), &files::random_token()?)?;
    }
    // A `cluster.json` that does not read is replaced all the same.
    let before = read_json::<ClusterConfig>(&state.join(CLUSTER_FILE)).ok();
    let changed = before.map_or_else(Vec::new, |before| {
        config
            .members
            .iter()
            .filter(|&(id, member)| before.members.get(id).is_some_and(|was| was != member))
            .map(|(&id, _)| id)
            .collect()
    });
    files::write_json(state, CLUSTER_FILE, config)
        .with_context(|| format!("writing {}", state.join(CLUSTER_FILE).display()))?;
    Ok(changed)
}

/// Whether the server state in `state` is a cluster member's.
pub fn is_member(state: &Path) -> bool {
    state.join(CLUSTER_FILE).exists()
}

/// A member's view of its cluster.
pub(crate) struct Cluster {
    config: ClusterConfig,
    token: String,
    /// The key-encryption key every member must share.
    kek: Arc<dyn Kek>,
    /// This member's seal of [`KEK_CHECK`], in hex, sent with every RPC.
    kek_check: HeaderValue,
    /// For each member, the last seal it sent and whether it opened here.
    checked: Mutex<BTreeMap<NodeId, (HeaderValue, bool)>>,
    /// The members that refuse this member's seal, as far as it knows.
    refused_by: Mutex<BTreeSet<NodeId>>,
    /// Whether the key-encryption key was unavailable the last time it
    /// checked a seal.
    kek_unavailable: AtomicBool,
    /// For Raft's RPCs, which openraft bounds itself.
    client: reqwest::Client,
    /// For requests passed on to a leader.
    forwarder: reqwest::Client,
}

impl Cluster {
    /// Opens this member's view of its cluster, with `kek` the
    /// key-encryption key it shows the others.
    pub(crate) fn open(state: &Path, kek: Arc<dyn Kek>) -> Result<Arc<Self>> {
        let config = ClusterConfig::read(state)?;
        let token = read_secret(&state.join(TOKEN_FILE))?;
        if token.len() < MIN_ADMIN_TOKEN_LEN {
            bail!(
                "{} must hold at least {MIN_ADMIN_TOKEN_LEN} characters",
                state.join(TOKEN_FILE).display()
            );
        }
        let sealed = kek
            .seal(&KEK_CHECK, KEK_CHECK_CONTEXT)
            .map_err(|error| anyhow!("sealing the key-encryption key check: {error}"))?;
        let kek_check =
            HeaderValue::from_str(&hex::encode(sealed)).expect("hex is a valid header value");
        let client = reqwest::Client::builder()
            .build()
            .context("building the HTTP client")?;
        let forwarder = reqwest::Client::builder()
            .connect_timeout(FORWARD_CONNECT_TIMEOUT)
            .tcp_keepalive(FORWARD_SILENCE / 3)
            .tcp_keepalive_interval(FORWARD_SILENCE / 3)
            .tcp_keepalive_retries(3);
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        let forwarder = forwarder.tcp_user_timeout(FORWARD_SILENCE);
        let forwarder = forwarder.build().context("building the HTTP client")?;
        Ok(Arc::new(Self {
            config,
            token,
            kek,
            kek_check,
            checked: Mutex::default(),
            refused_by: Mutex::default(),
            kek_unavailable: AtomicBool::new(false),
            client,
            forwarder,
        }))
    }

    pub(crate) fn node(&self) -> NodeId {
        self.config.node
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// Accepts a Raft request only from a member whose seal of
    /// [`KEK_CHECK`], in `headers`, this member's key-encryption key opens.
    /// A member's seal is unsealed once, and the outcome kept until it
    /// sends another, as it does when it restarts.
    fn check_member(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        let member = headers
            .get(&MEMBER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<NodeId>().ok())
            .filter(|member| self.config.members.contains_key(member))
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    "the request names no member of this cluster",
                )
            })?;
        let check = headers.get(&KEK_CHECK_HEADER).ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "malformed",
                "the request carries no key-encryption key check",
            )
        })?;
        let known = self
            .checked()
            .get(&member)
            .and_then(|(seal, same)| (seal == check).then_some(*same));
        let same = match known {
            Some(same) => same,
            None => {
                let same = self.opens(check)?;
                let before = self.checked().insert(member, (check.clone(), same));
                // Two requests can check one seal at once; only one logs it.
                let first = before.as_ref().is_none_or(|(seal, _)| seal != check);
                if first && !same {
                    tracing::error!(
                        member,
                        "kek.key differs from member {member}'s; refusing its Raft requests \
                         until every member holds the same key"
                    );
                } else if first && before.is_some_and(|(_, was)| !was) {
                    tracing::info!(member, "member {member} holds this member's kek.key now");
                }
                same
            }
        };
        if same {
            Ok(())
        } else {
            Err(ApiError::new(
                StatusCode::FORBIDDEN,
                KEK_MISMATCH,
                format!("kek.key differs from member {member}'s"),
            ))
        }
    }

    /// Whether `check` is a seal of [`KEK_CHECK`] that this member's
    /// key-encryption key opens.
    fn opens(&self, check: &HeaderValue) -> Result<bool, ApiError> {
        let sealed = hex::decode(check.as_bytes()).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "malformed",
                "the request's key-encryption key check is not hex",
            )
        })?;
        let opened = self.kek.unseal(&sealed, KEK_CHECK_CONTEXT);
        // Logged when it changes, rather than on every request refused.
        let unavailable = matches!(opened, Err(KekError::Unavailable(_)));
        let was = self.kek_unavailable.swap(unavailable, Ordering::Relaxed);
        if was && !unavailable {
            tracing::info!("the key-encryption key is available again");
        }
        match opened {
            Ok(secret) => Ok(*secret == KEK_CHECK),
            Err(KekError::Rejected) => Ok(false),
            // Not a verdict on the sender's key: the sender backs off, as
            // from a member that is down, and the seal is checked again when
            // it next sends it.
            Err(KekError::Unavailable(reason)) => {
                if !was {
                    tracing::error!(
                        %reason,
                        "the key-encryption key is unavailable; refusing the Raft requests \
                         of members whose seal is not checked yet"
                    );
                }
                Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    KEK_UNAVAILABLE,
                    "this member's key-encryption key is unavailable",
                ))
            }
        }
    }

    fn checked(&self) -> MutexGuard<'_, BTreeMap<NodeId, (HeaderValue, bool)>> {
        // The map holds plain values, so a poisoned one is still usable.
        self.checked.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Notes whether `member` refused this member's seal in answer to an
    /// RPC, and logs when that changes.
    fn answered(&self, member: NodeId, refused: bool) {
        let mut refused_by = self.refused_by.lock().unwrap_or_else(|e| e.into_inner());
        if refused {
            if refused_by.insert(member) {
                tracing::error!(
                    member,
                    "kek.key differs from member {member}'s; it refuses this member's Raft \
                     requests until every member holds the same key"
                );
            }
        } else if refused_by.remove(&member) {
            tracing::info!(member, "member {member} accepts this member's kek.key now");
        }
    }

    /// Every member as Raft knows it: by id, at its Raft port.
    pub(crate) fn members(&self) -> BTreeMap<NodeId, BasicNode> {
        self.config
            .members
            .iter()
            .map(|(&id, member)| (id, BasicNode::new(&member.raft)))
            .collect()
    }

    /// The Raft RPCs a mint's replica sends.
    pub(crate) fn network(self: &Arc<Self>, mint: PublicKey) -> Network<TypeConfig> {
        Network::new(self, format!("raft/{mint}"))
    }

    /// The Raft RPCs this member's copy of the registry sends.
    pub(crate) fn registry_network(self: &Arc<Self>) -> Network<RegistryConfig> {
        Network::new(self, "raft/registry".into())
    }

    /// Whether this is the member with the lowest id, which forms the groups
    /// of the stores the members start with.
    pub(crate) fn is_first(&self) -> bool {
        self.config.members.keys().next() == Some(&self.node())
    }

    /// Forms a group, `what`, whose members' stores had never joined one: at
    /// once if `at_once`, as one member only should, or else after a while
    /// and only if no member has formed it by then, since members that start
    /// elections together delay each other.
    pub(crate) async fn form<C>(
        self: Arc<Self>,
        raft: openraft::Raft<C>,
        what: String,
        at_once: bool,
    ) where
        C: RaftTypeConfig<NodeId = NodeId, Node = BasicNode>,
    {
        if !at_once {
            tokio::time::sleep(FORM_AFTER).await;
            if !matches!(raft.is_initialized().await, Ok(false)) {
                return;
            }
        }
        match raft.initialize(self.members()).await {
            Ok(()) => tracing::info!(group = what, "formed the group"),
            Err(RaftError::APIError(InitializeError::NotAllowed(_))) => {}
            Err(error) => {
                tracing::error!(group = what, %error, "could not form the group");
            }
        }
    }

    /// Sends the request in `parts` and `body` to the member whose API
    /// address is `api`, and returns its answer: the status, the body, and
    /// the body's type and `Retry-After`, if any.
    async fn relay(&self, api: &str, parts: &Parts, body: Bytes) -> reqwest::Result<Response> {
        let path = parts.uri.path_and_query().map_or("/", |path| path.as_str());
        let mut request = self
            .forwarder
            .request(parts.method.clone(), format!("http://{api}{path}"))
            .timeout(FORWARD_TIMEOUT)
            .header(FORWARDED, self.node())
            .body(body);
        for name in [header::CONTENT_TYPE, header::AUTHORIZATION] {
            if let Some(value) = parts.headers.get(&name) {
                request = request.header(name, value.clone());
            }
        }
        let answer = request.send().await?;
        let status = answer.status();
        let kept: Vec<_> = [header::CONTENT_TYPE, header::RETRY_AFTER]
            .into_iter()
            .filter_map(|name| {
                let value = answer.headers().get(&name)?.clone();
                Some((name, value))
            })
            .collect();
        let bytes = answer.bytes().await?;
        let mut response = (status, bytes).into_response();
        for (name, value) in kept {
            response.headers_mut().insert(name, value);
        }
        Ok(response)
    }
}

/// Opens this member's copy of the cluster's registry, in its state
/// directory `state`, and forms the registry's group if it never was.
pub(crate) async fn open_registry(state: &Path, cluster: &Arc<Cluster>) -> Result<Arc<Registry>> {
    let dir = state.join(REGISTRY_DIR);
    let new = Registry::is_new(&dir);
    let config = openraft::Config {
        // Records are few and the log is rewritten whole on every change,
        // so it is kept short.
        snapshot_policy: SnapshotPolicy::LogsSinceLast(32),
        max_in_snapshot_log_to_keep: 32,
        ..default_config()
    };
    let registry = Registry::open(cluster.node(), &dir, config, cluster.registry_network())
        .await
        .with_context(|| format!("opening the registry in {}", dir.display()))?;
    let registry = Arc::new(registry);
    if new {
        let raft = registry.raft().clone();
        tokio::spawn(
            cluster
                .clone()
                .form(raft, "registry".into(), cluster.is_first()),
        );
    }
    Ok(registry)
}

/// Passes a request for a mint this member does not lead on to the member
/// that does. See the [module documentation](self).
pub(crate) async fn forward(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(cluster) = state.cluster() else {
        return next.run(request).await;
    };
    let (parts, body) = request.into_parts();
    let path = parts.uri.path();
    let creates = parts.method == Method::POST && path == "/v1/admin/mints";
    let by_path = path
        .strip_prefix("/v1/mints/")
        .and_then(|rest| rest.split('/').next())
        .and_then(|key| key.parse::<PublicKey>().ok());
    let by_note = matches!(path, "/v1/verify" | "/v1/transfer" | "/v1/burn");
    if by_path.is_none() && !by_note && !creates {
        return next.run(Request::from_parts(parts, body)).await;
    }
    let Ok(bytes) = axum::body::to_bytes(body, BODY_LIMIT).await else {
        return ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "malformed",
            "the request body is too large",
        )
        .into_response();
    };
    // Whether this member leads the group that decides the request, and
    // which member does as far as it knows: the registry's for a new mint,
    // the mint's for the rest.
    let group = if creates {
        state
            .registry()
            .map(|registry| (registry.is_leader(), registry.leader()))
    } else {
        by_path
            .or_else(|| issuer_of(path, &bytes))
            .and_then(|key| state.replica(&key))
            .map(|replica| (replica.is_leader(), replica.leader()))
    };
    let Some((false, leader)) = group else {
        return next
            .run(Request::from_parts(parts, Body::from(bytes)))
            .await;
    };
    let no_leader = || {
        if creates {
            ApiError::no_registry_leader()
        } else {
            ApiError::no_leader()
        }
    };
    if parts.headers.contains_key(FORWARDED) {
        return no_leader().into_response();
    }
    let leader = leader
        .filter(|&leader| leader != cluster.node())
        .and_then(|leader| cluster.config.members.get(&leader));
    let Some(leader) = leader else {
        return no_leader().into_response();
    };
    match cluster.relay(&leader.api, &parts, bytes).await {
        Ok(response) => response,
        Err(error) if error.is_connect() => {
            tracing::warn!(leader = %leader.api, %error, "could not reach the leader");
            no_leader().into_response()
        }
        // The leader may have carried the request out.
        Err(error) => {
            tracing::warn!(leader = %leader.api, %error, "the leader did not answer");
            if creates {
                ApiError::registry_timeout().into_response()
            } else {
                ApiError::timeout().into_response()
            }
        }
    }
}

#[derive(Deserialize)]
struct NoteOnly {
    note: Note,
}

#[derive(Deserialize)]
struct Inputs {
    inputs: Vec<NoteOnly>,
}

/// The mint a holder route's request goes to: the note's issuer, or the
/// first input's. A body that does not parse goes to no mint here, so the
/// handler can say what is wrong with it.
fn issuer_of(path: &str, body: &[u8]) -> Option<PublicKey> {
    let note = if path == "/v1/verify" {
        serde_json::from_slice::<NoteOnly>(body).ok()?.note
    } else {
        serde_json::from_slice::<Inputs>(body)
            .ok()?
            .inputs
            .into_iter()
            .next()?
            .note
    };
    Some(*note.issuer())
}

/// Accepts Raft requests from members only, and only from those that hold
/// this member's key-encryption key.
pub(crate) async fn members_only(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(cluster) = state.cluster() else {
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_mint",
            "this server is not a cluster member",
        )
        .into_response();
    };
    let checked = check_bearer(request.headers(), cluster.token(), "cluster")
        .and_then(|()| cluster.check_member(request.headers()));
    match checked {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

type Answer<T, E> = Json<Result<T, E>>;

pub(crate) async fn append(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
    Json(rpc): Json<AppendEntriesRequest<TypeConfig>>,
) -> Result<Answer<AppendEntriesResponse<NodeId>, RaftError<NodeId>>, ApiError> {
    let replica = state.replica_at(&mint)?;
    from_another(&state, &rpc.vote)?;
    Ok(Json(replica.raft().append_entries(rpc).await))
}

pub(crate) async fn vote(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
    Json(rpc): Json<VoteRequest<NodeId>>,
) -> Result<Answer<VoteResponse<NodeId>, RaftError<NodeId>>, ApiError> {
    let replica = state.replica_at(&mint)?;
    from_another(&state, &rpc.vote)?;
    Ok(Json(replica.raft().vote(rpc).await))
}

pub(crate) async fn snapshot(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
    body: Bytes,
) -> Result<Answer<SnapshotResponse<NodeId>, Fatal<NodeId>>, ApiError> {
    let replica = state.replica_at(&mint)?;
    let (vote, snapshot) = decode_snapshot(&body)?;
    from_another(&state, &vote)?;
    Ok(Json(
        replica.raft().install_full_snapshot(vote, snapshot).await,
    ))
}

pub(crate) async fn registry_append(
    State(state): State<AppState>,
    Json(rpc): Json<AppendEntriesRequest<RegistryConfig>>,
) -> Result<Answer<AppendEntriesResponse<NodeId>, RaftError<NodeId>>, ApiError> {
    let registry = state.registry().ok_or_else(not_member)?;
    from_another(&state, &rpc.vote)?;
    Ok(Json(registry.raft().append_entries(rpc).await))
}

pub(crate) async fn registry_vote(
    State(state): State<AppState>,
    Json(rpc): Json<VoteRequest<NodeId>>,
) -> Result<Answer<VoteResponse<NodeId>, RaftError<NodeId>>, ApiError> {
    let registry = state.registry().ok_or_else(not_member)?;
    from_another(&state, &rpc.vote)?;
    Ok(Json(registry.raft().vote(rpc).await))
}

pub(crate) async fn registry_snapshot(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Answer<SnapshotResponse<NodeId>, Fatal<NodeId>>, ApiError> {
    let registry = state.registry().ok_or_else(not_member)?;
    let (vote, snapshot) = decode_snapshot(&body)?;
    from_another(&state, &vote)?;
    Ok(Json(
        registry.raft().install_full_snapshot(vote, snapshot).await,
    ))
}

fn not_member() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "unknown_mint",
        "this server is not a cluster member",
    )
}

/// Refuses a Raft RPC whose `vote` names this member as its sender, as one
/// does that this member sent itself, dialing another member at an address
/// in `cluster.json` that leads back here under another spelling. Answered,
/// it would count as that member's vote or append and let this member make
/// a majority alone; refused, that member is unreachable from this one.
fn from_another(state: &AppState, vote: &Vote<NodeId>) -> Result<(), ApiError> {
    let node = state.cluster().ok_or_else(not_member)?.node();
    if vote.leader_id().voted_for() != Some(node) {
        return Ok(());
    }
    Err(ApiError::new(
        StatusCode::MISDIRECTED_REQUEST,
        "misdirected",
        format!(
            "member {node} sent this Raft request to itself: its cluster.json gives another \
             member an address that leads back to it"
        ),
    ))
}

/// A snapshot as it crosses the network.
pub(crate) trait Wire: Sized {
    fn to_wire(&self) -> Vec<u8>;
    fn from_wire(bytes: &[u8]) -> Option<Self>;
}

impl Wire for SnapshotData {
    fn to_wire(&self) -> Vec<u8> {
        self.to_bytes()
    }

    fn from_wire(bytes: &[u8]) -> Option<Self> {
        Self::from_bytes(bytes).ok()
    }
}

impl Wire for Records {
    fn to_wire(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("records serialize")
    }

    fn from_wire(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// What travels before a snapshot's data.
#[derive(Serialize, Deserialize)]
struct SnapshotHeader {
    vote: Vote<NodeId>,
    meta: SnapshotMeta<NodeId, BasicNode>,
}

/// A snapshot request's body: the header's length as a big-endian `u32`,
/// the header as JSON, then the data as [`Wire`] encodes it.
fn encode_snapshot<C>(vote: Vote<NodeId>, snapshot: Snapshot<C>) -> serde_json::Result<Vec<u8>>
where
    C: RaftTypeConfig<NodeId = NodeId, Node = BasicNode>,
    C::SnapshotData: Wire,
{
    let header = serde_json::to_vec(&SnapshotHeader {
        vote,
        meta: snapshot.meta,
    })?;
    let data = snapshot.snapshot.to_wire();
    let mut body = Vec::with_capacity(4 + header.len() + data.len());
    body.extend_from_slice(&(header.len() as u32).to_be_bytes());
    body.extend_from_slice(&header);
    body.extend_from_slice(&data);
    Ok(body)
}

fn decode_snapshot<C>(body: &[u8]) -> Result<(Vote<NodeId>, Snapshot<C>), ApiError>
where
    C: RaftTypeConfig<NodeId = NodeId, Node = BasicNode>,
    C::SnapshotData: Wire,
{
    let decode = || {
        let len = u32::from_be_bytes(body.get(..4)?.try_into().ok()?) as usize;
        let header: SnapshotHeader = serde_json::from_slice(body.get(4..4 + len)?).ok()?;
        let data = C::SnapshotData::from_wire(&body[4 + len..])?;
        let snapshot = Snapshot {
            meta: header.meta,
            snapshot: Box::new(data),
        };
        Some((header.vote, snapshot))
    };
    decode()
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "malformed", "unreadable snapshot"))
}

/// Sends one group's Raft RPCs to the other members' Raft ports.
pub(crate) struct Network<C> {
    cluster: Arc<Cluster>,
    /// The group's routes, below each Raft port.
    path: String,
    config: PhantomData<fn() -> C>,
}

impl<C> Network<C> {
    fn new(cluster: &Arc<Cluster>, path: String) -> Self {
        Self {
            cluster: cluster.clone(),
            path,
            config: PhantomData,
        }
    }
}

impl<C> RaftNetworkFactory<C> for Network<C>
where
    C: RaftTypeConfig<NodeId = NodeId, Node = BasicNode>,
    C::SnapshotData: Wire,
{
    type Network = Connection<C>;

    /// Dials `target` at its Raft address in `cluster.json`, not at `node`,
    /// the one the group's membership kept from when it formed, so a member
    /// that moved is reached where it is now. `node` serves only for a
    /// member `cluster.json` does not list.
    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> Connection<C> {
        let raft = self
            .cluster
            .config
            .members
            .get(&target)
            .map_or(&node.addr, |member| &member.raft);
        Connection {
            cluster: self.cluster.clone(),
            target,
            base: format!("http://{raft}/{}", self.path),
            config: PhantomData,
        }
    }
}

pub(crate) struct Connection<C> {
    cluster: Arc<Cluster>,
    target: NodeId,
    base: String,
    config: PhantomData<fn() -> C>,
}

/// Why an RPC got no answer.
enum Failure {
    Unreachable(Unreachable),
    Network(NetworkError),
}

impl From<Failure> for RPCError<NodeId, BasicNode, RaftError<NodeId>> {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Unreachable(error) => Self::Unreachable(error),
            Failure::Network(error) => Self::Network(error),
        }
    }
}

impl<C> From<Failure> for StreamingError<C, Fatal<NodeId>>
where
    C: RaftTypeConfig<NodeId = NodeId, Node = BasicNode>,
{
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Unreachable(error) => Self::Unreachable(error),
            Failure::Network(error) => Self::Network(error),
        }
    }
}

impl<C> Connection<C> {
    async fn post<T: DeserializeOwned>(
        &self,
        route: &str,
        request: reqwest::RequestBuilder,
        ttl: Duration,
    ) -> Result<T, Failure> {
        let failed = |error: reqwest::Error| {
            if error.is_connect() {
                Failure::Unreachable(Unreachable::new(&error))
            } else {
                Failure::Network(NetworkError::new(&error))
            }
        };
        let answer = request
            .bearer_auth(self.cluster.token())
            .header(MEMBER, self.cluster.node())
            .header(KEK_CHECK_HEADER, self.cluster.kek_check.clone())
            .timeout(ttl)
            .send()
            .await
            .map_err(failed)?;
        let status = answer.status();
        if !status.is_success() {
            let text = answer.text().await.unwrap_or_default();
            let error = std::io::Error::other(format!("{route}: {status} {text}"));
            let code = serde_json::from_str::<serde_json::Value>(&text)
                .map(|body| body["error"].clone())
                .unwrap_or_default();
            if status == StatusCode::FORBIDDEN && code == KEK_MISMATCH {
                self.cluster.answered(self.target, true);
                // As good as down until one of the two starts again with the
                // other's key, so openraft backs off as it does for one.
                return Err(Failure::Unreachable(Unreachable::new(&error)));
            }
            if status == StatusCode::SERVICE_UNAVAILABLE && code == KEK_UNAVAILABLE {
                // As good as down until its key-encryption key is back.
                return Err(Failure::Unreachable(Unreachable::new(&error)));
            }
            return Err(Failure::Network(NetworkError::new(&error)));
        }
        self.cluster.answered(self.target, false);
        answer.json().await.map_err(failed)
    }

    fn url(&self, route: &str) -> String {
        format!("{}/{route}", self.base)
    }
}

impl<C> RaftNetwork<C> for Connection<C>
where
    C: RaftTypeConfig<NodeId = NodeId, Node = BasicNode>,
    C::SnapshotData: Wire,
{
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<C>,
        option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        let request = self.cluster.client.post(self.url("append")).json(&rpc);
        let answer: Result<_, RaftError<NodeId>> =
            self.post("append", request, option.hard_ttl()).await?;
        answer.map_err(|error| RemoteError::new(self.target, error).into())
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        let request = self.cluster.client.post(self.url("vote")).json(&rpc);
        let answer: Result<_, RaftError<NodeId>> =
            self.post("vote", request, option.hard_ttl()).await?;
        answer.map_err(|error| RemoteError::new(self.target, error).into())
    }

    async fn full_snapshot(
        &mut self,
        vote: Vote<NodeId>,
        snapshot: Snapshot<C>,
        cancel: impl Future<Output = ReplicationClosed> + Send + 'static,
        option: RPCOption,
    ) -> Result<SnapshotResponse<NodeId>, StreamingError<C, Fatal<NodeId>>> {
        let body = encode_snapshot(vote, snapshot)
            .map_err(|error| Failure::Network(NetworkError::new(&error)))?;
        let request = self.cluster.client.post(self.url("snapshot")).body(body);
        let sent = self.post::<Result<_, Fatal<NodeId>>>("snapshot", request, option.hard_ttl());
        let answer = tokio::select! {
            answer = sent => answer?,
            closed = cancel => return Err(closed.into()),
        };
        answer.map_err(|error| RemoteError::new(self.target, error).into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use worgl::FileKek;
    use zeroize::Zeroizing;

    use super::*;

    /// A file KEK that counts what it unseals, and can be made unavailable.
    #[derive(Debug)]
    struct Counted {
        kek: FileKek,
        unsealed: AtomicUsize,
        available: AtomicBool,
    }

    impl Kek for Counted {
        fn seal(&self, secret: &[u8; 32], context: &[u8]) -> Result<Vec<u8>, KekError> {
            self.kek.seal(secret, context)
        }

        fn unseal(&self, sealed: &[u8], context: &[u8]) -> Result<Zeroizing<[u8; 32]>, KekError> {
            if !self.available.load(Ordering::Relaxed) {
                return Err(KekError::Unavailable("the KMS is down".into()));
            }
            self.unsealed.fetch_add(1, Ordering::Relaxed);
            self.kek.unseal(sealed, context)
        }
    }

    /// Member `node` of a three-member cluster, holding `kek`.
    fn member(node: NodeId, kek: Arc<dyn Kek>) -> (tempfile::TempDir, Arc<Cluster>) {
        let dir = tempfile::tempdir().unwrap();
        crate::init_state(dir.path()).unwrap();
        let members = (1..=3)
            .map(|id| {
                let member = Member {
                    api: format!("127.0.0.1:{}", 8080 + id),
                    raft: format!("127.0.0.1:{}", 7080 + id),
                };
                (id, member)
            })
            .collect();
        join_cluster(dir.path(), &ClusterConfig { node, members }).unwrap();
        let cluster = Cluster::open(dir.path(), kek).unwrap();
        (dir, cluster)
    }

    /// The headers of `sender`'s RPCs.
    fn sent_by(sender: &Cluster) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(MEMBER, sender.node().into());
        headers.insert(KEK_CHECK_HEADER, sender.kek_check.clone());
        headers
    }

    #[test]
    fn a_member_unseals_each_members_check_once_and_refuses_another_kek() {
        let shared = [1; 32];
        let counted = Arc::new(Counted {
            kek: FileKek::from_bytes(&shared),
            unsealed: AtomicUsize::new(0),
            available: AtomicBool::new(true),
        });
        let unsealed = || counted.unsealed.load(Ordering::Relaxed);
        let (_one_dir, one) = member(1, counted.clone());
        let (_two_dir, two) = member(2, Arc::new(FileKek::from_bytes(&shared)));
        let (_three_dir, three) = member(3, Arc::new(FileKek::from_bytes(&[2; 32])));

        // Member 2 holds the same key, which is checked once.
        for _ in 0..3 {
            one.check_member(&sent_by(&two)).unwrap();
        }
        assert_eq!(unsealed(), 1);

        // Member 3 does not: every request is refused, naming it, and each
        // side refuses the other's.
        for _ in 0..3 {
            let error = one.check_member(&sent_by(&three)).unwrap_err();
            assert_eq!(error.status(), StatusCode::FORBIDDEN);
            assert_eq!(error.code(), KEK_MISMATCH);
            assert_eq!(error.message, "kek.key differs from member 3's");
        }
        assert_eq!(unsealed(), 2);
        let error = three.check_member(&sent_by(&one)).unwrap_err();
        assert_eq!(error.message, "kek.key differs from member 1's");

        // Started again with the shared key, it sends a new seal, which opens.
        let (_fixed_dir, fixed) = member(3, Arc::new(FileKek::from_bytes(&shared)));
        one.check_member(&sent_by(&fixed)).unwrap();
        assert_eq!(unsealed(), 3);

        // A key that cannot be used refuses a new seal without a verdict on
        // it, and still takes the seals it checked before.
        counted.available.store(false, Ordering::Relaxed);
        let (_restarted_dir, restarted) = member(2, Arc::new(FileKek::from_bytes(&shared)));
        let error = one.check_member(&sent_by(&restarted)).unwrap_err();
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code(), KEK_UNAVAILABLE);
        one.check_member(&sent_by(&fixed)).unwrap();
        counted.available.store(true, Ordering::Relaxed);
        one.check_member(&sent_by(&restarted)).unwrap();

        // A request must name a member and carry a seal, in hex.
        let mut headers = sent_by(&two);
        headers.insert(MEMBER, 9.into());
        assert_eq!(one.check_member(&headers).unwrap_err().code(), "malformed");
        headers.insert(MEMBER, 2.into());
        headers.insert(KEK_CHECK_HEADER, HeaderValue::from_static("not hex"));
        assert_eq!(one.check_member(&headers).unwrap_err().code(), "malformed");
        headers.remove(KEK_CHECK_HEADER);
        assert_eq!(one.check_member(&headers).unwrap_err().code(), "malformed");
    }
}
