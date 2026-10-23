//! `worgl-faucet`: hand out notes of a playground mint over HTTP.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;
use worgl::{MintKey, PublicKey};
use worgl_client::Client;
use worgl_faucet::{Config, Faucet, Limits, Origin, router};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// The mint's server. Give every member of a cluster: a request with no
    /// clear answer goes again to the next one.
    #[arg(
        long = "server",
        env = "WORGL_SERVER",
        value_delimiter = ',',
        required = true
    )]
    servers: Vec<String>,
    /// The mint's issuer key.
    #[arg(long, env = "WORGL_MINT")]
    mint: PublicKey,
    /// File holding a mint authority's secret key, as `worgl-server
    /// create-mint` writes it. The mint must list the key in its policy.
    #[arg(long, env = "WORGL_FAUCET_KEY")]
    key: PathBuf,
    /// Address to listen on.
    #[arg(long, env = "WORGL_LISTEN", default_value = "127.0.0.1:8090")]
    listen: SocketAddr,
    /// The value of each note.
    #[arg(long, default_value_t = Limits::default().amount)]
    amount: u64,
    /// Notes one client address may get in any hour.
    #[arg(long, default_value_t = Limits::default().per_address_per_hour)]
    per_address: u32,
    /// Notes handed out in a day, UTC.
    #[arg(long, default_value_t = Limits::default().per_day)]
    per_day: u32,
    /// Count clients by the last address in X-Forwarded-For. Only behind a
    /// proxy that appends it.
    #[arg(long)]
    behind_proxy: bool,
    /// An origin whose pages may ask the faucet from a browser, as
    /// https://purse.example. Repeat it, or separate origins with commas.
    /// Without any, the faucet sends no CORS headers. `*` is refused: any
    /// website could then collect notes through its visitors' addresses.
    #[arg(
        long = "allow-origin",
        env = "WORGL_FAUCET_ALLOW_ORIGIN",
        value_name = "ORIGIN",
        value_delimiter = ','
    )]
    allow_origins: Vec<Origin>,
}

fn read_key(path: &Path) -> Result<MintKey> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut secret = [0u8; 32];
    hex::decode_to_slice(text.trim(), &mut secret)
        .with_context(|| format!("{} must hold 64 hex digits", path.display()))?;
    Ok(MintKey::from_secret_bytes(&secret))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let servers = cli
        .servers
        .iter()
        .map(|url| Client::new(url).with_context(|| format!("server {url}")))
        .collect::<Result<Vec<_>>>()?;
    let authority = read_key(&cli.key)?;
    let faucet = Faucet::new(Config {
        servers,
        mint: cli.mint,
        authority,
        limits: Limits {
            amount: cli.amount,
            per_address_per_hour: cli.per_address,
            per_day: cli.per_day,
        },
        behind_proxy: cli.behind_proxy,
        allow_origins: cli.allow_origins,
    });
    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("binding {}", cli.listen))?;
    tracing::info!(listen = %cli.listen, mint = %cli.mint, "serving");
    axum::serve(
        listener,
        router(faucet).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
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
