//! The faucet against a real server on a local port.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use reqwest::StatusCode;
use serde_json::{Value, json};
use worgl::{HolderKey, LedgerConfig, MintKey, Note, NoteStatus, Policy, PublicKey};
use worgl_client::Client;
use worgl_faucet::{About, Config, Faucet, Limits, router};
use worgl_server::{AppState, MintSpec, init_state};

/// A server on a local port with one mint.
struct Mint {
    url: String,
    mint: PublicKey,
    token: String,
    /// The key the mint lists as its authority.
    authority: MintKey,
    _dir: tempfile::TempDir,
}

impl Mint {
    async fn start() -> Self {
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
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = worgl_server::router(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url,
            mint: created.mint,
            token: created.token,
            authority,
            _dir: dir,
        }
    }

    fn client(&self) -> Client {
        Client::new(&self.url).unwrap()
    }

    fn config(&self, limits: Limits) -> Config {
        Config {
            servers: vec![self.client()],
            mint: self.mint,
            authority: MintKey::from_secret_bytes(&self.authority.to_secret_bytes()),
            limits,
            behind_proxy: false,
        }
    }
}

/// Serves a faucet on a local port and returns its URL.
async fn serve(config: Config) -> String {
    let faucet = Faucet::new(config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = router(faucet).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn limits(per_address_per_hour: u32, per_day: u32) -> Limits {
    Limits {
        amount: 64,
        per_address_per_hour,
        per_day,
    }
}

/// Asks the faucet at `url` for a note to a fresh key, as if from
/// `forwarded_for` when given.
async fn ask(url: &str, forwarded_for: Option<&str>) -> (PublicKey, reqwest::Response) {
    let holder = HolderKey::generate().public_key();
    let mut request = reqwest::Client::new()
        .post(format!("{url}/v1/faucet"))
        .json(&json!({ "holder": holder }));
    if let Some(address) = forwarded_for {
        request = request.header("x-forwarded-for", address);
    }
    (holder, request.send().await.unwrap())
}

/// Asks for a note and expects one.
async fn note(url: &str, forwarded_for: Option<&str>) -> Note {
    let (holder, response) = ask(url, forwarded_for).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    let note: Note = serde_json::from_value(body["note"].clone()).unwrap();
    assert_eq!(note.holder(), &holder);
    note
}

/// Asks for a note and expects `status` with error `code`.
async fn refused(url: &str, forwarded_for: Option<&str>, status: StatusCode, code: &str) {
    let (_, response) = ask(url, forwarded_for).await;
    assert_eq!(response.status(), status);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], code, "{body}");
}

async fn about(url: &str) -> About {
    reqwest::get(format!("{url}/v1/faucet"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_holder_gets_a_live_note_of_the_mint() {
    let mint = Mint::start().await;
    let url = serve(mint.config(Limits::default())).await;

    let before = about(&url).await;
    assert_eq!(before.mint, mint.mint);
    assert_eq!(before.limits, Limits::default());
    assert_eq!(before.left_today, 10_000);

    let note = note(&url, None).await;
    assert_eq!(note.issuer(), &mint.mint);
    assert_eq!(note.amount(), 64);
    let verified = mint.client().verify(&note).await.unwrap();
    assert!(matches!(verified.status, NoteStatus::Live { .. }));
    assert_eq!(about(&url).await.left_today, 9_999);

    let health = reqwest::get(format!("{url}/healthz")).await.unwrap();
    assert_eq!(health.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn an_address_gets_its_notes_for_the_hour_and_no_more() {
    let mint = Mint::start().await;
    let url = serve(mint.config(limits(2, 100))).await;

    note(&url, None).await;
    note(&url, None).await;
    let (_, response) = ask(&url, None).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = response.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((3_500..=3_600).contains(&wait), "{wait}");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], "rate_limited");

    // Without a proxy in front, a client cannot name another address.
    refused(
        &url,
        Some("203.0.113.9"),
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
    )
    .await;
    // A refused request takes nothing from the day's notes.
    assert_eq!(about(&url).await.left_today, 98);
}

#[tokio::test]
async fn behind_a_proxy_clients_count_by_the_address_it_appends() {
    let mint = Mint::start().await;
    let mut config = mint.config(limits(1, 100));
    config.behind_proxy = true;
    let url = serve(config).await;

    note(&url, Some("203.0.113.1")).await;
    refused(
        &url,
        Some("203.0.113.1"),
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
    )
    .await;
    // The proxy appends the address it saw; whatever came before is the
    // client's to write.
    refused(
        &url,
        Some("198.51.100.4, 203.0.113.1"),
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
    )
    .await;
    note(&url, Some("203.0.113.1, 203.0.113.2")).await;
    // One IPv6 host picks from a whole /64.
    note(&url, Some("2001:db8:1:2::1")).await;
    refused(
        &url,
        Some("2001:db8:1:2::ffff"),
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
    )
    .await;
}

#[tokio::test]
async fn the_faucet_hands_out_its_notes_for_the_day_and_no_more() {
    let mint = Mint::start().await;
    let url = serve(mint.config(limits(100, 3))).await;

    for _ in 0..3 {
        note(&url, None).await;
    }
    let (_, response) = ask(&url, None).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = response.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=86_400).contains(&wait), "{wait}");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], "exhausted");
    assert_eq!(about(&url).await.left_today, 0);
}

#[tokio::test]
async fn requests_at_once_each_get_their_own_note() {
    let mint = Mint::start().await;
    let url = Arc::new(serve(mint.config(limits(100, 100))).await);

    let asks: Vec<_> = (0..20)
        .map(|_| {
            let url = url.clone();
            tokio::spawn(async move { note(&url, None).await })
        })
        .collect();
    let mut serials = HashSet::new();
    for ask in asks {
        let note = ask.await.unwrap();
        assert!(serials.insert(note.serial()));
        let verified = mint.client().verify(&note).await.unwrap();
        assert!(matches!(verified.status, NoteStatus::Live { .. }));
    }
}

#[tokio::test]
async fn a_server_that_does_not_answer_is_passed_over() {
    let mint = Mint::start().await;
    let gone = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let mut config = mint.config(Limits::default());
    config.servers.insert(0, Client::new(&gone).unwrap());
    let url = serve(config).await;

    for _ in 0..3 {
        let note = note(&url, None).await;
        let verified = mint.client().verify(&note).await.unwrap();
        assert!(matches!(verified.status, NoteStatus::Live { .. }));
    }
}

#[tokio::test]
async fn a_request_that_is_not_a_holder_key_is_malformed() {
    let mint = Mint::start().await;
    let url = serve(mint.config(limits(1, 100))).await;
    let http = reqwest::Client::new();
    for body in [json!({"holder": "nope"}), json!({}), json!([1, 2])] {
        let response = http
            .post(format!("{url}/v1/faucet"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"], "malformed");
    }
    // Malformed requests do not count against the address.
    note(&url, None).await;
}

#[tokio::test]
async fn a_paused_mint_is_passed_on() {
    let mint = Mint::start().await;
    let url = serve(mint.config(Limits::default())).await;
    mint.client().pause(&mint.mint, &mint.token).await.unwrap();
    refused(&url, None, StatusCode::SERVICE_UNAVAILABLE, "paused").await;
    mint.client().resume(&mint.mint, &mint.token).await.unwrap();
    note(&url, None).await;
}

#[tokio::test]
async fn a_key_the_mint_does_not_list_gets_nothing() {
    let mint = Mint::start().await;
    let mut config = mint.config(Limits::default());
    config.authority = MintKey::generate();
    let url = serve(config).await;
    refused(&url, None, StatusCode::INTERNAL_SERVER_ERROR, "internal").await;
}
