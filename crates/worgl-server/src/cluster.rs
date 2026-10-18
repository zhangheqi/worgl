//! Running a server as one member of a cluster, which replicates every mint
//! across its members with Raft (see [`worgl_raft`]).
//!
//! A member's state directory holds, beside what any server's does (see
//! [`mints`](crate::mints)),
//!
//! - `cluster.json`, this member's id and every member's addresses, as
//!   [`ClusterConfig`], and
//! - `cluster.token`, which members show each other on their Raft ports,
//!   mode 0600.
//!
//! Every member hosts every mint, and each mint is a Raft group of all the
//! members, so each mint has a leader of its own. The members' state
//! directories start as copies of one, with the same mints, operator token,
//! key-encryption key and cluster token, and differ only in `node`. A mint
//! that ran on a server alone carries on from where it was.
//!
//! Members send each other Raft's RPCs as HTTP requests to their Raft ports,
//! with the cluster token. Nothing on that port is encrypted yet, so it must
//! be reachable by the members only.
//!
//! A member that does not lead a mint passes the requests for it on to the
//! leader's public API and relays the answer. If it knows of no leader, or
//! cannot reach the one it knows of, it answers `503 no_leader`; if the
//! leader does not answer, `503 timeout`. Either way the client sends the
//! same request again: a mint, transfer or burn the mint already carried
//! out is answered as it was the first time.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderName, StatusCode, header};
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
use openraft::{BasicNode, RaftNetwork, RaftNetworkFactory, Snapshot, SnapshotMeta, Vote};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use worgl::{Note, PublicKey};
use worgl_raft::{NodeId, Replica, SnapshotData, TypeConfig};

use crate::files::{self, read_json, read_secret, write_secret};
use crate::{ApiError, AppState, BODY_LIMIT, MIN_ADMIN_TOKEN_LEN, check_bearer};

pub(crate) const CLUSTER_FILE: &str = "cluster.json";
const TOKEN_FILE: &str = "cluster.token";

/// Marks a request one member passed on to another, so it is passed on at
/// most once.
const FORWARDED: HeaderName = HeaderName::from_static("x-worgl-forwarded-by");

/// How long a member waits for the leader to answer a request it passed on:
/// longer than the leader waits for its group.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Every member, this one included, by id.
    pub members: BTreeMap<NodeId, Member>,
}

/// Where a member can be reached, each as `host:port`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// Its public API, which other members pass requests on to.
    pub api: String,
    /// Its Raft port.
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
        Ok(())
    }
}

/// Makes the server state in `state` a cluster member with `config`,
/// creating a cluster token unless it has one. The other members need a copy
/// of the token, and of the rest of the directory, before they first start.
pub fn join_cluster(state: &Path, config: &ClusterConfig) -> Result<()> {
    config.check()?;
    if !state.join(crate::mints::OPERATOR_TOKEN_FILE).exists() {
        bail!("{} holds no server; run `init` first", state.display());
    }
    if !state.join(TOKEN_FILE).exists() {
        write_secret(&state.join(TOKEN_FILE), &files::random_token()?)?;
    }
    files::write_json(state, CLUSTER_FILE, config)
        .with_context(|| format!("writing {}", state.join(CLUSTER_FILE).display()))
}

/// Whether the server state in `state` is a cluster member's.
pub fn is_member(state: &Path) -> bool {
    state.join(CLUSTER_FILE).exists()
}

/// A member's view of its cluster.
pub(crate) struct Cluster {
    config: ClusterConfig,
    token: String,
    client: reqwest::Client,
}

impl Cluster {
    pub(crate) fn open(state: &Path) -> Result<Arc<Self>> {
        let config = ClusterConfig::read(state)?;
        let token = read_secret(&state.join(TOKEN_FILE))?;
        if token.len() < MIN_ADMIN_TOKEN_LEN {
            bail!(
                "{} must hold at least {MIN_ADMIN_TOKEN_LEN} characters",
                state.join(TOKEN_FILE).display()
            );
        }
        let client = reqwest::Client::builder()
            .build()
            .context("building the HTTP client")?;
        Ok(Arc::new(Self {
            config,
            token,
            client,
        }))
    }

    pub(crate) fn node(&self) -> NodeId {
        self.config.node
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
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
    pub(crate) fn network(self: &Arc<Self>, mint: PublicKey) -> Network {
        Network {
            cluster: self.clone(),
            mint,
        }
    }

    /// Forms the group of a mint whose store had never joined one. The first
    /// member does it at once; another waits in case the first does, since
    /// members that start elections together delay each other.
    pub(crate) async fn form(self: Arc<Self>, replica: Arc<Replica>) {
        let first = self.config.members.keys().next().copied();
        if first != Some(self.node()) {
            tokio::time::sleep(FORM_AFTER).await;
            if !matches!(replica.raft().is_initialized().await, Ok(false)) {
                return;
            }
        }
        match replica.initialize(self.members()).await {
            Ok(()) => {
                tracing::info!(mint = %replica.public_key(), "formed the mint's group");
            }
            Err(error) if matches!(*error, RaftError::APIError(InitializeError::NotAllowed(_))) => {
            }
            Err(error) => {
                tracing::error!(mint = %replica.public_key(), %error, "could not form the mint's group");
            }
        }
    }

    /// Sends the request in `parts` and `body` to the member whose public
    /// API is at `api`, and returns its answer.
    async fn relay(&self, api: &str, parts: &Parts, body: Bytes) -> reqwest::Result<Response> {
        let path = parts.uri.path_and_query().map_or("/", |path| path.as_str());
        let mut request = self
            .client
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
        let content_type = answer.headers().get(header::CONTENT_TYPE).cloned();
        let bytes = answer.bytes().await?;
        let mut response = (status, bytes).into_response();
        if let Some(content_type) = content_type {
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, content_type);
        }
        Ok(response)
    }
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
    let by_path = path
        .strip_prefix("/v1/mints/")
        .and_then(|rest| rest.split('/').next())
        .and_then(|key| key.parse::<PublicKey>().ok());
    let by_note = matches!(path, "/v1/verify" | "/v1/transfer" | "/v1/burn");
    if by_path.is_none() && !by_note {
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
    let key = by_path.or_else(|| issuer_of(path, &bytes));
    let replica = key.and_then(|key| state.replica(&key));
    let Some(replica) = replica.filter(|replica| !replica.is_leader()) else {
        return next
            .run(Request::from_parts(parts, Body::from(bytes)))
            .await;
    };
    if parts.headers.contains_key(FORWARDED) {
        return ApiError::no_leader().into_response();
    }
    let leader = replica
        .leader()
        .filter(|&leader| leader != cluster.node())
        .and_then(|leader| cluster.config.members.get(&leader));
    let Some(leader) = leader else {
        return ApiError::no_leader().into_response();
    };
    match cluster.relay(&leader.api, &parts, bytes).await {
        Ok(response) => response,
        Err(error) if error.is_connect() => {
            tracing::warn!(leader = %leader.api, %error, "could not reach the leader");
            ApiError::no_leader().into_response()
        }
        // The leader may have carried the request out.
        Err(error) => {
            tracing::warn!(leader = %leader.api, %error, "the leader did not answer");
            ApiError::timeout().into_response()
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

/// Accepts Raft requests from members only.
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
    match check_bearer(request.headers(), cluster.token(), "cluster") {
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
    Ok(Json(replica.raft().append_entries(rpc).await))
}

pub(crate) async fn vote(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
    Json(rpc): Json<VoteRequest<NodeId>>,
) -> Result<Answer<VoteResponse<NodeId>, RaftError<NodeId>>, ApiError> {
    let replica = state.replica_at(&mint)?;
    Ok(Json(replica.raft().vote(rpc).await))
}

/// What travels before a snapshot's data.
#[derive(Serialize, Deserialize)]
struct SnapshotHeader {
    vote: Vote<NodeId>,
    meta: SnapshotMeta<NodeId, BasicNode>,
}

/// A snapshot request's body: the header's length as a big-endian `u32`,
/// the header as JSON, then [`SnapshotData::to_bytes`].
fn encode_snapshot(
    vote: Vote<NodeId>,
    snapshot: Snapshot<TypeConfig>,
) -> serde_json::Result<Vec<u8>> {
    let header = serde_json::to_vec(&SnapshotHeader {
        vote,
        meta: snapshot.meta,
    })?;
    let data = snapshot.snapshot.to_bytes();
    let mut body = Vec::with_capacity(4 + header.len() + data.len());
    body.extend_from_slice(&(header.len() as u32).to_be_bytes());
    body.extend_from_slice(&header);
    body.extend_from_slice(&data);
    Ok(body)
}

fn decode_snapshot(body: &[u8]) -> Option<(Vote<NodeId>, Snapshot<TypeConfig>)> {
    let len = u32::from_be_bytes(body.get(..4)?.try_into().ok()?) as usize;
    let header: SnapshotHeader = serde_json::from_slice(body.get(4..4 + len)?).ok()?;
    let data = SnapshotData::from_bytes(&body[4 + len..]).ok()?;
    let snapshot = Snapshot {
        meta: header.meta,
        snapshot: Box::new(data),
    };
    Some((header.vote, snapshot))
}

pub(crate) async fn snapshot(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
    body: Bytes,
) -> Result<Answer<SnapshotResponse<NodeId>, Fatal<NodeId>>, ApiError> {
    let replica = state.replica_at(&mint)?;
    let (vote, snapshot) = decode_snapshot(&body).ok_or_else(|| {
        ApiError::new(StatusCode::BAD_REQUEST, "malformed", "unreadable snapshot")
    })?;
    Ok(Json(
        replica.raft().install_full_snapshot(vote, snapshot).await,
    ))
}

/// Sends a mint's Raft RPCs to the other members' Raft ports.
pub(crate) struct Network {
    cluster: Arc<Cluster>,
    mint: PublicKey,
}

impl RaftNetworkFactory<TypeConfig> for Network {
    type Network = Connection;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> Connection {
        Connection {
            cluster: self.cluster.clone(),
            target,
            base: format!("http://{}/raft/{}", node.addr, self.mint),
        }
    }
}

pub(crate) struct Connection {
    cluster: Arc<Cluster>,
    target: NodeId,
    base: String,
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

impl From<Failure> for StreamingError<TypeConfig, Fatal<NodeId>> {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Unreachable(error) => Self::Unreachable(error),
            Failure::Network(error) => Self::Network(error),
        }
    }
}

impl Connection {
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
            .timeout(ttl)
            .send()
            .await
            .map_err(failed)?;
        let status = answer.status();
        if !status.is_success() {
            let text = answer.text().await.unwrap_or_default();
            let error = std::io::Error::other(format!("{route}: {status} {text}"));
            return Err(Failure::Network(NetworkError::new(&error)));
        }
        answer.json().await.map_err(failed)
    }

    fn url(&self, route: &str) -> String {
        format!("{}/{route}", self.base)
    }
}

impl RaftNetwork<TypeConfig> for Connection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
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
        snapshot: Snapshot<TypeConfig>,
        cancel: impl Future<Output = ReplicationClosed> + Send + 'static,
        option: RPCOption,
    ) -> Result<SnapshotResponse<NodeId>, StreamingError<TypeConfig, Fatal<NodeId>>> {
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
