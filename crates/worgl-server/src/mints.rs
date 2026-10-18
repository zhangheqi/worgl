//! The mints a server hosts. Each mint is one issuer with its own directory,
//! committer, verifier and rotation clock, so mints never wait on each other.
//!
//! A server's state directory holds
//!
//! - `operator.token`, which authorizes creating and listing mints,
//! - `kek.key`, the key-encryption key that seals each new generation key
//!   into its rotation's log entry, mode 0600 (a [`FileKek`], for
//!   development; replicas will share one from a KMS), and
//! - `mints/<issuer key>/`, one directory per mint, named by its issuer key
//!   in hex.
//!
//! A mint's directory is a [`PersistentIssuer`] store plus
//!
//! - `issuer.key`, the issuer's secret key, and `admin.token`, which
//!   authorizes rotating and pausing this mint only, both mode 0600,
//! - `policy.json`, the mint's rules, and
//! - `mint.json`, its rotation period.
//!
//! Whether a mint is paused, and when its last scheduled rotation was due,
//! are events in its log, so a replica of the log agrees on both. Older
//! servers kept them in `mint.json` and a `last_rotation` file: a mint from
//! then logs its pause when it opens, and keeps the file as a floor for its
//! schedule until its next scheduled rotation is logged.
//!
//! A new mint is built in a hidden `.new-<issuer key>` directory and renamed
//! into place once complete, so a crash never leaves a mint half made; the
//! server clears such leftovers when it starts.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use worgl::{
    BurnRequest, CommitError, Error, Issuer, IssuerKey, KeySet, LedgerConfig, MintRequest, Note,
    NoteError, PersistentIssuer, Policy, PublicKey, RotationReport, SetupError, TransferRequest,
    VerifiedBurn, VerifiedMint, VerifiedTransfer, Verifier,
};
use worgl::{FileKek, Kek};

use crate::files::{self, read_json, read_key, read_secret, write_secret};
use crate::{ApiError, Info, MAX_BATCH, MIN_ADMIN_TOKEN_LEN, RotationSchedule, Verified};

pub(crate) const OPERATOR_TOKEN_FILE: &str = "operator.token";
pub(crate) const KEK_FILE: &str = "kek.key";
pub(crate) const MINTS_DIR: &str = "mints";
const KEY_FILE: &str = "issuer.key";
const TOKEN_FILE: &str = "admin.token";
const POLICY_FILE: &str = "policy.json";
const SETTINGS_FILE: &str = "mint.json";
const LEGACY_SCHEDULE_FILE: &str = "last_rotation";
const NEW_PREFIX: &str = ".new-";

/// Changes that may wait for a mint's committer before senders have to wait too.
const QUEUE: usize = 4096;

/// What a mint is created with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MintSpec {
    /// The ledger's shape, fixed for the mint's life.
    pub ledger: LedgerConfig,
    /// The mint's rules. Missing fields take their cash-like defaults.
    #[serde(default)]
    pub policy: Policy,
    /// Seconds between scheduled rotations, at multiples of the period since
    /// the Unix epoch. Without it, the mint rotates only on request.
    #[serde(default)]
    pub rotate_every: Option<u64>,
}

/// A mint just created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedMint {
    /// The mint's issuer key, which names it in every route and every note.
    pub mint: PublicKey,
    /// The token that rotates and pauses this mint.
    pub token: String,
}

/// Why a mint could not be created.
#[derive(Debug)]
pub enum CreateError {
    /// The spec asks for a ledger, policy or period the issuer refuses.
    Invalid(String),
    /// Writing the mint's files failed; nothing was created.
    Storage(anyhow::Error),
}

impl fmt::Display for CreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => f.write_str(reason),
            Self::Storage(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for CreateError {}

/// What the server keeps about a mint beside its store, in `mint.json`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    rotate_every: Option<u64>,
    /// Read from older servers' files only; the pause is logged now.
    #[serde(skip_serializing)]
    paused: bool,
}

/// Creates a server's state directory: an operator token, a key-encryption
/// key and no mints.
pub fn init_state(dir: &Path) -> Result<()> {
    if dir.join(OPERATOR_TOKEN_FILE).exists() {
        bail!("{} already holds a server", dir.display());
    }
    fs::create_dir_all(dir.join(MINTS_DIR))
        .with_context(|| format!("creating {}", dir.display()))?;
    FileKek::create(&dir.join(KEK_FILE))
        .with_context(|| format!("creating {}", dir.join(KEK_FILE).display()))?;
    write_secret(&dir.join(OPERATOR_TOKEN_FILE), &files::random_token()?)
}

/// Reads the state directory's key-encryption key, making one for a state
/// directory from before they existed.
pub(crate) fn open_kek(dir: &Path) -> Result<Arc<dyn Kek>> {
    let path = dir.join(KEK_FILE);
    let kek = match FileKek::open(&path) {
        Ok(kek) => kek,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = %path.display(), "creating a key-encryption key");
            FileKek::create(&path).with_context(|| format!("creating {}", path.display()))?
        }
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    Ok(Arc::new(kek))
}

/// Creates a mint in the state directory `state` with a fresh issuer key and
/// admin token. A running server sees it only once restarted; use
/// [`AppState::create_mint`](crate::AppState::create_mint) instead to add a
/// mint while serving.
pub fn create_mint(state: &Path, spec: &MintSpec) -> Result<CreatedMint, CreateError> {
    if spec.rotate_every == Some(0) {
        return Err(CreateError::Invalid(
            "rotate_every must be at least 1 second".into(),
        ));
    }
    let key = IssuerKey::generate();
    let issuer = match Issuer::new(key.clone(), spec.ledger, spec.policy.clone()) {
        Ok(issuer) => issuer,
        Err(error @ (SetupError::Ledger(_) | SetupError::Policy(_))) => {
            return Err(CreateError::Invalid(error.to_string()));
        }
        Err(error) => return Err(CreateError::Storage(error.into())),
    };
    let mint = *issuer.public_key();
    let mints = state.join(MINTS_DIR);
    let new = mints.join(format!("{NEW_PREFIX}{mint}"));
    let done = mints.join(mint.to_string());
    let build = || -> Result<String> {
        let token = files::random_token()?;
        fs::create_dir(&new).with_context(|| format!("creating {}", new.display()))?;
        write_secret(&new.join(KEY_FILE), &hex::encode(key.to_secret_bytes()))?;
        write_secret(&new.join(TOKEN_FILE), &token)?;
        files::write_json(&new, POLICY_FILE, &spec.policy)?;
        let settings = Settings {
            rotate_every: spec.rotate_every,
            ..Settings::default()
        };
        files::write_json(&new, SETTINGS_FILE, &settings)?;
        drop(PersistentIssuer::create(&new, issuer)?);
        fs::rename(&new, &done)?;
        files::sync_dir(&mints)?;
        Ok(token)
    };
    match build() {
        Ok(token) => Ok(CreatedMint { mint, token }),
        Err(error) => {
            // The key is fresh, so nothing else can be in either directory,
            // and nobody was told of the mint yet.
            let _ = fs::remove_dir_all(&new);
            let _ = fs::remove_dir_all(&done);
            Err(CreateError::Storage(error))
        }
    }
}

/// Reads and opens every mint under `state`, clearing half-made ones.
pub(crate) fn load_all(state: &Path, kek: &Arc<dyn Kek>) -> Result<Vec<Loaded>> {
    let mints = state.join(MINTS_DIR);
    let mut loaded = Vec::new();
    for entry in fs::read_dir(&mints).with_context(|| format!("reading {}", mints.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(NEW_PREFIX) {
            tracing::warn!(dir = %entry.path().display(), "removing a mint left half made");
            fs::remove_dir_all(entry.path())
                .with_context(|| format!("removing {}", entry.path().display()))?;
        } else if !name.starts_with('.') && entry.file_type()?.is_dir() {
            loaded.push(load(&entry.path(), kek.clone())?);
        }
    }
    Ok(loaded)
}

/// A mint's files read and its store opened, before its tasks start.
pub(crate) struct Loaded {
    dir: PathBuf,
    issuer: PersistentIssuer,
    token: String,
    settings: Settings,
}

impl Loaded {
    pub(crate) fn key(&self) -> &PublicKey {
        self.issuer.public_key()
    }
}

/// Reads the mint in `dir` and opens its store, which seals the keys of its
/// rotations with `kek`.
pub(crate) fn load(dir: &Path, kek: Arc<dyn Kek>) -> Result<Loaded> {
    let key = IssuerKey::from_secret_bytes(&read_key(&dir.join(KEY_FILE))?);
    let name = key.public_key().to_string();
    if dir.file_name().and_then(|name| name.to_str()) != Some(&name) {
        bail!(
            "{} holds the mint {name}; a mint's directory must be named by its issuer key",
            dir.display()
        );
    }
    let token = read_secret(&dir.join(TOKEN_FILE))?;
    if token.len() < MIN_ADMIN_TOKEN_LEN {
        bail!(
            "{} must hold at least {MIN_ADMIN_TOKEN_LEN} characters",
            dir.join(TOKEN_FILE).display()
        );
    }
    let policy: Policy = read_json(&dir.join(POLICY_FILE))?;
    // A store made before mints had settings has none: no schedule, running.
    let settings: Settings = if dir.join(SETTINGS_FILE).exists() {
        read_json(&dir.join(SETTINGS_FILE))?
    } else {
        Settings::default()
    };
    if settings.rotate_every == Some(0) {
        bail!(
            "{}: rotate_every must be at least 1 second",
            dir.join(SETTINGS_FILE).display()
        );
    }
    let mut issuer = PersistentIssuer::open_with_kek(dir, key, policy, kek)
        .with_context(|| format!("opening {}", dir.display()))?;
    if settings.paused {
        // An older server paused it in mint.json. Log the pause first, so a
        // crash before the file is rewritten loses nothing.
        issuer
            .set_paused(true)
            .with_context(|| format!("logging the pause of {}", dir.display()))?;
        files::write_json(dir, SETTINGS_FILE, &settings)?;
    }
    Ok(Loaded {
        dir: dir.to_owned(),
        issuer,
        token,
        settings,
    })
}

/// A change for a mint's committer to make.
enum Change {
    Mint(VerifiedMint),
    Transfer(VerifiedTransfer),
    Burn(VerifiedBurn),
    /// With the boundary a schedule rotates for, or `None` on request.
    Rotate(Option<u64>),
}

/// What a change produced.
enum Done {
    Notes(Vec<Note>),
    Burned(u128),
    Rotated(RotationReport),
}

struct Job {
    change: Change,
    reply: oneshot::Sender<Result<Done, ApiError>>,
}

/// A mint the server hosts.
pub(crate) struct Mint {
    key: PublicKey,
    inner: Arc<Mutex<Inner>>,
    changes: mpsc::Sender<Job>,
    /// Replaced after every rotation, which adds a generation key.
    verifier: Arc<RwLock<Verifier>>,
    /// Mirrors whether the issuer is paused, which only changes under the
    /// lock, so requests to a paused mint are refused before their
    /// signatures are checked. The issuer itself decides in the committer.
    paused: Arc<AtomicBool>,
    token: Box<str>,
    schedule: RwLock<Option<RotationSchedule>>,
}

struct Inner {
    issuer: PersistentIssuer,
    verifier: Arc<RwLock<Verifier>>,
    paused: Arc<AtomicBool>,
    compact_every: u64,
    /// Events logged at which the next snapshot is written.
    compact_at: u64,
}

impl Mint {
    /// Starts a loaded mint's committer and, if it has a period, its rotation
    /// clock. Must be called within a Tokio runtime.
    pub(crate) fn start(loaded: Loaded, compact_every: u64) -> Arc<Self> {
        let Loaded {
            dir,
            issuer,
            token,
            settings,
        } = loaded;
        let key = *issuer.public_key();
        let compact_every = compact_every.max(1);
        let verifier = Arc::new(RwLock::new(issuer.issuer().verifier()));
        let paused = Arc::new(AtomicBool::new(issuer.issuer().is_paused()));
        let logged = issuer.issuer().last_scheduled_rotation().unwrap_or(0);
        let inner = Arc::new(Mutex::new(Inner {
            issuer,
            verifier: verifier.clone(),
            paused: paused.clone(),
            compact_every,
            compact_at: compact_every,
        }));
        let (changes, queue) = mpsc::channel(QUEUE);
        tokio::spawn(run_committer(inner.clone(), queue));
        let mint = Arc::new(Self {
            key,
            inner,
            changes,
            verifier,
            paused,
            token: token.into(),
            schedule: RwLock::new(None),
        });
        if let Some(period) = settings.rotate_every {
            let legacy = dir.join(LEGACY_SCHEDULE_FILE);
            let saved = fs::read_to_string(&legacy)
                .ok()
                .and_then(|text| text.trim().parse::<u64>().ok());
            let last = logged
                .max(saved.unwrap_or(0))
                .max(unix_now() / period * period);
            mint.set_schedule(period, last);
            let legacy = saved.is_some().then_some(legacy);
            tokio::spawn(rotate_on_schedule(mint.clone(), period, last, legacy));
        }
        mint
    }

    pub(crate) fn key(&self) -> &PublicKey {
        &self.key
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// The verifier for the mint's current generations.
    fn verifier(&self) -> Verifier {
        // The lock guards a plain value, so a poisoned one still holds a usable verifier.
        self.verifier
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_schedule(&self, period: u64, last: u64) {
        let schedule = RotationSchedule {
            period_secs: period,
            next_at: last.saturating_add(period),
        };
        *self.schedule.write().unwrap_or_else(|e| e.into_inner()) = Some(schedule);
    }

    fn check_running(&self) -> Result<(), ApiError> {
        if self.paused.load(Ordering::Acquire) {
            Err(ApiError::paused())
        } else {
            Ok(())
        }
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

    /// Runs `f` on the blocking pool with the mint locked.
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

    pub(crate) async fn info(&self) -> Result<Info, ApiError> {
        let rotation = *self.schedule.read().unwrap_or_else(|e| e.into_inner());
        self.locked(move |inner| {
            let issuer = inner.issuer.issuer();
            Ok(Info {
                issuer: *issuer.public_key(),
                sequence: issuer.sequence(),
                mint_layer: issuer.mint_position(),
                layers: issuer.layers(),
                supply: issuer.supply(),
                policy: issuer.policy().clone(),
                rotation,
                paused: issuer.is_paused(),
            })
        })
        .await
    }

    /// The keys that sign the live generations, certified by the issuer key.
    pub(crate) fn keys(&self) -> KeySet {
        self.verifier().keys().clone()
    }

    pub(crate) async fn verify(&self, note: Note) -> Result<Verified, ApiError> {
        // Check the signature before taking the lock; the lookup itself is O(1).
        let verifier = self.verifier();
        let keys = verifier.keys();
        match note.verify(keys) {
            Ok(()) => {}
            // A dropped generation's key is erased; the lookup reports it expired.
            Err(NoteError::UnknownGeneration(generation))
                if note.issuer() == keys.issuer() && generation < keys.oldest() => {}
            Err(error) => return Err(Error::Note { input: 0, error }.into()),
        }
        self.locked(move |inner| {
            let status = inner.issuer.issuer().serial_status(note.serial());
            Ok(Verified {
                serial: note.serial(),
                amount: note.amount(),
                status,
            })
        })
        .await
    }

    pub(crate) async fn transfer(&self, request: TransferRequest) -> Result<Vec<Note>, ApiError> {
        self.check_running()?;
        let verifier = self.verifier();
        let verified = tokio::task::spawn_blocking(move || verifier.verify(&request))
            .await
            .map_err(|_| ApiError::internal("verification panicked"))??;
        match self.submit(Change::Transfer(verified)).await? {
            Done::Notes(notes) => Ok(notes),
            _ => unreachable!("a transfer creates notes"),
        }
    }

    /// Burns the request's inputs and answers with the value destroyed.
    pub(crate) async fn burn(&self, request: BurnRequest) -> Result<u128, ApiError> {
        self.check_running()?;
        let verifier = self.verifier();
        let verified = tokio::task::spawn_blocking(move || verifier.verify_burn(&request))
            .await
            .map_err(|_| ApiError::internal("verification panicked"))??;
        match self.submit(Change::Burn(verified)).await? {
            Done::Burned(value) => Ok(value),
            _ => unreachable!("a burn reports its value"),
        }
    }

    pub(crate) async fn issue(&self, request: MintRequest) -> Result<Note, ApiError> {
        self.check_running()?;
        let verifier = self.verifier();
        let verified = tokio::task::spawn_blocking(move || verifier.verify_mint(&request))
            .await
            .map_err(|_| ApiError::internal("verification panicked"))??;
        match self.submit(Change::Mint(verified)).await? {
            Done::Notes(mut notes) => Ok(notes.remove(0)),
            _ => unreachable!("a mint creates a note"),
        }
    }

    /// Rotates and writes a fresh snapshot. `due` is the boundary a schedule
    /// rotates for, or `None` for a rotation on request. A paused mint
    /// refuses.
    pub(crate) async fn rotate(&self, due: Option<u64>) -> Result<RotationReport, ApiError> {
        match self.submit(Change::Rotate(due)).await? {
            Done::Rotated(report) => Ok(report),
            _ => unreachable!("a rotation reports a rotation"),
        }
    }

    /// Pauses or resumes the mint, durably, once the batch being committed
    /// has finished.
    pub(crate) async fn set_paused(&self, paused: bool) -> Result<(), ApiError> {
        self.locked(move |inner| inner.set_paused(paused)).await
    }
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
                Change::Mint(mint) => batch
                    .authorized_mint(&mint)
                    .map(|note| Done::Notes(vec![note]))
                    .map_err(ApiError::from),
                Change::Transfer(transfer) => batch
                    .transfer(&transfer)
                    .map(Done::Notes)
                    .map_err(ApiError::from),
                Change::Burn(burn) => batch.burn(&burn).map(Done::Burned).map_err(ApiError::from),
                Change::Rotate(due) => {
                    let report = match due {
                        Some(due) => batch.rotate_scheduled(due),
                        None => batch.rotate(),
                    }
                    .map_err(ApiError::from);
                    rotated |= report.is_ok();
                    report.map(Done::Rotated)
                }
            })
            .collect();
        if let Err(error) = batch.commit() {
            tracing::error!(
                mint = %self.issuer.public_key(),
                "the mint refuses changes until the server is restarted"
            );
            let error = ApiError::from(CommitError::Io(error));
            return results
                .into_iter()
                .map(|result| result.and(Err(error.clone())))
                .collect();
        }
        if rotated {
            // Before any answer goes out, since notes of the new generation
            // only verify with its key.
            let verifier = self.issuer.issuer().verifier();
            *self.verifier.write().unwrap_or_else(|e| e.into_inner()) = verifier;
            self.compact();
        } else if self.issuer.log_events() >= self.compact_at {
            self.compact();
        }
        results
    }

    fn compact(&mut self) {
        match self.issuer.compact() {
            Ok(()) => {
                self.compact_at = self.compact_every;
                tracing::debug!(mint = %self.issuer.public_key(), "wrote a fresh snapshot");
            }
            Err(error) => {
                // The log still holds every event, so nothing is lost.
                // Try again after another batch rather than on every request.
                self.compact_at = self.issuer.log_events() + self.compact_every;
                tracing::error!(
                    mint = %self.issuer.public_key(),
                    %error,
                    "snapshot failed; the log keeps growing"
                );
            }
        }
    }

    fn set_paused(&mut self, paused: bool) -> Result<(), ApiError> {
        if self.issuer.issuer().is_paused() == paused {
            return Ok(());
        }
        self.issuer.set_paused(paused)?;
        self.paused.store(paused, Ordering::Release);
        tracing::info!(mint = %self.issuer.public_key(), paused, "pause switched");
        Ok(())
    }
}

/// Takes every change waiting, up to [`MAX_BATCH`], commits them together on
/// the blocking pool and answers each. Returns once the mint is gone. An idle
/// mint holds no thread.
async fn run_committer(inner: Arc<Mutex<Inner>>, mut queue: mpsc::Receiver<Job>) {
    while let Some(first) = queue.recv().await {
        let mut jobs = vec![first];
        while jobs.len() < MAX_BATCH {
            match queue.try_recv() {
                Ok(job) => jobs.push(job),
                Err(_) => break,
            }
        }
        let (changes, replies): (Vec<_>, Vec<_>) =
            jobs.into_iter().map(|job| (job.change, job.reply)).unzip();
        let count = changes.len();
        let inner = inner.clone();
        let results = tokio::task::spawn_blocking(move || match inner.lock() {
            Ok(mut inner) => inner.commit(changes),
            Err(_) => {
                let error = ApiError::internal("issuer state is poisoned by an earlier panic");
                changes.iter().map(|_| Err(error.clone())).collect()
            }
        })
        .await
        .unwrap_or_else(|_| {
            let error = ApiError::internal("the committer panicked");
            (0..count).map(|_| Err(error.clone())).collect()
        });
        for (reply, result) in replies.into_iter().zip(results) {
            // The client may have gone away; the change stands regardless.
            let _ = reply.send(result);
        }
    }
}

/// Rotates at every multiple of `period` seconds since the Unix epoch, so the
/// schedule survives restarts. Boundaries missed while the server was down,
/// and those reached while the mint is paused, are not made up.
///
/// Each scheduled rotation logs the boundary it was due at, and the issuer
/// refuses one that is not due after the last, so neither a clock that steps
/// backwards nor a restart can rotate twice for one period. `legacy` is the
/// file an older server kept the last boundary in, removed once a scheduled
/// rotation is logged.
async fn rotate_on_schedule(
    mint: Arc<Mint>,
    period: u64,
    mut last: u64,
    mut legacy: Option<PathBuf>,
) {
    loop {
        let now = unix_now();
        let next = last.saturating_add(period);
        mint.set_schedule(period, last);
        if now < next {
            // Sleeping is on the monotonic clock; check the wall clock again on waking.
            tokio::time::sleep(Duration::from_secs(next - now)).await;
            continue;
        }
        let due = now / period * period;
        match mint.rotate(Some(due)).await {
            Ok(report) => {
                tracing::info!(mint = %mint.key, ?report, "scheduled rotation");
                if let Some(file) = legacy.take()
                    && let Err(error) = fs::remove_file(&file)
                {
                    tracing::warn!(mint = %mint.key, %error, "could not remove {}", file.display());
                }
            }
            Err(error) if error.code() == "paused" => {
                tracing::info!(mint = %mint.key, "paused; skipped a scheduled rotation");
            }
            Err(error) if error.code() == "stale_rotation" => {
                tracing::info!(mint = %mint.key, due, "this period's rotation is already logged");
            }
            Err(error) => {
                tracing::error!(mint = %mint.key, ?error, "scheduled rotation failed; retrying");
                tokio::time::sleep(Duration::from_secs(period.min(60))).await;
                continue;
            }
        }
        last = due;
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The mints a server hosts, by issuer key.
pub(crate) type Registry = HashMap<PublicKey, Arc<Mint>>;
