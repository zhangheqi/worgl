//! `worgl-server`: create an issuer's state directory, then serve it over HTTP.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use worgl::{Issuer, IssuerKey, LedgerConfig, PersistentIssuer, Policy};
use worgl_server::{AppState, router};

const KEY_FILE: &str = "issuer.key";
const TOKEN_FILE: &str = "admin.token";
const POLICY_FILE: &str = "policy.json";
const SCHEDULE_FILE: &str = "last_rotation";

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new issuer: a key, an admin token and an empty ledger.
    Init {
        /// State directory to create.
        #[arg(long, env = "WORGL_STATE")]
        state: PathBuf,
        /// Number of layers.
        #[arg(long, default_value_t = 4)]
        layers: u32,
        /// Notes each layer can hold per generation; a multiple of 64.
        #[arg(long, default_value_t = 1 << 20)]
        layer_size: u32,
        /// Policy as a JSON file, copied into the state directory. Missing
        /// fields take their cash-like defaults.
        #[arg(long)]
        policy: Option<PathBuf>,
    },
    /// Serve an issuer over HTTP.
    Serve {
        /// State directory made by `init`.
        #[arg(long, env = "WORGL_STATE")]
        state: PathBuf,
        /// Address to listen on.
        #[arg(long, env = "WORGL_LISTEN", default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
        /// Rotate every this many seconds, at multiples of the period since
        /// the Unix epoch. Without it, rotate through the admin API.
        #[arg(long)]
        rotate_every: Option<u64>,
        /// Policy as a JSON file, instead of the one in the state directory.
        #[arg(long)]
        policy: Option<PathBuf>,
        /// Write a fresh snapshot after this many journaled events.
        #[arg(long, default_value_t = 100_000)]
        compact_every: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    match Cli::parse().command {
        Command::Init {
            state,
            layers,
            layer_size,
            policy,
        } => {
            let policy = match policy {
                Some(path) => load_policy(&path)?,
                None => Policy::cash_like(),
            };
            init(&state, LedgerConfig { layers, layer_size }, policy)
        }
        Command::Serve {
            state,
            listen,
            rotate_every,
            policy,
            compact_every,
        } => {
            let policy = load_policy(&policy.unwrap_or_else(|| state.join(POLICY_FILE)))?;
            serve(&state, listen, rotate_every, policy, compact_every).await
        }
    }
}

fn load_policy(path: &Path) -> Result<Policy> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing policy {}", path.display()))
}

fn init(state: &Path, ledger: LedgerConfig, policy: Policy) -> Result<()> {
    if state.join(KEY_FILE).exists() {
        bail!("{} already holds an issuer", state.display());
    }
    let key = IssuerKey::generate();
    let issuer = Issuer::new(key.clone(), ledger, policy.clone())?;
    fs::create_dir_all(state)?;
    fs::write(
        state.join(POLICY_FILE),
        serde_json::to_string_pretty(&policy)? + "\n",
    )?;
    let mut token = [0u8; 32];
    getrandom::fill(&mut token).map_err(|e| anyhow::anyhow!("system RNG failed: {e}"))?;
    write_secret(&state.join(KEY_FILE), &hex::encode(key.to_secret_bytes()))?;
    write_secret(&state.join(TOKEN_FILE), &hex::encode(token))?;
    PersistentIssuer::create(state, issuer)?;
    println!("issuer key  {}", key.public_key());
    println!("admin token {}", state.join(TOKEN_FILE).display());
    println!("policy      {}", state.join(POLICY_FILE).display());
    Ok(())
}

fn write_secret(path: &Path, text: &str) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    writeln!(file, "{text}")?;
    file.sync_all()?;
    Ok(())
}

fn read_secret(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(text.trim().to_owned())
}

async fn serve(
    state: &Path,
    listen: SocketAddr,
    rotate_every: Option<u64>,
    policy: Policy,
    compact_every: u64,
) -> Result<()> {
    let mut secret = [0u8; 32];
    hex::decode_to_slice(read_secret(&state.join(KEY_FILE))?, &mut secret)
        .context("issuer.key must hold 64 hex digits")?;
    let key = IssuerKey::from_secret_bytes(&secret);
    let token = read_secret(&state.join(TOKEN_FILE))?;
    let issuer = PersistentIssuer::open(state, key, policy)
        .with_context(|| format!("opening {}", state.display()))?;
    tracing::info!(
        issuer = %issuer.public_key(),
        sequence = issuer.issuer().sequence(),
        "issuer loaded"
    );
    let app = AppState::new(issuer, &token, compact_every)
        .with_context(|| format!("checking {}", state.join(TOKEN_FILE).display()))?;

    if let Some(period) = rotate_every {
        if period == 0 {
            bail!("--rotate-every must be at least 1 second");
        }
        tokio::spawn(rotate_on_schedule(
            app.clone(),
            period,
            state.join(SCHEDULE_FILE),
        ));
    }

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!(%listen, "serving");
    axum::serve(listener, router(app))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("stopped");
    Ok(())
}

/// Rotates at every multiple of `period` seconds since the Unix epoch, so the
/// schedule survives restarts. Boundaries missed while the server was down
/// are not made up.
///
/// The boundary of the last scheduled rotation is kept in `marker`, and a
/// rotation happens only once the wall clock has passed the boundary after
/// it, so a clock that steps backwards can never rotate twice for one period.
async fn rotate_on_schedule(app: AppState, period: u64, marker: PathBuf) {
    let saved = fs::read_to_string(&marker)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let mut last = saved.max(unix_now() / period * period);
    loop {
        let now = unix_now();
        let next = last.saturating_add(period);
        if now < next {
            // Sleeping is on the monotonic clock; check the wall clock again on waking.
            tokio::time::sleep(Duration::from_secs(next - now)).await;
            continue;
        }
        match app.rotate().await {
            Ok(report) => tracing::info!(?report, "scheduled rotation"),
            Err(error) => {
                tracing::error!(?error, "scheduled rotation failed; retrying");
                tokio::time::sleep(Duration::from_secs(period.min(60))).await;
                continue;
            }
        }
        last = now / period * period;
        let tmp = marker.with_extension("tmp");
        let saved = fs::write(&tmp, format!("{last}\n")).and_then(|()| fs::rename(&tmp, &marker));
        if let Err(error) = saved {
            tracing::error!(%error, "could not record the rotation time");
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
