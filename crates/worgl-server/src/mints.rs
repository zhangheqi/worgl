//! The mints a server hosts. Each mint is one issuer with its own directory,
//! committer, verifier and rotation clock, so mints never wait on each other.
//!
//! A server's state directory holds
//!
//! - `operator.token`, which authorizes creating and listing mints,
//! - `kek.key`, the key-encryption key that seals each new generation key
//!   into its rotation's log entry, mode 0600 (a [`FileKek`], for
//!   development; replicas will share one from a KMS). Only [`init_state`]
//!   makes it, the directory does not open without it, and every member of
//!   a cluster must hold the same one; and
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
//!
//! On a cluster member (see [`cluster`](crate::cluster)), a mint is a
//! [`Replica`] of its group instead of a store of its own, and its directory
//! also holds the replica's `vote.json`. Changes are answered once a
//! majority of the members hold them on disk, and the leader alone makes
//! them, scheduled rotations included. A mint created while the cluster
//! runs is a record in the cluster's registry first, holding its secrets
//! sealed by the key-encryption key and its first state; each member writes
//! the mint's directory from that record, so all of them start alike.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use axum::http::StatusCode;
use openraft::SnapshotPolicy;
use serde::{Deserialize, Serialize};
use tokio::runtime::{Handle, RuntimeFlavor};
use tokio::sync::{mpsc, oneshot, watch};
use worgl::{
    BurnRequest, Clock, CommitError, Error, Issuer, IssuerKey, KeySet, LedgerConfig, MintRequest,
    Note, NoteError, PersistentIssuer, Policy, PublicKey, RotationReport, SetupError, StoreState,
    TransferRequest, VerifiedBurn, VerifiedMint, VerifiedTransfer, Verifier,
};
use worgl::{FileKek, Kek, KeyRing};
use worgl_raft::registry::Records;
use worgl_raft::{Options, Replica, SnapshotData, default_config};

use crate::cluster::Cluster;
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

/// What a registry record's sealed secrets are bound to, before the mint's
/// issuer key.
const ISSUER_KEY_CONTEXT: &[u8] = b"worgl/registry/issuer-key/v1";
const ADMIN_TOKEN_CONTEXT: &[u8] = b"worgl/registry/admin-token/v1";

/// Changes that may wait for a mint's committer before senders have to wait too.
const QUEUE: usize = 4096;

/// How long a replica that does not lead waits for its leader to log a
/// scheduled rotation before looking again.
const FOLLOWER_WAIT: Duration = Duration::from_secs(1);

/// Transfers and burns a second, sustained, below which a mint's receipt
/// policy is worth a warning when the mint starts.
const LOW_CEILING: u32 = 1_000;

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
/// key and no mints. This is the only place a key-encryption key is made.
pub fn init_state(dir: &Path) -> Result<()> {
    if dir.join(OPERATOR_TOKEN_FILE).exists() {
        bail!("{} already holds a server", dir.display());
    }
    fs::create_dir_all(dir.join(MINTS_DIR))
        .with_context(|| format!("creating {}", dir.display()))?;
    FileKek::create(&dir.join(KEK_FILE))
        .with_context(|| format!("creating {}", dir.join(KEK_FILE).display()))?;
    write_secret(&dir.join(OPERATOR_TOKEN_FILE), &files::random_token()?)?;
    files::sync_dir(dir).with_context(|| format!("syncing {}", dir.display()))
}

/// Reads the state directory's key-encryption key. A missing one is an
/// error: a new key would not open what the old one sealed, and on a
/// cluster it would differ from the other members' key.
pub(crate) fn open_kek(dir: &Path) -> Result<Arc<dyn Kek>> {
    let path = dir.join(KEK_FILE);
    match FileKek::open(&path) {
        Ok(kek) => Ok(Arc::new(kek)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => bail!(
            "{} is missing: restore it from a backup of this directory or, on a cluster, \
             copy it from another member; a new key would not open what the old one sealed",
            path.display()
        ),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// Creates a mint in the state directory `state` with a fresh issuer key and
/// admin token. A running server sees it only once restarted; use
/// [`AppState::create_mint`](crate::AppState::create_mint) instead to add a
/// mint while serving.
pub fn create_mint(state: &Path, spec: &MintSpec) -> Result<CreatedMint, CreateError> {
    let (key, issuer) = new_issuer(spec)?;
    let token = files::random_token().map_err(CreateError::Storage)?;
    write_mint(state, &key, &token, &spec.policy, spec.rotate_every, issuer)
        .map_err(CreateError::Storage)?;
    Ok(CreatedMint {
        mint: key.public_key(),
        token,
    })
}

/// A fresh issuer key and the new issuer `spec` describes.
fn new_issuer(spec: &MintSpec) -> Result<(IssuerKey, Issuer), CreateError> {
    if spec.rotate_every == Some(0) {
        return Err(CreateError::Invalid(
            "rotate_every must be at least 1 second".into(),
        ));
    }
    let key = IssuerKey::generate();
    match Issuer::new(key.clone(), spec.ledger, spec.policy.clone()) {
        Ok(issuer) => Ok((key, issuer)),
        Err(error @ (SetupError::Ledger(_) | SetupError::Policy(_))) => {
            Err(CreateError::Invalid(error.to_string()))
        }
        Err(error) => Err(CreateError::Storage(error.into())),
    }
}

/// Writes the directory of the mint `issuer`, whose key is `key`, under
/// `state`: built in a hidden directory and renamed into place once
/// complete.
fn write_mint(
    state: &Path,
    key: &IssuerKey,
    token: &str,
    policy: &Policy,
    rotate_every: Option<u64>,
    issuer: Issuer,
) -> Result<()> {
    let mint = *issuer.public_key();
    let mints = state.join(MINTS_DIR);
    let new = mints.join(format!("{NEW_PREFIX}{mint}"));
    let done = mints.join(mint.to_string());
    let build = || -> Result<()> {
        // One a crash left half made, while writing it from the registry.
        if new.exists() {
            fs::remove_dir_all(&new).with_context(|| format!("removing {}", new.display()))?;
        }
        fs::create_dir(&new).with_context(|| format!("creating {}", new.display()))?;
        write_secret(&new.join(KEY_FILE), &hex::encode(key.to_secret_bytes()))?;
        write_secret(&new.join(TOKEN_FILE), token)?;
        files::write_json(&new, POLICY_FILE, policy)?;
        let settings = Settings {
            rotate_every,
            ..Settings::default()
        };
        files::write_json(&new, SETTINGS_FILE, &settings)?;
        drop(PersistentIssuer::create(&new, issuer)?);
        fs::rename(&new, &done)?;
        files::sync_dir(&mints)?;
        Ok(())
    };
    build().inspect_err(|_| {
        // The key is fresh, or this member has no directory for it yet, so
        // nothing else can be in either directory.
        let _ = fs::remove_dir_all(&new);
        let _ = fs::remove_dir_all(&done);
    })
}

/// What a cluster's registry keeps about a mint created while the cluster
/// runs: everything a member needs to write the mint's directory, with the
/// secrets sealed by the key-encryption key the members share.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MintRecord {
    policy: Policy,
    rotate_every: Option<u64>,
    /// The issuer key, sealed, in hex.
    issuer_key: String,
    /// The admin token's bytes, sealed, in hex.
    admin_token: String,
    /// The new issuer, as [`SnapshotData::to_bytes`] encodes it with its
    /// generation keys sealed, in hex.
    genesis: String,
}

fn context(label: &[u8], mint: &PublicKey) -> Vec<u8> {
    [label, mint.as_bytes()].concat()
}

/// Makes a new mint as `spec` describes for a cluster's registry: the record
/// every member writes the mint from, and what to tell its creator.
pub(crate) fn new_record(
    spec: &MintSpec,
    kek: &dyn Kek,
) -> Result<(CreatedMint, MintRecord), CreateError> {
    let (key, issuer) = new_issuer(spec)?;
    let mint = key.public_key();
    let mut token = [0u8; 32];
    getrandom::fill(&mut token)
        .map_err(|e| CreateError::Storage(anyhow::anyhow!("system RNG failed: {e}")))?;
    let sealing = |error| CreateError::Storage(anyhow::anyhow!("sealing the mint: {error}"));
    let issuer_key = kek
        .seal(&key.to_secret_bytes(), &context(ISSUER_KEY_CONTEXT, &mint))
        .map_err(sealing)?;
    let admin_token = kek
        .seal(&token, &context(ADMIN_TOKEN_CONTEXT, &mint))
        .map_err(sealing)?;
    let genesis = SnapshotData {
        issuer: issuer.snapshot(),
        keys: issuer.seal_keys(kek).map_err(sealing)?,
    };
    let record = MintRecord {
        policy: spec.policy.clone(),
        rotate_every: spec.rotate_every,
        issuer_key: hex::encode(issuer_key),
        admin_token: hex::encode(admin_token),
        genesis: hex::encode(genesis.to_bytes()),
    };
    let created = CreatedMint {
        mint,
        token: hex::encode(token),
    };
    Ok((created, record))
}

/// Writes the directory of the mint `mint` under `state` from its registry
/// `record`, unless there is one, and returns where it is.
pub(crate) fn materialize(
    state: &Path,
    mint: &PublicKey,
    record: &serde_json::Value,
    kek: &dyn Kek,
) -> Result<PathBuf> {
    let done = state.join(MINTS_DIR).join(mint.to_string());
    if done.exists() {
        return Ok(done);
    }
    let record = MintRecord::deserialize(record).context("reading the mint's record")?;
    let unseal = |sealed: &str, label: &[u8], what: &str| {
        let sealed = hex::decode(sealed).with_context(|| format!("decoding the {what}"))?;
        kek.unseal(&sealed, &context(label, mint))
            .with_context(|| format!("unsealing the {what}"))
    };
    let key = IssuerKey::from_secret_bytes(&*unseal(
        &record.issuer_key,
        ISSUER_KEY_CONTEXT,
        "issuer key",
    )?);
    if key.public_key() != *mint {
        bail!("the record holds the key of another mint");
    }
    let token = hex::encode(*unseal(
        &record.admin_token,
        ADMIN_TOKEN_CONTEXT,
        "admin token",
    )?);
    let genesis = hex::decode(&record.genesis).context("decoding the first state")?;
    let genesis = SnapshotData::from_bytes(&genesis).context("decoding the first state")?;
    let mut ring = KeyRing::new(*mint);
    for sealed in &genesis.keys {
        ring.insert(
            sealed
                .unseal(kek, mint)
                .with_context(|| format!("unsealing generation {}'s key", sealed.generation))?,
        );
    }
    let issuer = Issuer::restore(key.clone(), ring, &genesis.issuer, record.policy.clone())
        .context("restoring the first state")?;
    write_mint(
        state,
        &key,
        &token,
        &record.policy,
        record.rotate_every,
        issuer,
    )?;
    tracing::info!(%mint, "wrote a mint from the registry");
    Ok(done)
}

/// The directory of every mint under `state`, clearing half-made ones.
fn mint_dirs(state: &Path) -> Result<Vec<PathBuf>> {
    let mints = state.join(MINTS_DIR);
    let mut dirs = Vec::new();
    for entry in fs::read_dir(&mints).with_context(|| format!("reading {}", mints.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(NEW_PREFIX) {
            tracing::warn!(dir = %entry.path().display(), "removing a mint left half made");
            fs::remove_dir_all(entry.path())
                .with_context(|| format!("removing {}", entry.path().display()))?;
        } else if !name.starts_with('.') && entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

/// Reads and opens every mint under `state`, clearing half-made ones.
pub(crate) fn load_all(state: &Path, kek: &Arc<dyn Kek>) -> Result<Vec<Loaded>> {
    mint_dirs(state)?
        .iter()
        .map(|dir| load(dir, kek.clone()))
        .collect()
}

/// Reads every mint under `state` and opens it as a replica of its group in
/// `cluster`, as [`load_member`] does; those in `registered` came from the
/// cluster's registry.
pub(crate) async fn load_all_members(
    state: &Path,
    kek: &Arc<dyn Kek>,
    cluster: &Arc<Cluster>,
    compact_every: u64,
    registered: &Records,
) -> Result<Vec<Loaded>> {
    let mut loaded = Vec::new();
    for dir in mint_dirs(state)? {
        let from_registry = dir
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.parse::<PublicKey>().ok())
            .is_some_and(|key| registered.contains_key(&key));
        loaded.push(load_member(&dir, kek.clone(), cluster, compact_every, from_registry).await?);
    }
    Ok(loaded)
}

/// A mint's files read and its store opened, before its tasks start.
pub(crate) struct Loaded {
    dir: PathBuf,
    key: PublicKey,
    store: Store,
    token: String,
    settings: Settings,
}

enum Store {
    /// A store of the mint's own.
    Local(Box<PersistentIssuer>),
    /// A replica of the mint's group.
    Replicated(Arc<Replica>),
}

impl Loaded {
    pub(crate) fn key(&self) -> &PublicKey {
        &self.key
    }
}

/// What a mint's directory holds beside its store.
struct Files {
    key: IssuerKey,
    token: String,
    policy: Policy,
    settings: Settings,
}

fn read_files(dir: &Path) -> Result<Files> {
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
    Ok(Files {
        key,
        token,
        policy,
        settings,
    })
}

/// Reads the mint in `dir` and opens its store, which seals the keys of its
/// rotations with `kek`.
pub(crate) fn load(dir: &Path, kek: Arc<dyn Kek>) -> Result<Loaded> {
    let Files {
        key,
        token,
        policy,
        settings,
    } = read_files(dir)?;
    let mut issuer = PersistentIssuer::open_with_kek(dir, key, policy, kek)
        .with_context(|| format!("opening {}", dir.display()))?;
    if let Some(error) = issuer.take_compaction_error() {
        // The store opened from the snapshot and log as they are.
        tracing::warn!(
            dir = %dir.display(),
            %error,
            "could not compact the log on opening; it keeps every event, and the next \
             snapshot tries again"
        );
    }
    issuer.set_clock(Clock::monotonic());
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
        key: *issuer.public_key(),
        store: Store::Local(Box::new(issuer)),
        token,
        settings,
    })
}

/// Reads the mint in `dir` and opens it as a replica of its group in
/// `cluster`. A store that has never joined the group, whether new or one a
/// server ran the mint on alone, is readied for it first (see
/// [`worgl_raft::prepare`]), and the group formed: by the first member, or,
/// for a mint `from_registry`, by the member that created it, and by the
/// others only if it has not after a while.
pub(crate) async fn load_member(
    dir: &Path,
    kek: Arc<dyn Kek>,
    cluster: &Arc<Cluster>,
    compact_every: u64,
    from_registry: bool,
) -> Result<Loaded> {
    if !worgl_raft::has_joined(dir) {
        // Open it as a server on its own would first, which converts older
        // stores and logs an older server's pause.
        drop(load(dir, kek.clone())?);
    }
    let Files {
        key,
        token,
        policy,
        settings,
    } = read_files(dir)?;
    let public = key.public_key();
    let new = worgl_raft::prepare(dir, key.clone(), policy.clone(), &*kek)
        .with_context(|| format!("readying {} for its group", dir.display()))?;
    let mut options = Options::new(cluster.node(), dir, key, policy, kek);
    options.raft = openraft::Config {
        snapshot_policy: SnapshotPolicy::LogsSinceLast(compact_every.max(1)),
        ..default_config()
    };
    let replica = Replica::open(options, cluster.network(public))
        .await
        .with_context(|| format!("opening {} as a replica", dir.display()))?;
    let replica = Arc::new(replica);
    if new {
        let raft = replica.raft().clone();
        let at_once = cluster.is_first() && !from_registry;
        tokio::spawn(
            cluster
                .clone()
                .form(raft, format!("mint {public}"), at_once),
        );
    }
    Ok(Loaded {
        dir: dir.to_owned(),
        key: public,
        store: Store::Replicated(replica),
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
    /// Pauses, or resumes if false.
    Pause(bool),
}

/// What a change produced.
enum Done {
    Notes(Vec<Note>),
    Burned(u128),
    Rotated(RotationReport),
    Paused,
}

struct Job {
    change: Change,
    reply: oneshot::Sender<Result<Done, ApiError>>,
}

/// Whether a mint serves, as `GET /readyz` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Readiness {
    /// The mint takes changes and answers reads.
    Serving,
    /// A batch did not reach the mint's disk, as on a full disk, and no
    /// batch has since. The mint answers reads from what reached its disk,
    /// and serves again once a batch does.
    Failing,
    /// The mint's store is [ahead](StoreState::Ahead) of its disk: a batch
    /// did not reach it, and the mint could not reread its state from it.
    Ahead,
    /// The mint's store has [stopped](StoreState::Stopped), or on a cluster
    /// member, the member's copy of the mint has.
    Stopped,
}

/// A mint the server hosts.
pub(crate) struct Mint {
    key: PublicKey,
    backend: Backend,
    changes: mpsc::Sender<Job>,
    token: Box<str>,
    schedule: RwLock<Option<RotationSchedule>>,
}

enum Backend {
    /// A store of the mint's own.
    Local {
        inner: Arc<Mutex<Inner>>,
        /// Replaced after every rotation, which adds a generation key.
        verifier: Arc<RwLock<Verifier>>,
        /// Mirrors whether the issuer is paused, which only changes under
        /// the lock, so requests to a paused mint are refused before their
        /// signatures are checked. The issuer itself decides in the
        /// committer.
        paused: Arc<AtomicBool>,
        /// Mirrors whether the mint serves, which also only changes under
        /// the lock, so readiness checks do not wait on a commit.
        readiness: Arc<RwLock<Readiness>>,
    },
    /// A replica of the mint's group, whose verifier changes as it applies
    /// rotations.
    Replicated {
        replica: Arc<Replica>,
        verifier: watch::Receiver<Verifier>,
    },
}

struct Inner {
    issuer: PersistentIssuer,
    verifier: Arc<RwLock<Verifier>>,
    paused: Arc<AtomicBool>,
    readiness: Arc<RwLock<Readiness>>,
    /// Set when a batch does not reach the disk, and cleared when one does.
    failing: bool,
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
            key,
            store,
            token,
            settings,
        } = loaded;
        let (changes, queue) = mpsc::channel(QUEUE);
        let policy = match &store {
            Store::Local(issuer) => issuer.issuer().policy().clone(),
            Store::Replicated(replica) => replica.read_local(|issuer| issuer.policy().clone()),
        };
        warn_if_capped(&key, &policy);
        let (backend, logged) = match store {
            Store::Local(issuer) => {
                let compact_every = compact_every.max(1);
                let verifier = Arc::new(RwLock::new(issuer.issuer().verifier()));
                let paused = Arc::new(AtomicBool::new(issuer.issuer().is_paused()));
                let readiness = Arc::new(RwLock::new(Readiness::Serving));
                let logged = issuer.issuer().last_scheduled_rotation();
                let inner = Inner {
                    issuer: *issuer,
                    verifier: verifier.clone(),
                    paused: paused.clone(),
                    readiness: readiness.clone(),
                    failing: false,
                    compact_every,
                    compact_at: compact_every,
                };
                inner.mirror_readiness();
                let inner = Arc::new(Mutex::new(inner));
                tokio::spawn(run_committer(inner.clone(), queue));
                let backend = Backend::Local {
                    inner,
                    verifier,
                    paused,
                    readiness,
                };
                (backend, logged)
            }
            Store::Replicated(replica) => {
                let logged = replica.read_local(Issuer::last_scheduled_rotation);
                tokio::spawn(run_replicated(replica.clone(), queue));
                let verifier = replica.verifier();
                (Backend::Replicated { replica, verifier }, logged)
            }
        };
        let mint = Arc::new(Self {
            key,
            backend,
            changes,
            token: token.into(),
            schedule: RwLock::new(None),
        });
        if let Some(period) = settings.rotate_every {
            let legacy = dir.join(LEGACY_SCHEDULE_FILE);
            let saved = fs::read_to_string(&legacy)
                .ok()
                .and_then(|text| text.trim().parse::<u64>().ok());
            let last = logged
                .unwrap_or(0)
                .max(saved.unwrap_or(0))
                .max(unix_now() / period * period);
            mint.set_schedule(period, last);
            let legacy = saved.map(|floor| (legacy, floor));
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

    /// The mint's replica, if it is one.
    pub(crate) fn replica(&self) -> Option<&Arc<Replica>> {
        match &self.backend {
            Backend::Local { .. } => None,
            Backend::Replicated { replica, .. } => Some(replica),
        }
    }

    /// The verifier for the mint's current generations.
    fn verifier(&self) -> Verifier {
        match &self.backend {
            // The lock guards a plain value, so a poisoned one still holds a usable verifier.
            Backend::Local { verifier, .. } => {
                verifier.read().unwrap_or_else(|e| e.into_inner()).clone()
            }
            Backend::Replicated { verifier, .. } => verifier.borrow().clone(),
        }
    }

    fn is_paused(&self) -> bool {
        match &self.backend {
            Backend::Local { paused, .. } => paused.load(Ordering::Acquire),
            Backend::Replicated { replica, .. } => replica.read_local(Issuer::is_paused),
        }
    }

    /// Whether the mint serves. A replica counts as
    /// [`Stopped`](Readiness::Stopped) once its Raft node has stopped, and
    /// as serving until then.
    pub(crate) fn readiness(&self) -> Readiness {
        match &self.backend {
            Backend::Local { readiness, .. } => {
                *readiness.read().unwrap_or_else(|e| e.into_inner())
            }
            Backend::Replicated { replica, .. } => {
                if replica.raft().metrics().borrow().running_state.is_err() {
                    Readiness::Stopped
                } else {
                    Readiness::Serving
                }
            }
        }
    }

    /// Whether this server makes the mint's changes: always for a store of
    /// its own, and only while it leads for a replica.
    fn leads(&self) -> bool {
        match &self.backend {
            Backend::Local { .. } => true,
            Backend::Replicated { replica, .. } => replica.is_leader(),
        }
    }

    /// For a replica, the boundary of the last scheduled rotation its group
    /// logged, whichever member made it.
    fn logged_rotation(&self) -> Option<u64> {
        match &self.backend {
            Backend::Local { .. } => None,
            Backend::Replicated { replica, .. } => {
                replica.read_local(Issuer::last_scheduled_rotation)
            }
        }
    }

    fn set_schedule(&self, period: u64, last: u64) {
        let schedule = RotationSchedule {
            period_secs: period,
            next_at: last.saturating_add(period),
        };
        *self.schedule.write().unwrap_or_else(|e| e.into_inner()) = Some(schedule);
    }

    fn check_running(&self) -> Result<(), ApiError> {
        if self.is_paused() {
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

    /// Reads the issuer: with its store locked, on the blocking pool, for a
    /// mint of its own, and once the leader has confirmed it leads and is up
    /// to date for a replica. A mint of its own whose last batch did not
    /// reach its disk rereads its state from the disk first, and refuses
    /// while that fails or its store has stopped.
    async fn read<T, F>(&self, read: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce(&Issuer) -> T + Send + 'static,
    {
        match &self.backend {
            Backend::Local { inner, .. } => {
                let inner = inner.clone();
                tokio::task::spawn_blocking(move || {
                    let mut inner = inner.lock().map_err(|_| {
                        ApiError::internal("issuer state is poisoned by an earlier panic")
                    })?;
                    inner.recover();
                    inner.check_in_step()?;
                    Ok(read(inner.issuer.issuer()))
                })
                .await
                .map_err(|_| ApiError::internal("request handler panicked"))?
            }
            Backend::Replicated { replica, .. } => Ok(replica.read(read).await?),
        }
    }

    pub(crate) async fn info(&self) -> Result<Info, ApiError> {
        let rotation = *self.schedule.read().unwrap_or_else(|e| e.into_inner());
        self.read(move |issuer| Info {
            issuer: *issuer.public_key(),
            sequence: issuer.sequence(),
            mint_layer: issuer.mint_position(),
            layers: issuer.layers(),
            supply: issuer.supply(),
            policy: issuer.policy().clone(),
            rotation,
            paused: issuer.is_paused(),
        })
        .await
    }

    /// The keys that sign the live generations, certified by the issuer key.
    pub(crate) async fn keys(&self) -> Result<KeySet, ApiError> {
        match &self.backend {
            Backend::Local { .. } => Ok(self.verifier().keys().clone()),
            Backend::Replicated { .. } => self.read(|issuer| issuer.keys().clone()).await,
        }
    }

    pub(crate) async fn verify(&self, note: Note) -> Result<Verified, ApiError> {
        // Check the signature before taking the lock; the lookup itself is O(1).
        let verifier = self.verifier();
        let keys = verifier.keys();
        match note.verify(keys) {
            Ok(()) => {}
            // The live keys omit a dropped generation; the lookup reports it expired.
            Err(NoteError::UnknownGeneration(generation))
                if note.issuer() == keys.issuer() && generation < keys.oldest() => {}
            Err(error) => return Err(Error::Note { input: 0, error }.into()),
        }
        let (serial, amount) = (note.serial(), note.amount());
        self.read(move |issuer| Verified {
            serial,
            amount,
            status: issuer.serial_status(serial),
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

    /// Pauses or resumes the mint, durably, after the changes queued before.
    pub(crate) async fn set_paused(&self, paused: bool) -> Result<(), ApiError> {
        match self.submit(Change::Pause(paused)).await? {
            Done::Paused => {
                tracing::info!(mint = %self.key, paused, "pause switched");
                Ok(())
            }
            _ => unreachable!("a pause reports nothing"),
        }
    }
}

impl Inner {
    /// Carries out `changes` as one batch with one flush. A change the issuer
    /// refuses gets its error; if the flush fails, every other change gets a
    /// storage error, the issuer is rebuilt from what reached the disk, and
    /// the mint is failing until a batch reaches it. If rebuilding failed
    /// before, it is tried again first, and every change is refused while it
    /// fails.
    fn commit(&mut self, changes: Vec<Change>) -> Vec<Result<Done, ApiError>> {
        self.recover();
        if self.issuer.state() == StoreState::Ahead {
            let error = ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage",
                "the issuer could not reread its state from its disk; nothing was applied",
            );
            return changes.iter().map(|_| Err(error.clone())).collect();
        }
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
                Change::Pause(paused) => {
                    batch.set_paused(paused);
                    Ok(Done::Paused)
                }
            })
            .collect();
        let writes = !batch.is_empty();
        if let Err(error) = batch.commit() {
            if self.issuer.state() == StoreState::Stopped {
                tracing::error!(
                    mint = %self.issuer.public_key(),
                    %error,
                    "a batch did not reach the disk, and the log could not be cut back after \
                     it, so what the log holds is unknown; the mint refuses changes and reads \
                     of its ledger until the server is restarted"
                );
                self.mirror_readiness();
                // A transfer, burn, mint or pause sent again is made at most
                // once, but a rotation on request is made again.
                let error = ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage",
                    "the issuer could not record the change or undo it, so it may have been \
                     applied; send the same request again once the server has been restarted",
                );
                let rotation = ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage",
                    "the issuer could not record the rotation or undo it, so it may have been \
                     made; once the server has been restarted, check the generation of the \
                     mint's last layer before rotating again",
                );
                return results
                    .into_iter()
                    .map(|result| match result {
                        Ok(Done::Rotated(_)) => Err(rotation.clone()),
                        result => result.and(Err(error.clone())),
                    })
                    .collect();
            }
            let error = ApiError::from(CommitError::Io(error));
            // The batch took effect in memory only.
            self.failing = true;
            self.recover();
            return results
                .into_iter()
                .map(|result| result.and(Err(error.clone())))
                .collect();
        }
        if writes && self.failing {
            self.failing = false;
            self.mirror_readiness();
        }
        let paused = self.issuer.issuer().is_paused();
        self.paused.store(paused, Ordering::Release);
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
                if self.issuer.state() == StoreState::Stopped {
                    tracing::error!(
                        mint = %self.issuer.public_key(),
                        %error,
                        "snapshot failed, and the log could not be opened again after it was \
                         rewritten; the mint refuses changes and reads of its ledger until the \
                         server is restarted"
                    );
                    self.mirror_readiness();
                } else {
                    tracing::error!(
                        mint = %self.issuer.public_key(),
                        %error,
                        "snapshot failed; the log keeps growing"
                    );
                }
            }
        }
    }

    /// Rebuilds the issuer from what reached the disk if the last batch did
    /// not, which needs nothing written, so the mint answers from what
    /// reached it. If that fails, the issuer stays ahead, and the next change
    /// or read tries again.
    fn recover(&mut self) {
        if self.issuer.state() != StoreState::Ahead {
            return;
        }
        let recovered = self.issuer.recover();
        self.mirror_readiness();
        match recovered {
            Ok(()) => tracing::info!(
                mint = %self.issuer.public_key(),
                "reread the mint's state from the disk after a batch did not reach it"
            ),
            Err(error) => tracing::error!(
                mint = %self.issuer.public_key(),
                %error,
                "a batch did not reach the disk, and the mint could not reread its state from \
                 the disk; it refuses changes and reads of its ledger until it can, and tries \
                 again at its next request"
            ),
        }
    }

    /// Refuses a read while the issuer may hold what never reached the disk.
    fn check_in_step(&self) -> Result<(), ApiError> {
        let message = match self.issuer.state() {
            StoreState::InStep => return Ok(()),
            StoreState::Ahead => {
                "the mint's last change did not reach its disk, and the mint could not reread \
                 its state from the disk; it tries again at its next request"
            }
            StoreState::Stopped => {
                "a change to the mint's log failed and could not be undone; it answers again \
                 once the server is restarted"
            }
        };
        Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage",
            message,
        ))
    }

    /// Mirrors whether the mint serves for readiness checks.
    fn mirror_readiness(&self) {
        let readiness = match self.issuer.state() {
            StoreState::InStep if self.failing => Readiness::Failing,
            StoreState::InStep => Readiness::Serving,
            StoreState::Ahead => Readiness::Ahead,
            StoreState::Stopped => Readiness::Stopped,
        };
        *self.readiness.write().unwrap_or_else(|e| e.into_inner()) = readiness;
    }
}

/// Warns when the mint's receipts cap the transfers and burns it takes, as
/// the defaults of an older server can: it then keeps `receipt_capacity`
/// receipts, each for at least `receipt_min_age_secs`.
fn warn_if_capped(mint: &PublicKey, policy: &Policy) {
    let (capacity, min_age) = (policy.receipt_capacity, policy.receipt_min_age_secs);
    if capacity == 0 || min_age == 0 {
        return;
    }
    let ceiling = capacity / min_age;
    if ceiling < LOW_CEILING {
        tracing::warn!(
            %mint,
            receipt_capacity = capacity,
            receipt_min_age_secs = min_age,
            "the mint takes at most {ceiling} transfers and burns a second, sustained; raise \
             receipt_capacity in its policy.json to take more"
        );
    }
}

/// Takes every change waiting, up to [`MAX_BATCH`], commits them together on
/// the blocking pool and answers each. Returns once the mint is gone. An idle
/// mint holds no thread.
async fn run_committer(inner: Arc<Mutex<Inner>>, mut queue: mpsc::Receiver<Job>) {
    while let Some(first) = queue.recv().await {
        let (changes, replies) = take_waiting(first, &mut queue);
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
        answer(replies, results);
    }
}

type Reply = oneshot::Sender<Result<Done, ApiError>>;

/// `first` and every job waiting behind it, up to [`MAX_BATCH`].
fn take_waiting(first: Job, queue: &mut mpsc::Receiver<Job>) -> (Vec<Change>, Vec<Reply>) {
    let mut jobs = vec![first];
    while jobs.len() < MAX_BATCH {
        match queue.try_recv() {
            Ok(job) => jobs.push(job),
            Err(_) => break,
        }
    }
    jobs.into_iter().map(|job| (job.change, job.reply)).unzip()
}

fn answer(replies: Vec<Reply>, results: Vec<Result<Done, ApiError>>) {
    for (reply, result) in replies.into_iter().zip(results) {
        // The client may have gone away; the change stands regardless.
        let _ = reply.send(result);
    }
}

/// The committer of a replica: takes every change waiting, as
/// [`run_committer`] does, and commits them through the group as one entry.
/// A replica that does not lead refuses them all.
async fn run_replicated(replica: Arc<Replica>, mut queue: mpsc::Receiver<Job>) {
    while let Some(first) = queue.recv().await {
        let (changes, replies) = take_waiting(first, &mut queue);
        let results = commit_replicated(&replica, changes).await;
        answer(replies, results);
    }
}

async fn commit_replicated(replica: &Replica, changes: Vec<Change>) -> Vec<Result<Done, ApiError>> {
    let mut batch = match replica.batch().await {
        Ok(batch) => batch,
        Err(error) => {
            let error = ApiError::from(error);
            return changes.iter().map(|_| Err(error.clone())).collect();
        }
    };
    // Planning signs every new note.
    let results: Vec<_> = blocking(|| {
        changes
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
                Change::Rotate(due) => match due {
                    Some(due) => batch.rotate_scheduled(due),
                    None => batch.rotate(),
                }
                .map(Done::Rotated)
                .map_err(ApiError::from),
                Change::Pause(paused) => {
                    batch.set_paused(paused);
                    Ok(Done::Paused)
                }
            })
            .collect()
    });
    if let Err(error) = batch.commit().await {
        let error = ApiError::from(error);
        return results
            .into_iter()
            .map(|result| result.and(Err(error.clone())))
            .collect();
    }
    results
}

/// Runs `work` without stalling the tasks that share the thread, where the
/// runtime allows it.
fn blocking<T>(work: impl FnOnce() -> T) -> T {
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// Rotates at every multiple of `period` seconds since the Unix epoch, so the
/// schedule survives restarts. Boundaries missed while the server was down,
/// and those reached while the mint is paused, are not made up.
///
/// Each scheduled rotation logs the boundary it was due at, and the issuer
/// refuses one that is not due after the last, so neither a clock that steps
/// backwards nor a restart can rotate twice for one period. For a replica,
/// only the leader rotates; the others follow what the group logs, so a new
/// leader carries on from the last rotation logged. `legacy` is the file an
/// older server kept the last boundary in, with that boundary, removed once
/// a scheduled rotation after it is logged.
async fn rotate_on_schedule(
    mint: Arc<Mint>,
    period: u64,
    mut last: u64,
    mut legacy: Option<(PathBuf, u64)>,
) {
    loop {
        if let Some(logged) = mint.logged_rotation() {
            last = last.max(logged);
            if legacy.as_ref().is_some_and(|&(_, floor)| logged >= floor) {
                remove_legacy(&mint, legacy.take());
            }
        }
        let now = unix_now();
        let next = last.saturating_add(period);
        mint.set_schedule(period, last);
        if now < next {
            // Sleeping is on the monotonic clock; check the wall clock again on waking.
            tokio::time::sleep(Duration::from_secs(next - now)).await;
            continue;
        }
        let due = now / period * period;
        if !mint.leads() {
            if !mint.is_paused() {
                // The leader rotates; look again once it has likely logged it.
                tokio::time::sleep(FOLLOWER_WAIT).await;
                continue;
            }
        } else {
            match mint.rotate(Some(due)).await {
                Ok(report) => {
                    tracing::info!(mint = %mint.key, ?report, "scheduled rotation");
                    remove_legacy(&mint, legacy.take());
                }
                Err(error) if error.code() == "paused" => {
                    tracing::info!(mint = %mint.key, "paused; skipped a scheduled rotation");
                }
                Err(error) if error.code() == "stale_rotation" => {
                    tracing::info!(mint = %mint.key, due, "this period's rotation is already logged");
                }
                Err(error) if error.code() == "no_leader" => {
                    tokio::time::sleep(FOLLOWER_WAIT).await;
                    continue;
                }
                Err(error) => {
                    tracing::error!(mint = %mint.key, ?error, "scheduled rotation failed; retrying");
                    tokio::time::sleep(Duration::from_secs(period.min(60))).await;
                    continue;
                }
            }
        }
        last = due;
    }
}

fn remove_legacy(mint: &Mint, legacy: Option<(PathBuf, u64)>) {
    if let Some((file, _)) = legacy
        && let Err(error) = fs::remove_file(&file)
    {
        tracing::warn!(mint = %mint.key, %error, "could not remove {}", file.display());
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The mints a server hosts, by issuer key.
pub(crate) type Hosted = HashMap<PublicKey, Arc<Mint>>;
