//! An HTTP issuer for the [`worgl`] crate.
//!
//! The server holds one [`PersistentIssuer`] behind a mutex. Signature checks
//! run first, on Tokio's blocking pool. Changes then queue for a single
//! committer thread, which takes everything waiting (up to [`MAX_BATCH`]
//! requests), carries it out as one [`Batch`](worgl::Batch), flushes it to
//! disk once and only then answers. Under load a flush is shared by many
//! requests; with one request at a time it behaves as before. Reads take the
//! mutex on the blocking pool and never see a change before it is on disk.
//!
//! # API
//!
//! All bodies are JSON. Keys and signatures are hex, notes are unpadded
//! URL-safe base64.
//!
//! | method | path                | body                               | answer                 |
//! |--------|---------------------|------------------------------------|------------------------|
//! | GET    | `/healthz`          |                                    | `ok`                   |
//! | GET    | `/v1/info`          |                                    | [`Info`]               |
//! | POST   | `/v1/verify`        | `{"note": …}`                      | [`Verified`]           |
//! | POST   | `/v1/transfer`      | [`TransferRequest`]                | `{"notes": [...]}`     |
//! | POST   | `/v1/admin/mint`    | `{"amount": …, "holder": …}`       | `{"note": …}`          |
//! | POST   | `/v1/admin/rotate`  |                                    | [`RotationReport`]     |
//!
//! Admin routes need `Authorization: Bearer <token>`. Errors answer with a
//! status code and `{"error": code, "message": text}`, where `code` is one
//! of the stable strings listed on [`ApiError::code`].

use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot};
use worgl::issuer::LayerSummary;
use worgl::{
    CommitError, Error, Note, NoteStatus, PersistentIssuer, Policy, PublicKey, RotationReport,
    Serial, Supply, TransferRequest, VerifiedTransfer, Verifier,
};

/// Largest request body accepted, in bytes.
pub const BODY_LIMIT: usize = 64 * 1024;

/// Shortest admin token accepted, in bytes.
pub const MIN_ADMIN_TOKEN_LEN: usize = 32;

/// Most changes the committer carries out with one flush.
pub const MAX_BATCH: usize = 256;

/// Changes that may wait for the committer before senders have to wait too.
const QUEUE: usize = 4096;

/// The admin token is shorter than [`MIN_ADMIN_TOKEN_LEN`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeakAdminToken;

impl std::fmt::Display for WeakAdminToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the admin token must be at least {MIN_ADMIN_TOKEN_LEN} bytes"
        )
    }
}

impl std::error::Error for WeakAdminToken {}

/// Shared state of the HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Mutex<Inner>>,
    changes: mpsc::Sender<Job>,
    verifier: Verifier,
    admin_token: Arc<str>,
    schedule: Arc<RwLock<Option<RotationSchedule>>>,
}

/// When the issuer rotates on its own, as reported by `GET /v1/info`.
///
/// Clients use it to tell when a frozen note can move again and when a note
/// must be refreshed before its layer is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationSchedule {
    /// Seconds between rotations.
    pub period_secs: u64,
    /// Unix time, in seconds, of the next scheduled rotation.
    pub next_at: u64,
}

/// A change for the committer to make.
enum Change {
    Mint { amount: u64, holder: PublicKey },
    Transfer(VerifiedTransfer),
    Rotate,
}

/// What a change produced.
enum Done {
    Notes(Vec<Note>),
    Rotated(RotationReport),
}

struct Job {
    change: Change,
    reply: oneshot::Sender<Result<Done, ApiError>>,
}

struct Inner {
    issuer: PersistentIssuer,
    compact_every: u64,
    /// Journal length at which the next snapshot is written.
    compact_at: u64,
}

impl Inner {
    /// Carries out `changes` as one batch with one flush. A change the issuer
    /// refuses gets its error; if the flush fails, every other change gets a
    /// storage error.
    fn commit(&mut self, changes: Vec<Change>) -> Vec<Result<Done, ApiError>> {
        let mut batch = match self.issuer.batch() {
            Ok(batch) => batch,
            Err(error) => {
                let error = ApiError::from(CommitError::Io(error));
                return changes.iter().map(|_| Err(error.clone())).collect();
            }
        };
        let mut rotated = false;
        let results: Vec<_> = changes
            .into_iter()
            .map(|change| match change {
                Change::Mint { amount, holder } => batch
                    .mint(amount, holder)
                    .map(|note| Done::Notes(vec![note]))
                    .map_err(ApiError::from),
                Change::Transfer(transfer) => batch
                    .transfer(&transfer)
                    .map(Done::Notes)
                    .map_err(ApiError::from),
                Change::Rotate => {
                    rotated = true;
                    Ok(Done::Rotated(batch.rotate()))
                }
            })
            .collect();
        if let Err(error) = batch.commit() {
            tracing::error!("the issuer refuses changes until it is restarted");
            let error = ApiError::from(CommitError::Io(error));
            return results
                .into_iter()
                .map(|result| result.and(Err(error.clone())))
                .collect();
        }
        if rotated {
            self.compact();
        } else {
            self.after_write();
        }
        results
    }

    /// Writes a snapshot once enough events have been journaled.
    fn after_write(&mut self) {
        if self.issuer.journal_len() >= self.compact_at {
            self.compact();
        }
    }

    fn compact(&mut self) {
        match self.issuer.compact() {
            Ok(()) => {
                self.compact_at = self.compact_every;
                tracing::debug!("wrote a fresh snapshot");
            }
            Err(error) => {
                // The journal still holds every event, so nothing is lost.
                // Try again after another batch rather than on every request.
                self.compact_at = self.issuer.journal_len() + self.compact_every;
                tracing::error!(%error, "snapshot failed; the journal keeps growing");
            }
        }
    }
}

impl AppState {
    /// Wraps an opened issuer. `compact_every` is how many journaled events
    /// trigger a fresh snapshot; rotations always do. Refuses an admin token
    /// shorter than [`MIN_ADMIN_TOKEN_LEN`].
    pub fn new(
        issuer: PersistentIssuer,
        admin_token: &str,
        compact_every: u64,
    ) -> Result<Self, WeakAdminToken> {
        if admin_token.len() < MIN_ADMIN_TOKEN_LEN {
            return Err(WeakAdminToken);
        }
        let compact_every = compact_every.max(1);
        let verifier = issuer.issuer().verifier();
        let inner = Arc::new(Mutex::new(Inner {
            issuer,
            compact_every,
            compact_at: compact_every,
        }));
        let (changes, queue) = mpsc::channel(QUEUE);
        let committer = inner.clone();
        thread::Builder::new()
            .name("worgl-committer".into())
            .spawn(move || run_committer(&committer, queue))
            .expect("spawning the committer thread");
        Ok(Self {
            verifier,
            inner,
            changes,
            admin_token: admin_token.into(),
            schedule: Arc::new(RwLock::new(None)),
        })
    }

    /// Records the rotation schedule that `GET /v1/info` reports. The
    /// scheduler calls this whenever it picks its next boundary; `None` means
    /// rotation happens only through the admin API.
    pub fn set_rotation_schedule(&self, schedule: Option<RotationSchedule>) {
        // The lock guards a plain value, so a poisoned one still holds a usable schedule.
        *self.schedule.write().unwrap_or_else(|e| e.into_inner()) = schedule;
    }

    fn rotation_schedule(&self) -> Option<RotationSchedule> {
        *self.schedule.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues a change for the committer and waits until it is on disk.
    async fn submit(&self, change: Change) -> Result<Done, ApiError> {
        let (reply, done) = oneshot::channel();
        self.changes
            .send(Job { change, reply })
            .await
            .map_err(|_| ApiError::internal("the committer has stopped"))?;
        done.await
            .map_err(|_| ApiError::internal("the committer dropped a request"))?
    }

    /// Runs `f` on the blocking pool with the issuer locked.
    async fn locked<T, F>(&self, f: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Inner) -> Result<T, ApiError> + Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut inner = inner
                .lock()
                .map_err(|_| ApiError::internal("issuer state is poisoned by an earlier panic"))?;
            f(&mut inner)
        })
        .await
        .map_err(|_| ApiError::internal("request handler panicked"))?
    }

    /// Rotates and writes a fresh snapshot.
    pub async fn rotate(&self) -> Result<RotationReport, ApiError> {
        match self.submit(Change::Rotate).await? {
            Done::Rotated(report) => Ok(report),
            Done::Notes(_) => unreachable!("a rotation reports a rotation"),
        }
    }

    fn check_admin(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default();
        if bool::from(given.as_bytes().ct_eq(self.admin_token.as_bytes())) {
            Ok(())
        } else {
            Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "admin_unauthorized",
                "missing or wrong admin token",
            ))
        }
    }
}

/// Takes every change waiting, up to [`MAX_BATCH`], commits them together and
/// answers each. Returns once every [`AppState`] is gone.
fn run_committer(inner: &Mutex<Inner>, mut queue: mpsc::Receiver<Job>) {
    while let Some(first) = queue.blocking_recv() {
        let mut jobs = vec![first];
        while jobs.len() < MAX_BATCH {
            match queue.try_recv() {
                Ok(job) => jobs.push(job),
                Err(_) => break,
            }
        }
        let (changes, replies): (Vec<_>, Vec<_>) =
            jobs.into_iter().map(|job| (job.change, job.reply)).unzip();
        let results = match inner.lock() {
            Ok(mut inner) => inner.commit(changes),
            Err(_) => {
                let error = ApiError::internal("issuer state is poisoned by an earlier panic");
                changes.iter().map(|_| Err(error.clone())).collect()
            }
        };
        for (reply, result) in replies.into_iter().zip(results) {
            // The client may have gone away; the change stands regardless.
            let _ = reply.send(result);
        }
    }
}

/// The HTTP routes. See the [crate documentation](crate).
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/info", get(info))
        .route("/v1/verify", post(verify))
        .route("/v1/transfer", post(transfer))
        .route("/v1/admin/mint", post(mint))
        .route("/v1/admin/rotate", post(rotate))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// Answer of `GET /v1/info`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Info {
    /// The issuer's public key.
    pub issuer: PublicKey,
    /// Events applied since the issuer was created.
    pub sequence: u64,
    /// Position new money is minted into.
    pub mint_layer: u32,
    /// Every layer, first to last.
    pub layers: Vec<LayerSummary>,
    /// Value accounting.
    pub supply: Supply,
    /// The rules in force.
    pub policy: Policy,
    /// The rotation schedule, or `null` when rotation happens only through
    /// the admin API.
    #[serde(default)]
    pub rotation: Option<RotationSchedule>,
}

async fn info(State(state): State<AppState>) -> Result<Json<Info>, ApiError> {
    let rotation = state.rotation_schedule();
    state
        .locked(move |inner| {
            let issuer = inner.issuer.issuer();
            Ok(Json(Info {
                issuer: *issuer.public_key(),
                sequence: issuer.sequence(),
                mint_layer: issuer.mint_position(),
                layers: issuer.layers(),
                supply: issuer.supply(),
                policy: issuer.policy().clone(),
                rotation,
            }))
        })
        .await
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
    // Check the signature before taking the lock; the lookup itself is O(1).
    note.verify(state.verifier.issuer())
        .map_err(|error| ApiError::from(Error::Note { input: 0, error }))?;
    state
        .locked(move |inner| {
            let status = inner.issuer.issuer().serial_status(note.serial());
            Ok(Json(Verified {
                serial: note.serial(),
                amount: note.amount(),
                status,
            }))
        })
        .await
}

#[derive(Serialize, Deserialize)]
struct NotesBody {
    notes: Vec<Note>,
}

async fn transfer(
    State(state): State<AppState>,
    body: Result<Json<TransferRequest>, JsonRejection>,
) -> Result<Json<NotesBody>, ApiError> {
    let Json(request) = body?;
    let verifier = state.verifier.clone();
    let verified = tokio::task::spawn_blocking(move || verifier.verify(&request))
        .await
        .map_err(|_| ApiError::internal("verification panicked"))??;
    match state.submit(Change::Transfer(verified)).await? {
        Done::Notes(notes) => Ok(Json(NotesBody { notes })),
        Done::Rotated(_) => unreachable!("a transfer creates notes"),
    }
}

#[derive(Deserialize)]
struct MintBody {
    amount: u64,
    holder: PublicKey,
}

#[derive(Serialize, Deserialize)]
struct NoteBody {
    note: Note,
}

async fn mint(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<MintBody>, JsonRejection>,
) -> Result<Json<NoteBody>, ApiError> {
    state.check_admin(&headers)?;
    let Json(MintBody { amount, holder }) = body?;
    match state.submit(Change::Mint { amount, holder }).await? {
        Done::Notes(mut notes) => Ok(Json(NoteBody {
            note: notes.remove(0),
        })),
        Done::Rotated(_) => unreachable!("a mint creates a note"),
    }
}

async fn rotate(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<RotationReport>, ApiError> {
    state.check_admin(&headers)?;
    let report = state.rotate().await?;
    tracing::info!(?report, "rotated by admin request");
    Ok(Json(report))
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

    /// The HTTP status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// One of: `malformed`, `policy`, `invalid_note`, `duplicate_input`,
    /// `unauthorized` (a holder signature), `admin_unauthorized`, `spent`,
    /// `expired`, `unissued`, `frozen`, `layer_full`, `storage`, `internal`.
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
            Error::WrongKey { .. } | Error::Unauthorized { .. } => {
                (StatusCode::FORBIDDEN, "unauthorized")
            }
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
