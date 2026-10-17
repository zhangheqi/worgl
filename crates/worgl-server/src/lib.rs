//! An HTTP server hosting many mints, built on the [`worgl`] crate.
//!
//! Each mint is one issuer: its own key, ledger, policy and rotation clock,
//! in its own directory (see [`mints`] for the layout). Holder routes take
//! no mint in their path: a note names its issuer, so `/v1/verify`,
//! `/v1/transfer` and `/v1/burn` go to the mint whose key signed the note,
//! or the first input note.
//!
//! Signature checks run first, on Tokio's blocking pool. Changes then queue
//! for the mint's committer, which takes everything waiting (up to
//! [`MAX_BATCH`] requests), carries it out as one [`Batch`](worgl::Batch),
//! flushes it to disk once and only then answers. Under load a flush is
//! shared by many requests; with one request at a time it behaves as one
//! flush each. Reads never see a change before it is on disk. Mints share
//! nothing but the process, and an idle mint holds no thread.
//!
//! # API
//!
//! All bodies are JSON. Keys and signatures are hex, notes are unpadded
//! URL-safe base64. `{mint}` is a mint's issuer key.
//!
//! | method | path                       | token    | body                  | answer                 |
//! |--------|----------------------------|----------|-----------------------|------------------------|
//! | GET    | `/healthz`                 |          |                       | `ok`                   |
//! | POST   | `/v1/verify`               |          | `{"note": …}`         | [`Verified`]           |
//! | POST   | `/v1/transfer`             |          | [`TransferRequest`]   | `{"notes": [...]}`     |
//! | POST   | `/v1/burn`                 |          | [`BurnRequest`]       | `{"value": …}`         |
//! | GET    | `/v1/mints/{mint}/info`    |          |                       | [`Info`]               |
//! | GET    | `/v1/mints/{mint}/keys`    |          |                       | [`KeySet`]             |
//! | POST   | `/v1/mints/{mint}/mint`    |          | [`MintRequest`]       | `{"note": …}`          |
//! | POST   | `/v1/mints/{mint}/rotate`  | mint     |                       | [`RotationReport`]     |
//! | POST   | `/v1/mints/{mint}/pause`   | mint     |                       | `{"paused": true}`     |
//! | POST   | `/v1/mints/{mint}/resume`  | mint     |                       | `{"paused": false}`    |
//! | GET    | `/v1/admin/mints`          | operator |                       | `{"mints": [...]}`     |
//! | POST   | `/v1/admin/mints`          | operator | [`MintSpec`]          | [`CreatedMint`]        |
//!
//! A mint request needs a signature from a key the mint's policy lists in
//! `mint_authorities`; no token can mint. Routes with a token need
//! `Authorization: Bearer <token>`: a mint's routes take that mint's own
//! admin token, and `/v1/admin` routes the server's operator token. A paused mint refuses transfers, burns, mints
//! and rotations, scheduled or not, so no note expires while its holder
//! cannot refresh it; it still verifies notes. Errors answer with a status
//! code and `{"error": code, "message": text}`, where `code` is one of the
//! stable strings listed on [`ApiError::code`].

pub mod files;
pub mod mints;

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::bail;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use worgl::issuer::LayerSummary;
use worgl::{
    BurnRequest, CommitError, Error, KeySet, MintRequest, Note, NoteStatus, Policy, PublicKey,
    RotationReport, Serial, Supply, TransferRequest,
};

pub use mints::{CreateError, CreatedMint, MintSpec, create_mint, init_state};
use mints::{Mint, Registry};

/// Largest request body accepted, in bytes.
pub const BODY_LIMIT: usize = 64 * 1024;

/// Shortest admin or operator token accepted, in bytes.
pub const MIN_ADMIN_TOKEN_LEN: usize = 32;

/// Most changes a mint's committer carries out with one flush.
pub const MAX_BATCH: usize = 256;

/// Shared state of the HTTP handlers: every mint the server hosts.
#[derive(Clone)]
pub struct AppState {
    dir: Arc<Path>,
    mints: Arc<RwLock<Registry>>,
    operator_token: Arc<str>,
    compact_every: u64,
}

/// When a mint rotates on its own, as reported by its `info`.
///
/// Clients use it to tell when a frozen note can move again and when a note
/// must be refreshed before its layer is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationSchedule {
    /// Seconds between rotations.
    pub period_secs: u64,
    /// Unix time, in seconds, of the next scheduled rotation. A paused mint
    /// skips it.
    pub next_at: u64,
}

impl AppState {
    /// Opens the server state in `dir`, made by [`init_state`], and every
    /// mint in it, starting their committers and rotation clocks. Fails if
    /// any mint does not open, rather than serve without it.
    /// `compact_every` is how many journaled events trigger a fresh snapshot
    /// of a mint; rotations always do. Must be called within a Tokio runtime.
    pub fn open(dir: &Path, compact_every: u64) -> anyhow::Result<Self> {
        if dir.join("issuer.key").exists() {
            bail!(
                "{} holds a single issuer from before mints; run `init` on a new directory \
                 and move this one to its mints/<issuer key>/",
                dir.display()
            );
        }
        let token_file = dir.join(mints::OPERATOR_TOKEN_FILE);
        let operator_token = files::read_secret(&token_file)?;
        if operator_token.len() < MIN_ADMIN_TOKEN_LEN {
            bail!(
                "{} must hold at least {MIN_ADMIN_TOKEN_LEN} characters",
                token_file.display()
            );
        }
        let mut registry = Registry::new();
        for loaded in mints::load_all(dir)? {
            let key = *loaded.key();
            tracing::info!(mint = %key, "mint loaded");
            registry.insert(key, Mint::start(loaded, compact_every));
        }
        Ok(Self {
            dir: dir.into(),
            mints: Arc::new(RwLock::new(registry)),
            operator_token: operator_token.into(),
            compact_every,
        })
    }

    /// The issuer keys of every mint, in order.
    pub fn mints(&self) -> Vec<PublicKey> {
        let mut keys: Vec<_> = self.registry().keys().copied().collect();
        keys.sort_unstable();
        keys
    }

    /// Creates a mint and starts serving it.
    pub async fn create_mint(&self, spec: MintSpec) -> Result<CreatedMint, ApiError> {
        let dir = PathBuf::from(&*self.dir);
        let (created, loaded) = tokio::task::spawn_blocking(move || {
            let created = mints::create_mint(&dir, &spec)?;
            let path = dir.join(mints::MINTS_DIR).join(created.mint.to_string());
            let loaded = mints::load(&path).map_err(CreateError::Storage)?;
            Ok::<_, CreateError>((created, loaded))
        })
        .await
        .map_err(|_| ApiError::internal("creating a mint panicked"))??;
        let mint = Mint::start(loaded, self.compact_every);
        self.mints
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(created.mint, mint);
        tracing::info!(mint = %created.mint, "mint created");
        Ok(created)
    }

    fn registry(&self) -> std::sync::RwLockReadGuard<'_, Registry> {
        // Mints are only ever added, so a poisoned map is still a usable one.
        self.mints.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The mint with issuer key `key`.
    fn mint(&self, key: &PublicKey) -> Result<Arc<Mint>, ApiError> {
        self.registry().get(key).cloned().ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_mint",
                format!("no mint here has the issuer key {key}"),
            )
        })
    }

    /// The mint named in a route's path.
    fn mint_at(&self, path: &str) -> Result<Arc<Mint>, ApiError> {
        match path.parse::<PublicKey>() {
            Ok(key) => self.mint(&key),
            Err(_) => Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_mint",
                format!("{path} is not an issuer key"),
            )),
        }
    }

    fn check_operator(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        check_bearer(headers, &self.operator_token, "operator")
    }
}

/// Accepts the request only if it carries `Authorization: Bearer <token>`.
fn check_bearer(headers: &HeaderMap, token: &str, which: &str) -> Result<(), ApiError> {
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    if bool::from(given.as_bytes().ct_eq(token.as_bytes())) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "admin_unauthorized",
            format!("missing or wrong {which} token"),
        ))
    }
}

/// The HTTP routes. See the [crate documentation](crate).
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/verify", post(verify))
        .route("/v1/transfer", post(transfer))
        .route("/v1/burn", post(burn))
        .route("/v1/mints/{mint}/info", get(info))
        .route("/v1/mints/{mint}/keys", get(keys))
        .route("/v1/mints/{mint}/mint", post(mint))
        .route("/v1/mints/{mint}/rotate", post(rotate))
        .route("/v1/mints/{mint}/pause", post(pause))
        .route("/v1/mints/{mint}/resume", post(resume))
        .route("/v1/admin/mints", get(list_mints).post(new_mint))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// Answer of `GET /v1/mints/{mint}/info`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Info {
    /// The mint's issuer key.
    pub issuer: PublicKey,
    /// Events applied since the mint was created.
    pub sequence: u64,
    /// Position new money is minted into.
    pub mint_layer: u32,
    /// Every layer, first to last.
    pub layers: Vec<LayerSummary>,
    /// Value accounting.
    pub supply: Supply,
    /// The rules in force.
    pub policy: Policy,
    /// The rotation schedule, or `null` when the mint rotates only on
    /// request.
    #[serde(default)]
    pub rotation: Option<RotationSchedule>,
    /// Whether the mint is paused: it then refuses transfers, burns, mints
    /// and rotations, so its notes neither move nor expire.
    #[serde(default)]
    pub paused: bool,
}

async fn info(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
) -> Result<Json<Info>, ApiError> {
    Ok(Json(state.mint_at(&mint)?.info().await?))
}

/// The keys that sign the mint's live generations, certified by its issuer key.
async fn keys(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
) -> Result<Json<KeySet>, ApiError> {
    Ok(Json(state.mint_at(&mint)?.keys()))
}

#[derive(Deserialize)]
struct VerifyBody {
    note: Note,
}

/// Answer of `POST /v1/verify` for a genuine note.
#[derive(Debug, Serialize, Deserialize)]
pub struct Verified {
    /// The note's serial.
    pub serial: Serial,
    /// Its amount.
    pub amount: u64,
    /// Its state, flattened into this object.
    #[serde(flatten)]
    pub status: NoteStatus,
}

async fn verify(
    State(state): State<AppState>,
    body: Result<Json<VerifyBody>, JsonRejection>,
) -> Result<Json<Verified>, ApiError> {
    let Json(VerifyBody { note }) = body?;
    let mint = state.mint(note.issuer())?;
    Ok(Json(mint.verify(note).await?))
}

#[derive(Serialize, Deserialize)]
struct NotesBody {
    notes: Vec<Note>,
}

/// The mint that issued the first input. The issuer checks the others.
fn first_input_mint(state: &AppState, inputs: &[worgl::Spend]) -> Result<Arc<Mint>, ApiError> {
    let first = inputs.first().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed",
            "the request spends no notes",
        )
    })?;
    state.mint(first.note.issuer())
}

async fn transfer(
    State(state): State<AppState>,
    body: Result<Json<TransferRequest>, JsonRejection>,
) -> Result<Json<NotesBody>, ApiError> {
    let Json(request) = body?;
    let mint = first_input_mint(&state, &request.inputs)?;
    let notes = mint.transfer(request).await?;
    Ok(Json(NotesBody { notes }))
}

/// Answer of `POST /v1/burn`: the value destroyed. A resent burn answers
/// with the same value and destroys nothing more.
#[derive(Serialize, Deserialize)]
struct BurnedBody {
    value: u128,
}

async fn burn(
    State(state): State<AppState>,
    body: Result<Json<BurnRequest>, JsonRejection>,
) -> Result<Json<BurnedBody>, ApiError> {
    let Json(request) = body?;
    let mint = first_input_mint(&state, &request.inputs)?;
    let value = mint.burn(request).await?;
    Ok(Json(BurnedBody { value }))
}

#[derive(Serialize, Deserialize)]
struct NoteBody {
    note: Note,
}

async fn mint(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
    body: Result<Json<MintRequest>, JsonRejection>,
) -> Result<Json<NoteBody>, ApiError> {
    let mint = state.mint_at(&mint)?;
    let Json(request) = body?;
    let (authority, sequence, amount) = (request.authority, request.sequence, request.amount);
    let note = mint.issue(request).await?;
    tracing::info!(
        mint = %mint.key(),
        %authority,
        sequence,
        amount,
        "mint request answered"
    );
    Ok(Json(NoteBody { note }))
}

async fn rotate(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Json<RotationReport>, ApiError> {
    let mint = state.mint_at(&mint)?;
    check_bearer(&headers, mint.token(), "mint admin")?;
    let report = mint.rotate().await?;
    tracing::info!(mint = %mint.key(), ?report, "rotated by admin request");
    Ok(Json(report))
}

#[derive(Serialize, Deserialize)]
struct PausedBody {
    paused: bool,
}

async fn pause(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Json<PausedBody>, ApiError> {
    set_paused(&state, &mint, &headers, true).await
}

async fn resume(
    State(state): State<AppState>,
    UrlPath(mint): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Json<PausedBody>, ApiError> {
    set_paused(&state, &mint, &headers, false).await
}

async fn set_paused(
    state: &AppState,
    mint: &str,
    headers: &HeaderMap,
    paused: bool,
) -> Result<Json<PausedBody>, ApiError> {
    let mint = state.mint_at(mint)?;
    check_bearer(headers, mint.token(), "mint admin")?;
    mint.set_paused(paused).await?;
    Ok(Json(PausedBody { paused }))
}

#[derive(Serialize, Deserialize)]
struct MintsBody {
    mints: Vec<PublicKey>,
}

async fn list_mints(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MintsBody>, ApiError> {
    state.check_operator(&headers)?;
    Ok(Json(MintsBody {
        mints: state.mints(),
    }))
}

/// Creates a mint. Its policy must list a mint authority, since the server
/// never holds one and could not mint otherwise.
async fn new_mint(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<MintSpec>, JsonRejection>,
) -> Result<Json<CreatedMint>, ApiError> {
    state.check_operator(&headers)?;
    let Json(spec) = body?;
    if spec.policy.mint_authorities.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed",
            "the policy must list at least one mint authority",
        ));
    }
    Ok(Json(state.create_mint(spec).await?))
}

/// An error answer: a status code and a stable machine-readable code.
#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: &'a str,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn internal(message: &str) -> Self {
        tracing::error!(message, "internal error");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    fn paused() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "paused",
            "the mint is paused; its notes keep their places until it resumes",
        )
    }

    /// The HTTP status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// One of: `malformed`, `policy`, `invalid_note`, `duplicate_input`,
    /// `unauthorized` (a holder's or mint authority's signature),
    /// `unknown_authority`, `admin_unauthorized` (a missing or wrong token),
    /// `unknown_mint`, `paused`, `spent`, `expired`, `unissued`, `frozen`,
    /// `stale_mint`, `layer_full`, `storage`, `internal`.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        let (status, code) = match &error {
            Error::Policy(_) => (StatusCode::BAD_REQUEST, "policy"),
            Error::Note { .. } => (StatusCode::BAD_REQUEST, "invalid_note"),
            Error::DuplicateInput(_) => (StatusCode::BAD_REQUEST, "duplicate_input"),
            Error::WrongKey { .. } | Error::Unauthorized { .. } | Error::MintUnauthorized => {
                (StatusCode::FORBIDDEN, "unauthorized")
            }
            Error::UnknownAuthority(_) => (StatusCode::FORBIDDEN, "unknown_authority"),
            Error::StaleMint { .. } => (StatusCode::CONFLICT, "stale_mint"),
            Error::Spent(_) => (StatusCode::CONFLICT, "spent"),
            Error::Expired(_) => (StatusCode::CONFLICT, "expired"),
            Error::Unissued(_) => (StatusCode::CONFLICT, "unissued"),
            Error::Frozen(_) => (StatusCode::CONFLICT, "frozen"),
            Error::LayerFull { .. } => (StatusCode::SERVICE_UNAVAILABLE, "layer_full"),
            _ => (StatusCode::BAD_REQUEST, "malformed"),
        };
        Self::new(status, code, error.to_string())
    }
}

impl From<CommitError> for ApiError {
    fn from(error: CommitError) -> Self {
        match error {
            CommitError::Rejected(error) => error.into(),
            CommitError::Io(error) => {
                tracing::error!(%error, "journal write failed");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage",
                    "the issuer could not record the change; nothing was applied",
                )
            }
        }
    }
}

impl From<CreateError> for ApiError {
    fn from(error: CreateError) -> Self {
        match error {
            CreateError::Invalid(reason) => Self::new(StatusCode::BAD_REQUEST, "malformed", reason),
            CreateError::Storage(error) => {
                tracing::error!(error = format!("{error:#}"), "creating a mint failed");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage",
                    "the mint could not be written; nothing was created",
                )
            }
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        Self::new(rejection.status(), "malformed", rejection.body_text())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: self.code,
            message: &self.message,
        };
        (self.status, Json(body)).into_response()
    }
}
