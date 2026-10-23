//! The faucet against a real server on a local port.

use std::collections::HashSet;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use worgl::{HolderKey, LedgerConfig, MintKey, Note, NoteStatus, Policy, PublicKey};
use worgl_client::Client;
use worgl_faucet::{About, Config, Faucet, Limits, Origin, router};
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
            allow_origins: Vec::new(),
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

/// A port on 127.0.0.1 kept for one test while this lives, with nothing
/// listening on it.
///
/// It is below the ports the kernel picks for a bind to port 0 or an
/// outgoing connection, so no other socket takes it, and a connection to it
/// is refused. Tests, in this process or another, claim such a port by
/// binding a UDP socket to it, which only one of them can, and leave the
/// TCP port free.
struct Port {
    address: SocketAddr,
    _claim: UdpSocket,
}

impl Port {
    fn new() -> Self {
        let ephemeral = first_ephemeral_port();
        for port in (1024..ephemeral).rev() {
            let address = SocketAddr::from(([127, 0, 0, 1], port));
            // Claimed by another test, or used by another program.
            let Ok(claim) = UdpSocket::bind(address) else {
                continue;
            };
            if std::net::TcpListener::bind(address).is_ok() {
                return Self {
                    address,
                    _claim: claim,
                };
            }
        }
        panic!("no port below {ephemeral} is free");
    }
}

/// The first port the kernel picks itself: on Linux the start of
/// `net.ipv4.ip_local_port_range`, and elsewhere 49152, where macOS starts.
fn first_ephemeral_port() -> u16 {
    if cfg!(target_os = "linux") {
        let range = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").unwrap();
        range.split_whitespace().next().unwrap().parse().unwrap()
    } else {
        49152
    }
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

/// The origin of the page in the tests that name one.
const PURSE: &str = "https://purse.example";

fn origins(origins: &[&str]) -> Vec<Origin> {
    origins
        .iter()
        .map(|origin| origin.parse().unwrap())
        .collect()
}

/// Asks the faucet at `url` for a note to a fresh key, as a browser does
/// from a page on `origin`, with `headers` besides.
async fn ask_from(url: &str, origin: &str, headers: &[(&str, &str)]) -> reqwest::Response {
    let holder = HolderKey::generate().public_key();
    let mut request = reqwest::Client::new()
        .post(format!("{url}/v1/faucet"))
        .header("origin", origin)
        .json(&json!({ "holder": holder }));
    for &(name, value) in headers {
        request = request.header(name, value);
    }
    request.send().await.unwrap()
}

/// What a browser sends before asking for a note from a page on `origin`.
async fn preflight(url: &str, origin: &str) -> reqwest::Response {
    reqwest::Client::new()
        .request(Method::OPTIONS, format!("{url}/v1/faucet"))
        .header("origin", origin)
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "content-type")
        .send()
        .await
        .unwrap()
}

/// `GET /v1/faucet` from a page on `origin`.
async fn about_from(url: &str, origin: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{url}/v1/faucet"))
        .header("origin", origin)
        .send()
        .await
        .unwrap()
}

/// The answer's `name` header, if it has one.
fn header<'a>(response: &'a reqwest::Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .map(|value| value.to_str().unwrap())
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
    let gone = Port::new();
    let mut config = mint.config(Limits::default());
    config
        .servers
        .insert(0, Client::new(&format!("http://{}", gone.address)).unwrap());
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

#[tokio::test]
async fn a_page_on_a_named_origin_reads_the_answers() {
    let mint = Mint::start().await;
    let mut config = mint.config(limits(1, 100));
    config.allow_origins = origins(&[PURSE, "http://localhost:5173"]);
    let url = serve(config).await;

    let response = ask_from(&url, PURSE, &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, "access-control-allow-origin"),
        Some(PURSE)
    );
    assert_eq!(header(&response, "vary"), Some("origin"));
    let body: Value = response.json().await.unwrap();
    assert!(body["note"].is_string(), "{body}");

    // Past the limit, the page can read when to ask again.
    let response = ask_from(&url, PURSE, &[]).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        header(&response, "access-control-allow-origin"),
        Some(PURSE)
    );
    assert_eq!(
        header(&response, "access-control-expose-headers"),
        Some("retry-after")
    );
    assert!(header(&response, "retry-after").is_some());

    let response = about_from(&url, "http://localhost:5173").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, "access-control-allow-origin"),
        Some("http://localhost:5173")
    );
    assert_eq!(header(&response, "vary"), Some("origin"));
}

#[tokio::test]
async fn a_preflight_is_answered_for_a_named_origin_only() {
    let mint = Mint::start().await;
    let mut config = mint.config(Limits::default());
    config.allow_origins = origins(&[PURSE]);
    let url = serve(config).await;

    let response = preflight(&url, PURSE).await;
    assert!(response.status().is_success(), "{}", response.status());
    assert_eq!(
        header(&response, "access-control-allow-origin"),
        Some(PURSE)
    );
    let methods = header(&response, "access-control-allow-methods").unwrap();
    assert!(
        methods.split(',').any(|method| method.trim() == "POST"),
        "{methods}"
    );
    assert_eq!(
        header(&response, "access-control-allow-headers"),
        Some("content-type")
    );
    assert_eq!(header(&response, "access-control-max-age"), Some("7200"));
    assert_eq!(header(&response, "vary"), Some("origin"));

    // The origin must match exactly; a browser that gets no
    // Access-Control-Allow-Origin never sends the request.
    for other in [
        "https://elsewhere.example",
        "https://purse.example.elsewhere.example",
        "https://PURSE.example",
        "https://purse.example/",
        "https://purse.example:8443",
        "http://purse.example",
        "null",
    ] {
        let response = preflight(&url, other).await;
        assert_eq!(
            header(&response, "access-control-allow-origin"),
            None,
            "{other}"
        );
        assert_eq!(header(&response, "vary"), Some("origin"));
    }
}

#[tokio::test]
async fn a_page_on_another_origin_gets_no_note() {
    let mint = Mint::start().await;
    let mut config = mint.config(limits(1, 100));
    config.allow_origins = origins(&[PURSE]);
    let url = serve(config).await;

    for other in ["https://elsewhere.example", "http://purse.example", "null"] {
        for site in ["cross-site", "same-site"] {
            let response = ask_from(&url, other, &[("sec-fetch-site", site)]).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{other} {site}");
            assert_eq!(header(&response, "access-control-allow-origin"), None);
            assert_eq!(header(&response, "vary"), Some("origin"));
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["error"], "unknown_origin", "{body}");
        }
    }
    // A browser without Sec-Fetch-Site is refused all the same.
    let response = ask_from(&url, "https://elsewhere.example", &[]).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    // So is a form, which a browser sends without a preflight.
    let response = reqwest::Client::new()
        .post(format!("{url}/v1/faucet"))
        .header("origin", "https://elsewhere.example")
        .header("content-type", "text/plain")
        .body(json!({ "holder": HolderKey::generate().public_key() }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], "unknown_origin", "{body}");
    // Its answer about the faucet is not for that page to read.
    let response = about_from(&url, "https://elsewhere.example").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, "access-control-allow-origin"), None);

    // None of those counted against the address or the day.
    assert_eq!(about(&url).await.left_today, 100);
    note(&url, None).await;
}

#[tokio::test]
async fn without_named_origins_the_faucet_sends_no_cors_headers() {
    let mint = Mint::start().await;
    let url = serve(mint.config(limits(100, 100))).await;

    let response = about_from(&url, PURSE).await;
    assert_eq!(response.status(), StatusCode::OK);
    for name in [
        "access-control-allow-origin",
        "access-control-expose-headers",
        "vary",
    ] {
        assert_eq!(header(&response, name), None, "{name}");
    }
    let response = preflight(&url, PURSE).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(header(&response, "access-control-allow-origin"), None);

    // A page elsewhere still gets no note.
    let response = ask_from(&url, PURSE, &[("sec-fetch-site", "cross-site")]).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], "unknown_origin", "{body}");
    // A page on the faucet's own origin, which needs no CORS, gets one, and
    // so does a client outside a browser, which sends no Origin.
    let response = ask_from(&url, &url, &[("sec-fetch-site", "same-origin")]).await;
    assert_eq!(response.status(), StatusCode::OK);
    note(&url, None).await;
    // Without Sec-Fetch-Site, which browsers send only to https and
    // loopback addresses, the faucet cannot tell its own page from another.
    let response = ask_from(&url, &url, &[]).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], "unknown_origin", "{body}");
}

#[tokio::test]
async fn a_body_a_page_can_send_without_a_preflight_is_refused_before_it_counts() {
    let mint = Mint::start().await;
    let url = serve(mint.config(limits(1, 100))).await;
    let http = reqwest::Client::new();
    let body = json!({ "holder": HolderKey::generate().public_key() }).to_string();
    // A page may send these types to another origin without a preflight,
    // and a body with no type at all.
    for content_type in [
        Some("text/plain"),
        Some("text/plain; charset=utf-8"),
        Some("application/x-www-form-urlencoded"),
        Some("multipart/form-data; boundary=x"),
        None,
    ] {
        let mut request = http.post(format!("{url}/v1/faucet")).body(body.clone());
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        let response = request.send().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{content_type:?}"
        );
        let answer: Value = response.json().await.unwrap();
        assert_eq!(answer["error"], "malformed", "{content_type:?}");
    }
    note(&url, None).await;
}
