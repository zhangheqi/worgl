//! `worgl-server`: create a server's state directory and its mints, serve
//! them over HTTP, alone or as one member of a cluster, and sign mint
//! requests.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use serde_json::json;
use tracing_subscriber::EnvFilter;
use worgl::log::Payload;
use worgl::{FileLog, LedgerConfig, LogId, MintKey, MintRequest, Policy, PublicKey};
use worgl_server::files::{read_key, write_secret};
use worgl_server::{AppState, ClusterConfig, Member, MintSpec, raft_router, router};

const MINT_KEY_FILE: &str = "mint.key";

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a server's state directory: an operator token and no mints.
    Init {
        /// State directory to create.
        #[arg(long, env = "WORGL_STATE")]
        state: PathBuf,
    },
    /// Create a mint: an issuer key, an admin token, a mint authority key
    /// and an empty ledger. A running server sees it once restarted; POST
    /// /v1/admin/mints creates one while serving, and on a cluster creates
    /// it on every member.
    CreateMint {
        /// State directory made by `init`.
        #[arg(long, env = "WORGL_STATE")]
        state: PathBuf,
        /// Number of layers.
        #[arg(long, default_value_t = 4)]
        layers: u32,
        /// Bits per layer: how many notes can ever enter one generation,
        /// minted or moved in. A multiple of 64.
        #[arg(long, default_value_t = 1 << 20)]
        layer_size: u32,
        /// Policy as a JSON file, copied into the mint's directory. Missing
        /// fields take their cash-like defaults. If it lists no
        /// `mint_authorities`, a mint authority key is created and listed.
        #[arg(long)]
        policy: Option<PathBuf>,
        /// Rotate every this many seconds, at multiples of the period since
        /// the Unix epoch. Without it, rotate through the admin API.
        #[arg(long)]
        rotate_every: Option<u64>,
    },
    /// Sign a mint request and print it as JSON, ready to POST to
    /// /v1/mints/<issuer>/mint.
    SignMint {
        /// File holding the mint authority's secret key, as `create-mint` writes it.
        #[arg(long)]
        key: PathBuf,
        /// The mint's issuer key, as `create-mint` printed it.
        #[arg(long)]
        issuer: PublicKey,
        /// Above every sequence number this key has signed for this mint.
        #[arg(long)]
        sequence: u64,
        /// Value of the note.
        #[arg(long)]
        amount: u64,
        /// The public key the note is bound to.
        #[arg(long)]
        holder: PublicKey,
    },
    /// Print a mint's log for an audit, one JSON object per entry, without
    /// the sealed generation keys its rotations carry. Safe to run while
    /// the mint is being served.
    ExportLog {
        /// The mint's directory, `mints/<issuer key>` in the state directory.
        #[arg(long)]
        mint: PathBuf,
    },
    /// Make a state directory a cluster member's: write its cluster.json and
    /// create a cluster token unless it has one. Every member starts from a
    /// copy of one directory, the token, mints and key-encryption key
    /// included, run with its own --node. To give a member new addresses,
    /// run it again on every member with the same --member list, then
    /// restart the members one at a time.
    JoinCluster {
        /// State directory made by `init`.
        #[arg(long, env = "WORGL_STATE")]
        state: PathBuf,
        /// This member's id.
        #[arg(long)]
        node: u64,
        /// A member as ID=API,RAFT, where API is the host:port other members
        /// pass API requests on to and RAFT that of its Raft port, both on a
        /// network only the members and a proxy in front of them can reach.
        /// Give every member, this one included.
        #[arg(long = "member", value_parser = parse_member, required = true)]
        members: Vec<(u64, Member)>,
    },
    /// Serve every mint in a state directory over HTTP. A cluster member
    /// also serves its Raft port.
    Serve {
        /// State directory made by `init`.
        #[arg(long, env = "WORGL_STATE")]
        state: PathBuf,
        /// Address to listen on.
        #[arg(long, env = "WORGL_LISTEN", default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
        /// Address a cluster member listens on for the other members' Raft
        /// requests, if not its Raft address in cluster.json.
        #[arg(long, env = "WORGL_RAFT_LISTEN")]
        raft_listen: Option<SocketAddr>,
        /// Write a fresh snapshot of a mint after this many logged events.
        #[arg(long, default_value_t = 100_000)]
        compact_every: u64,
    },
}

fn parse_member(text: &str) -> Result<(u64, Member), String> {
    let (id, addresses) = text
        .split_once('=')
        .ok_or("expected ID=API,RAFT".to_owned())?;
    let id = id.parse().map_err(|_| format!("{id} is not a member id"))?;
    let (api, raft) = addresses
        .split_once(',')
        .ok_or("expected ID=API,RAFT".to_owned())?;
    let member = Member {
        api: api.to_owned(),
        raft: raft.to_owned(),
    };
    Ok((id, member))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    match Cli::parse().command {
        Command::Init { state } => {
            worgl_server::init_state(&state)?;
            println!("operator token {}", state.join("operator.token").display());
            Ok(())
        }
        Command::CreateMint {
            state,
            layers,
            layer_size,
            policy,
            rotate_every,
        } => {
            let policy = match policy {
                Some(path) => load_policy(&path)?,
                None => Policy::cash_like(),
            };
            let spec = MintSpec {
                ledger: LedgerConfig { layers, layer_size },
                policy,
                rotate_every,
            };
            create_mint(&state, spec)
        }
        Command::SignMint {
            key,
            issuer,
            sequence,
            amount,
            holder,
        } => {
            let key = MintKey::from_secret_bytes(&read_key(&key)?);
            let request = MintRequest::sign(&key, &issuer, sequence, amount, holder);
            println!("{}", serde_json::to_string_pretty(&request)?);
            Ok(())
        }
        Command::ExportLog { mint } => export_log(&mint),
        Command::JoinCluster {
            state,
            node,
            members,
        } => {
            let members: BTreeMap<_, _> = members.into_iter().collect();
            let config = ClusterConfig { node, members };
            let changed = worgl_server::join_cluster(&state, &config)?;
            println!("cluster       {}", state.join("cluster.json").display());
            println!("cluster token {}", state.join("cluster.token").display());
            if !changed.is_empty() {
                let ids: Vec<_> = changed.iter().map(u64::to_string).collect();
                let ids = ids.join(", ");
                println!("moved members {ids}");
                let s = if changed.len() == 1 { "" } else { "s" };
                eprintln!(
                    "warning: this changes the addresses of member{s} {ids}; give every member's \
                     cluster.json the same addresses, with join-cluster or by hand, then restart \
                     the members one at a time"
                );
                if changed.len() > 1 {
                    eprintln!(
                        "warning: move one member at a time, restarting every member before \
                         moving the next: with two moved at once, a restart can leave the running \
                         members unable to reach each other"
                    );
                }
            }
            Ok(())
        }
        Command::Serve {
            state,
            listen,
            raft_listen,
            compact_every,
        } => {
            if worgl_server::is_member(&state) {
                serve_member(&state, listen, raft_listen, compact_every).await
            } else if raft_listen.is_some() {
                bail!(
                    "{} is not a cluster member's, so it has no Raft port",
                    state.display()
                )
            } else {
                serve(&state, listen, compact_every).await
            }
        }
    }
}

fn load_policy(path: &Path) -> Result<Policy> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing policy {}", path.display()))
}

fn create_mint(state: &Path, mut spec: MintSpec) -> Result<()> {
    let mint_key = spec
        .policy
        .mint_authorities
        .is_empty()
        .then(MintKey::generate);
    if let Some(mint_key) = &mint_key {
        spec.policy.mint_authorities.push(mint_key.public_key());
    }
    let created = worgl_server::create_mint(state, &spec)?;
    let dir = state.join("mints").join(created.mint.to_string());
    println!("mint        {}", created.mint);
    println!("admin token {}", dir.join("admin.token").display());
    println!("policy      {}", dir.join("policy.json").display());
    if let Some(mint_key) = &mint_key {
        let path = dir.join(MINT_KEY_FILE);
        write_secret(&path, &hex::encode(mint_key.to_secret_bytes()))?;
        println!(
            "mint key    {} (the server does not need it; keep it where you mint from)",
            path.display()
        );
    }
    if worgl_server::is_member(state) {
        println!(
            "copy {} to every other member before any of them starts",
            dir.display()
        );
    }
    Ok(())
}

fn export_log(dir: &Path) -> Result<()> {
    let issuer: PublicKey = dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse().ok())
        .ok_or_else(|| {
            anyhow!(
                "{} is not a mint's directory, which is named by its issuer key",
                dir.display()
            )
        })?;
    let entries = FileLog::export(dir, &issuer)
        .with_context(|| format!("reading the log in {}", dir.display()))?;
    let mut out = std::io::stdout().lock();
    for entry in entries {
        let LogId { term, node, index } = entry.id;
        let mut line = json!({"index": index, "term": term, "node": node});
        match entry.payload {
            Payload::Blank => {}
            Payload::Events { first, events, .. } => {
                line["first"] = json!(first);
                line["events"] = json!(events);
            }
            Payload::Membership(bytes) => line["membership"] = json!(hex::encode(bytes)),
        }
        writeln!(out, "{line}")?;
    }
    Ok(())
}

async fn serve(state: &Path, listen: SocketAddr, compact_every: u64) -> Result<()> {
    let app = AppState::open(state, compact_every)
        .with_context(|| format!("opening {}", state.display()))?;
    tracing::info!(mints = app.mints().len(), "state loaded");
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

/// Serves a cluster member: its public API on `listen`, and its Raft port.
async fn serve_member(
    state: &Path,
    listen: SocketAddr,
    raft_listen: Option<SocketAddr>,
    compact_every: u64,
) -> Result<()> {
    let config = ClusterConfig::read(state)?;
    let app = AppState::open_member(state, compact_every)
        .await
        .with_context(|| format!("opening {}", state.display()))?;
    tracing::info!(
        mints = app.mints().len(),
        node = config.node,
        "state loaded"
    );
    let raft = match raft_listen {
        Some(address) => address.to_string(),
        None => config.members[&config.node].raft.clone(),
    };
    let raft_listener = tokio::net::TcpListener::bind(&raft)
        .await
        .with_context(|| format!("binding {raft}"))?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!(%listen, %raft, "serving");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let members = raft_router(app.clone());
    let raft = tokio::spawn(async move {
        axum::serve(raft_listener, members)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    axum::serve(listener, router(app.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    // Stop the replicas while the Raft port still answers, so the others
    // hear of it and elect new leaders.
    app.shutdown().await;
    let _ = stop.send(());
    raft.await??;
    tracing::info!("stopped");
    Ok(())
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
