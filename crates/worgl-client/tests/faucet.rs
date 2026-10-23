//! A purse taking notes from a playground faucet, both on local ports.

use std::net::SocketAddr;

use worgl::{LedgerConfig, MintKey, Policy, PublicKey};
use worgl_client::{Client, MemoryStore, Purse};
use worgl_faucet::{Config, Faucet, Limits};
use worgl_server::{AppState, MintSpec, init_state, router};

/// A server with one mint, and a faucet for it.
struct Playground {
    server: Client,
    faucet: Client,
    mint: PublicKey,
    _dir: tempfile::TempDir,
}

impl Playground {
    async fn start(limits: Limits) -> Self {
        let dir = tempfile::tempdir().unwrap();
        init_state(dir.path()).unwrap();
        let state = AppState::open(dir.path(), 1000).unwrap();
        let authority = MintKey::generate();
        let mut policy = Policy::cash_like();
        policy.mint_authorities = vec![authority.public_key()];
        let created = state
            .create_mint(MintSpec {
                ledger: LedgerConfig {
                    layers: 4,
                    layer_size: 1024,
                },
                policy,
                rotate_every: None,
            })
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Client::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });

        let faucet = Faucet::new(Config {
            servers: vec![server.clone()],
            mint: created.mint,
            authority,
            limits,
            behind_proxy: false,
            allow_origins: Vec::new(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = worgl_faucet::router(faucet).into_make_service_with_connect_info::<SocketAddr>();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            server,
            faucet: Client::new(&url).unwrap(),
            mint: created.mint,
            _dir: dir,
        }
    }

    fn purse(&self) -> Purse<MemoryStore> {
        Purse::new(self.server.clone(), Default::default(), MemoryStore::new())
    }
}

#[tokio::test]
async fn a_purse_keeps_what_the_faucet_hands_out() {
    let playground = Playground::start(Limits::default()).await;
    let about = playground.faucet.faucet().await.unwrap();
    assert_eq!(about.mint, playground.mint);
    assert_eq!(about.amount, 64);
    assert_eq!(about.left_today, 10_000);

    let purse = playground.purse();
    for _ in 0..2 {
        let (mint, value) = purse.take_from_faucet(&playground.faucet).await.unwrap();
        assert_eq!(mint, playground.mint);
        assert_eq!(value, 64);
    }
    let balance = purse.balance(&playground.mint).await.unwrap();
    assert_eq!(balance.spendable, 128);
    assert_eq!(balance.expected, 0);
    // Each note went to a key of its own.
    let holder = purse.holder();
    let notes: Vec<_> = holder.notes().collect();
    assert_eq!(notes.len(), 2);
    assert_ne!(notes[0].holder(), notes[1].holder());
    assert_eq!(playground.faucet.faucet().await.unwrap().left_today, 9_998);
}

#[tokio::test]
async fn a_refusal_leaves_nothing_expected() {
    let playground = Playground::start(Limits {
        amount: 64,
        per_address_per_hour: 1,
        per_day: 100,
    })
    .await;
    let purse = playground.purse();
    purse.take_from_faucet(&playground.faucet).await.unwrap();
    let error = purse
        .take_from_faucet(&playground.faucet)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some("rate_limited"));
    assert!(!error.is_transient());
    let balance = purse.balance(&playground.mint).await.unwrap();
    assert_eq!(balance.spendable, 64);
    assert_eq!(balance.expected, 0);
}

#[tokio::test]
async fn a_faucet_amount_the_mint_cannot_make_as_one_note_is_refused_first() {
    let playground = Playground::start(Limits {
        amount: 48,
        per_address_per_hour: 10,
        per_day: 100,
    })
    .await;
    let purse = playground.purse();
    let error = purse
        .take_from_faucet(&playground.faucet)
        .await
        .unwrap_err();
    assert!(
        matches!(error, worgl_client::Error::FaucetAmount(48)),
        "{error}"
    );
    let balance = purse.balance(&playground.mint).await.unwrap();
    assert_eq!(balance.expected, 0);
    // The faucet was never asked.
    assert_eq!(playground.faucet.faucet().await.unwrap().left_today, 100);
}
