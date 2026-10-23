//! End-to-end tests of the HTTP API, in process, against real stores on disk.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use worgl::{
    BurnRequest, HolderKey, Issuer, IssuerKey, KeySet, LedgerConfig, MintKey, MintRequest, Note,
    Output, Policy, PublicKey, TransferRequest,
};
use worgl_server::{
    AppState, CreatedMint, Info, MintSpec, create_mint, init_state, router, router_without_cors,
};

const LEDGER: LedgerConfig = LedgerConfig {
    layers: 4,
    layer_size: 1024,
};

/// The origin of the page that calls the server in the CORS tests.
const PAGE: &str = "https://purse.example";

/// Sends one request to the router and parses the JSON answer.
async fn call(
    app: Router,
    method: Method,
    path: String,
    token: Option<String>,
    body: Value,
) -> (StatusCode, Value) {
    let (status, _, answer) = call_from(app, None, method, &path, token.as_deref(), body).await;
    (status, answer)
}

/// Sends one request to the router, from a page on `origin` if one is
/// given, and returns the answer's status, headers and parsed JSON.
async fn call_from(
    app: Router,
    origin: Option<&str>,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(origin) = origin {
        request = request.header(header::ORIGIN, origin);
    }
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Sends the preflight a browser sends before a page on [`PAGE`] makes a
/// `method` request to `path` with the request headers `headers`, and
/// returns the answer's status and headers.
async fn preflight(
    app: &Router,
    method: &Method,
    path: &str,
    headers: &str,
) -> (StatusCode, HeaderMap) {
    let request = Request::builder()
        .method(Method::OPTIONS)
        .uri(path)
        .header(header::ORIGIN, PAGE)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, method.as_str())
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, headers)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    (response.status(), response.headers().clone())
}

/// Checks that a page on any origin can read an answer whole, its `Date`
/// and `Retry-After` included, and that the server allows no credentials.
fn assert_readable_from_any_page(headers: &HeaderMap) {
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert_eq!(
        headers[header::ACCESS_CONTROL_EXPOSE_HEADERS],
        "date,retry-after"
    );
    assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
}

/// Checks that an answer carries no CORS header, so a page on another
/// origin can read none of it.
fn assert_no_cors(headers: &HeaderMap) {
    assert!(
        headers
            .keys()
            .all(|name| !name.as_str().starts_with("access-control-")),
        "{headers:?}"
    );
}

/// A mint the tests created, with the keys to run it.
struct TestMint {
    key: PublicKey,
    token: String,
    authority: MintKey,
    sequence: AtomicU64,
}

impl TestMint {
    /// A mint request signed with the next sequence number.
    fn request(&self, amount: u64, holder: &HolderKey) -> MintRequest {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        MintRequest::sign(
            &self.authority,
            &self.key,
            sequence,
            amount,
            holder.public_key(),
        )
    }

    fn path(&self, route: &str) -> String {
        format!("/v1/mints/{}/{route}", self.key)
    }
}

/// A server with one mint to begin with.
struct Server {
    app: Router,
    operator: String,
    first: TestMint,
    dir: tempfile::TempDir,
}

impl std::ops::Deref for Server {
    type Target = TestMint;

    fn deref(&self) -> &TestMint {
        &self.first
    }
}

impl Server {
    async fn new() -> Self {
        Self::with_policy(Policy::cash_like(), None).await
    }

    async fn with_policy(policy: Policy, rotate_every: Option<u64>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        init_state(dir.path()).unwrap();
        let operator = read_token(&dir.path().join("operator.token"));
        let app = router(AppState::open(dir.path(), 1000).unwrap());
        let first = add_mint(&app, &operator, policy, rotate_every).await;
        Self {
            app,
            operator,
            first,
            dir,
        }
    }

    /// A server whose one mint was made offline and then changed on disk by
    /// `edit`, given the mint's directory, before the server opened it.
    async fn opened_after(rotate_every: Option<u64>, edit: impl FnOnce(&Path)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        init_state(dir.path()).unwrap();
        let operator = read_token(&dir.path().join("operator.token"));
        let authority = MintKey::generate();
        let mut policy = Policy::cash_like();
        policy.mint_authorities.push(authority.public_key());
        let spec = MintSpec {
            ledger: LEDGER,
            policy,
            rotate_every,
        };
        let created = create_mint(dir.path(), &spec).unwrap();
        edit(&dir.path().join("mints").join(created.mint.to_string()));
        let app = router(AppState::open(dir.path(), 1000).unwrap());
        Self {
            app,
            operator,
            first: TestMint {
                key: created.mint,
                token: created.token,
                authority,
                sequence: AtomicU64::new(0),
            },
            dir,
        }
    }

    /// Creates another mint through the operator API.
    async fn add_mint(&self, policy: Policy) -> TestMint {
        add_mint(&self.app, &self.operator, policy, None).await
    }

    /// Stops serving and opens the state directory again, as a restart would.
    async fn restart(self) -> Self {
        self.restart_with(router).await
    }

    /// Stops serving and opens the state directory again, serving it with
    /// `routes`.
    async fn restart_with(mut self, routes: fn(AppState) -> Router) -> Self {
        drop(std::mem::replace(&mut self.app, Router::new()));
        // Each committer lets go of its store once it sees its mint dropped.
        for _ in 0..200 {
            if let Ok(state) = AppState::open(self.dir.path(), 1000) {
                self.app = routes(state);
                return self;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the stores were never released");
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        call(
            self.app.clone(),
            method,
            path.to_owned(),
            token.map(str::to_owned),
            body,
        )
        .await
    }

    /// Sends one request from a page on [`PAGE`] and returns the answer's
    /// status, headers and parsed JSON.
    async fn call_from_page(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Value,
    ) -> (StatusCode, HeaderMap, Value) {
        call_from(self.app.clone(), Some(PAGE), method, path, token, body).await
    }

    async fn post_mint(&self, mint: &TestMint, request: &MintRequest) -> (StatusCode, Value) {
        let body = serde_json::to_value(request).unwrap();
        self.call(Method::POST, &mint.path("mint"), None, body)
            .await
    }

    async fn mint_in(&self, mint: &TestMint, amount: u64, holder: &HolderKey) -> Note {
        let (status, answer) = self.post_mint(mint, &mint.request(amount, holder)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        serde_json::from_value(answer["note"].clone()).unwrap()
    }

    async fn mint(&self, amount: u64, holder: &HolderKey) -> Note {
        self.mint_in(&self.first, amount, holder).await
    }

    async fn transfer(&self, request: &TransferRequest) -> (StatusCode, Value) {
        let body = serde_json::to_value(request).unwrap();
        self.call(Method::POST, "/v1/transfer", None, body).await
    }

    /// Sends a transfer and returns the answer's status, `Retry-After` and
    /// error code.
    async fn transfer_retry_after(
        &self,
        request: &TransferRequest,
    ) -> (StatusCode, Option<u64>, Value) {
        let body = serde_json::to_value(request).unwrap().to_string();
        let request = Request::builder()
            .method(Method::POST)
            .uri("/v1/transfer")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .map(|value| value.to_str().unwrap().parse().unwrap());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let answer: Value = serde_json::from_slice(&bytes).unwrap();
        (status, retry_after, answer["error"].clone())
    }

    async fn verify(&self, note: &Note) -> Value {
        let (status, answer) = self
            .call(Method::POST, "/v1/verify", None, json!({"note": note}))
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        answer
    }

    async fn info_of(&self, mint: &TestMint) -> Info {
        let (status, answer) = self
            .call(Method::GET, &mint.path("info"), None, Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        serde_json::from_value(answer).unwrap()
    }

    async fn info(&self) -> Info {
        self.info_of(&self.first).await
    }

    /// Calls one of a mint's admin routes with its own token.
    async fn admin(&self, mint: &TestMint, route: &str) -> (StatusCode, Value) {
        self.call(
            Method::POST,
            &mint.path(route),
            Some(&mint.token),
            Value::Null,
        )
        .await
    }
}

async fn add_mint(
    app: &Router,
    operator: &str,
    policy: Policy,
    rotate_every: Option<u64>,
) -> TestMint {
    let authority = MintKey::generate();
    let mut policy = policy;
    policy.mint_authorities.push(authority.public_key());
    let spec = MintSpec {
        ledger: LEDGER,
        policy,
        rotate_every,
    };
    let (status, answer) = call(
        app.clone(),
        Method::POST,
        "/v1/admin/mints".into(),
        Some(operator.into()),
        serde_json::to_value(spec).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let created: CreatedMint = serde_json::from_value(answer).unwrap();
    TestMint {
        key: created.mint,
        token: created.token,
        authority,
        sequence: AtomicU64::new(0),
    }
}

fn read_token(path: &Path) -> String {
    fs::read_to_string(path).unwrap().trim().to_owned()
}

fn pay(note: Note, key: &HolderKey) -> (TransferRequest, HolderKey) {
    let next = HolderKey::generate();
    let output = Output {
        amount: note.amount(),
        holder: next.public_key(),
    };
    (TransferRequest::single(note, key, output).unwrap(), next)
}

#[tokio::test]
async fn each_token_opens_only_its_own_routes() {
    let server = Server::new().await;
    let other = server.add_mint(Policy::cash_like()).await;
    for route in ["rotate", "pause", "resume"] {
        let path = server.path(route);
        for token in [
            None,
            Some("wrong"),
            Some(&*other.token),
            Some(&*server.operator),
        ] {
            let (status, answer) = server.call(Method::POST, &path, token, Value::Null).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{route} {token:?}");
            assert_eq!(answer["error"], "admin_unauthorized");
        }
    }
    for token in [None, Some(&*server.token)] {
        let (status, _) = server
            .call(Method::GET, "/v1/admin/mints", token, Value::Null)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let spec = json!({"ledger": LEDGER, "policy": {"mint_authorities": [other.authority.public_key()]}});
        let (status, _) = server
            .call(Method::POST, "/v1/admin/mints", token, spec)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, answer) = server
        .call(
            Method::GET,
            "/v1/admin/mints",
            Some(&server.operator),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut expected = vec![server.key, other.key];
    expected.sort_unstable();
    assert_eq!(answer["mints"], json!(expected));

    // No token can mint: there is no admin mint route.
    let body = json!({"amount": 8, "holder": HolderKey::generate().public_key()});
    for path in ["/v1/admin/mint", "/v1/mint"] {
        let (status, _) = server
            .call(Method::POST, path, Some(&server.operator), body.clone())
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn the_routes_without_a_token_answer_pages_on_any_origin() {
    // One receipt, kept an hour: longer than the test takes.
    let server = Server::with_policy(
        Policy {
            receipt_capacity: 1,
            receipt_min_age_secs: 3600,
            ..Policy::cash_like()
        },
        None,
    )
    .await;
    let routes = [
        (Method::GET, "/healthz".to_owned()),
        (Method::GET, "/readyz".to_owned()),
        (Method::GET, server.path("info")),
        (Method::GET, server.path("keys")),
        (Method::POST, "/v1/verify".to_owned()),
        (Method::POST, "/v1/transfer".to_owned()),
        (Method::POST, "/v1/burn".to_owned()),
        (Method::POST, server.path("mint")),
    ];
    for (method, path) in &routes {
        let (status, headers) = preflight(&server.app, method, path, "content-type").await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert_eq!(
            headers[header::ACCESS_CONTROL_ALLOW_METHODS],
            method.as_str()
        );
        assert_eq!(
            headers[header::ACCESS_CONTROL_ALLOW_HEADERS],
            "content-type"
        );
        assert_eq!(headers[header::ACCESS_CONTROL_MAX_AGE], "86400");
        assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
    }

    let (status, headers, info) = server
        .call_from_page(Method::GET, &server.path("info"), None, Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_readable_from_any_page(&headers);
    let alice = HolderKey::generate();
    let first = server.mint(8, &alice).await;
    let second = server.mint(8, &alice).await;
    let body = serde_json::to_value(pay(first, &alice).0).unwrap();
    let (status, headers, answer) = server
        .call_from_page(Method::POST, "/v1/transfer", None, body)
        .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_readable_from_any_page(&headers);

    // Refusals too, so a page can read their codes and when to come back.
    let body = serde_json::to_value(pay(second, &alice).0).unwrap();
    let (status, headers, answer) = server
        .call_from_page(Method::POST, "/v1/transfer", None, body)
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answer["error"], "busy");
    assert!(headers.contains_key(header::RETRY_AFTER));
    assert_readable_from_any_page(&headers);
    let (status, headers, answer) = server
        .call_from_page(Method::POST, "/v1/transfer", None, json!({"inputs": 1}))
        .await;
    assert_eq!(status.as_u16() / 100, 4);
    assert_eq!(answer["error"], "malformed");
    assert_readable_from_any_page(&headers);
    let (status, headers, answer) = server
        .call_from_page(Method::GET, "/v1/mints/zz/info", None, Value::Null)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(answer["error"], "unknown_mint");
    assert_readable_from_any_page(&headers);
}

#[tokio::test]
async fn paths_no_route_serves_answer_pages_on_any_origin() {
    let server = Server::new().await;
    // A page that calls the wrong server, or a route this one lacks, reads
    // the 404 rather than a network error.
    for path in [
        "/v1/nope".to_owned(),
        server.path("nope"),
        "/v2/info".to_owned(),
    ] {
        for method in [Method::GET, Method::POST] {
            let (status, headers) = preflight(&server.app, &method, &path, "content-type").await;
            assert_eq!(status, StatusCode::OK, "{method} {path}");
            assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
            assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_METHODS], "GET,POST");
            let (status, headers, _) = server
                .call_from_page(method.clone(), &path, None, Value::Null)
                .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
            assert_readable_from_any_page(&headers);
        }
    }
}

#[tokio::test]
async fn the_routes_with_a_token_answer_no_page_on_another_origin() {
    let server = Server::new().await;
    let routes = [
        (Method::POST, server.path("rotate")),
        (Method::POST, server.path("pause")),
        (Method::POST, server.path("resume")),
        (Method::GET, "/v1/admin/mints".to_owned()),
        (Method::POST, "/v1/admin/mints".to_owned()),
    ];
    // A browser never sends the request, so a page cannot use a token.
    for (method, path) in &routes {
        let headers = "authorization,content-type";
        let (status, headers) = preflight(&server.app, method, path, headers).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
        assert_no_cors(&headers);
    }
    // Nor could it read an answer, right token or wrong.
    for (token, expected) in [
        (&*server.token, StatusCode::OK),
        ("wrong", StatusCode::UNAUTHORIZED),
    ] {
        let (status, headers, _) = server
            .call_from_page(
                Method::POST,
                &server.path("resume"),
                Some(token),
                Value::Null,
            )
            .await;
        assert_eq!(status, expected);
        assert_no_cors(&headers);
    }
    let (status, headers, _) = server
        .call_from_page(
            Method::GET,
            "/v1/admin/mints",
            Some(&server.operator),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_no_cors(&headers);
}

#[tokio::test]
async fn without_cors_no_route_answers_pages_on_other_origins() {
    let server = Server::new().await.restart_with(router_without_cors).await;
    for (method, path) in [
        (Method::GET, server.path("info")),
        (Method::POST, "/v1/transfer".to_owned()),
    ] {
        let (status, headers) = preflight(&server.app, &method, &path, "content-type").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{path}");
        assert_no_cors(&headers);
    }
    let (status, headers) = preflight(&server.app, &Method::GET, "/v1/nope", "content-type").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_no_cors(&headers);
    let (status, headers, info) = server
        .call_from_page(Method::GET, &server.path("info"), None, Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_no_cors(&headers);
}

#[tokio::test]
async fn minting_needs_a_listed_authority_and_fresh_numbers() {
    let server = Server::new().await;
    let alice = HolderKey::generate();
    let request = server.request(8, &alice);
    let (code, answer) = server.post_mint(&server.first, &request).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    let note: Note = serde_json::from_value(answer["note"].clone()).unwrap();
    assert_eq!(note.holder(), &alice.public_key());
    assert_eq!(note.issuer(), &server.key);
    assert_eq!(server.verify(&note).await["state"], "live");

    // A resent request gets the same note back; a copied one under a used
    // number mints nothing.
    let (code, again) = server.post_mint(&server.first, &request).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(again, answer);
    let used = MintRequest::sign(
        &server.authority,
        &server.key,
        request.sequence,
        16,
        alice.public_key(),
    );
    let (code, answer) = server.post_mint(&server.first, &used).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(answer["error"], "stale_mint");

    // Only listed keys can mint, and only with a good signature.
    let stranger = MintKey::generate();
    let foreign = MintRequest::sign(&stranger, &server.key, 1, 8, alice.public_key());
    let (code, answer) = server.post_mint(&server.first, &foreign).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_eq!(answer["error"], "unknown_authority");
    let mut forged = server.request(8, &alice);
    forged.amount = 1024;
    let (code, answer) = server.post_mint(&server.first, &forged).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_eq!(answer["error"], "unauthorized");

    // A request signed for one mint does not mint at another, even one that
    // lists the same authority.
    let twin = server
        .add_mint(Policy {
            mint_authorities: vec![server.authority.public_key()],
            ..Policy::cash_like()
        })
        .await;
    let (code, answer) = server.post_mint(&twin, &server.request(8, &alice)).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_eq!(answer["error"], "unauthorized");

    let info = server.info().await;
    assert_eq!(info.supply.minted, 8);
    assert_eq!(
        info.policy.mint_authorities,
        [server.authority.public_key()]
    );
    let twin = server.info_of(&twin).await;
    assert_eq!(twin.supply.minted, 0);
    assert_eq!(
        twin.policy.mint_authorities[0],
        server.authority.public_key()
    );
}

#[tokio::test]
async fn a_note_moves_hand_to_hand_and_cannot_be_spent_twice() {
    let server = Server::new().await;
    let alice = HolderKey::generate();
    let note = server.mint(8, &alice).await;
    let status = server.verify(&note).await;
    assert_eq!(status["state"], "live");
    assert_eq!(status["position"], 2);
    assert_eq!(status["hops_left"], 1);

    let (request, bob) = pay(note, &alice);
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    let bobs: Note = serde_json::from_value(answer["notes"][0].clone()).unwrap();
    assert_eq!(server.verify(&note).await["state"], "spent");
    assert_eq!(server.verify(&bobs).await["position"], 3);

    // The same request again returns the same notes instead of failing.
    let (code, again) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(again, answer);

    // A different request spending Alice's note is a double spend.
    let (other, _) = pay(note, &alice);
    let (code, answer) = server.transfer(&other).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(answer["error"], "spent");

    // Bob's note is in the last layer until the next rotation.
    let (request, _) = pay(bobs, &bob);
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(answer["error"], "frozen");

    let (code, report) = server.admin(&server.first, "rotate").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(report["dropped"], 0);
    let (code, _) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::OK);
}

#[tokio::test]
async fn a_mint_full_of_young_receipts_answers_busy_with_retry_after() {
    // Two receipts, each kept an hour: longer than the test takes.
    let server = Server::with_policy(
        Policy {
            receipt_capacity: 2,
            receipt_min_age_secs: 3600,
            ..Policy::cash_like()
        },
        None,
    )
    .await;
    let alice = HolderKey::generate();
    let mut requests = Vec::new();
    for _ in 0..3 {
        requests.push(pay(server.mint(8, &alice).await, &alice).0);
    }
    for request in &requests[..2] {
        let (code, answer) = server.transfer(request).await;
        assert_eq!(code, StatusCode::OK, "{answer}");
    }

    let busy = |(code, retry_after, error): (StatusCode, Option<u64>, Value)| {
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error, "busy");
        let retry_after = retry_after.expect("a busy answer says when to come back");
        assert!((3500..=3600).contains(&retry_after), "{retry_after}");
    };
    busy(server.transfer_retry_after(&requests[2]).await);
    let (code, retry_after, _) = server.transfer_retry_after(&requests[0]).await;
    assert_eq!((code, retry_after), (StatusCode::OK, None));

    // A restart counts the receipts the log made as made no earlier than
    // its last write, so they are still too young to push out.
    let server = server.restart().await;
    busy(server.transfer_retry_after(&requests[2]).await);
    let (code, answer) = server.transfer(&requests[1]).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
}

#[tokio::test]
async fn bad_requests_get_stable_error_codes() {
    let server = Server::new().await;
    let alice = HolderKey::generate();
    let note = server.mint(8, &alice).await;

    let (code, answer) = server
        .call(Method::POST, "/v1/transfer", None, json!({"inputs": 1}))
        .await;
    assert_eq!(code.as_u16() / 100, 4);
    assert_eq!(answer["error"], "malformed");
    let (code, answer) = server
        .call(
            Method::POST,
            "/v1/transfer",
            None,
            json!({"inputs": [], "outputs": []}),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(answer["error"], "malformed");

    // 3 is not a power of two under the default policy.
    let (code, answer) = server
        .post_mint(&server.first, &server.request(3, &alice))
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(answer["error"], "policy");

    // A thief who copied the note cannot sign for it.
    let thief = HolderKey::generate();
    let (mut request, _) = pay(note, &alice);
    request.inputs[0].signature = TransferRequest::single(
        note,
        &alice,
        Output {
            amount: 8,
            holder: thief.public_key(),
        },
    )
    .unwrap()
    .inputs[0]
        .signature;
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_eq!(answer["error"], "unauthorized");

    // A note from an issuer this server does not host.
    let mut foreign = Issuer::new(
        IssuerKey::generate(),
        LedgerConfig {
            layers: 2,
            layer_size: 64,
        },
        Policy::cash_like(),
    )
    .unwrap();
    let fake = foreign.mint(8, alice.public_key()).unwrap();
    let (code, answer) = server
        .call(Method::POST, "/v1/verify", None, json!({"note": fake}))
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(answer["error"], "unknown_mint");
    let (request, _) = pay(fake, &alice);
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(answer["error"], "unknown_mint");
    for path in [
        format!("/v1/mints/{}/info", foreign.public_key()),
        "/v1/mints/zz/info".to_owned(),
    ] {
        let (code, answer) = server.call(Method::GET, &path, None, Value::Null).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert_eq!(answer["error"], "unknown_mint");
    }
}

#[tokio::test]
async fn info_reports_layers_and_supply() {
    let server = Server::new().await;
    let alice = HolderKey::generate();
    server.mint(8, &alice).await;
    server.mint(4, &alice).await;
    let info = server.info().await;
    assert_eq!(info.issuer, server.key);
    assert_eq!(info.sequence, 2);
    assert_eq!(info.mint_layer, 2);
    assert_eq!(info.layers[2].live, 2);
    assert_eq!(info.layers[2].value, 12);
    assert_eq!(info.supply.outstanding, 12);
    assert_eq!(info.rotation, None);
    assert!(!info.paused);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_all_land_and_never_double_spend() {
    let server = Server::new().await;
    let keys: Vec<HolderKey> = (0..64).map(|_| HolderKey::generate()).collect();
    // An authority sends one mint at a time, so the notes are minted in turn.
    let mut notes = Vec::new();
    for key in &keys {
        notes.push(server.mint(8, key).await);
    }

    // Each note is paid to two different keys at once: one payment wins.
    let mut races = Vec::new();
    for (note, key) in notes.iter().zip(&keys) {
        for _ in 0..2 {
            let (request, _) = pay(*note, key);
            let body = serde_json::to_value(&request).unwrap();
            let app = server.app.clone();
            races.push(tokio::spawn(call(
                app,
                Method::POST,
                "/v1/transfer".into(),
                None,
                body,
            )));
        }
    }
    let mut won = Vec::new();
    for race in races {
        let (code, answer) = race.await.unwrap();
        match code {
            StatusCode::OK => {
                won.push(serde_json::from_value::<Note>(answer["notes"][0].clone()).unwrap())
            }
            StatusCode::CONFLICT => assert_eq!(answer["error"], "spent"),
            other => panic!("unexpected {other}: {answer}"),
        }
    }
    assert_eq!(won.len(), 64);
    // The winners were committed in batches, each still on its own bit.
    let mut serials: Vec<_> = won.iter().map(Note::serial).collect();
    serials.sort_unstable();
    serials.dedup();
    assert_eq!(serials.len(), 64);

    let info = server.info().await;
    assert_eq!(info.sequence, 128);
    assert_eq!(info.supply.outstanding, 64 * 8);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mints_keep_their_own_ledgers_and_notes_find_their_mint() {
    let server = Server::with_policy(Policy::flexible(), None).await;
    let other = server.add_mint(Policy::flexible()).await;
    let alice = HolderKey::generate();
    let ours = server.mint(8, &alice).await;
    let theirs = server.mint_in(&other, 8, &alice).await;
    assert_eq!(ours.issuer(), &server.key);
    assert_eq!(theirs.issuer(), &other.key);

    // Holder routes find the mint from the note alone.
    let (request, bob) = pay(theirs, &alice);
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    let bobs: Note = serde_json::from_value(answer["notes"][0].clone()).unwrap();
    assert_eq!(bobs.issuer(), &other.key);
    assert_eq!(server.verify(&theirs).await["state"], "spent");
    assert_eq!(server.verify(&ours).await["state"], "live");
    let burn = serde_json::to_value(BurnRequest::single(bobs, &bob).unwrap()).unwrap();
    let (code, answer) = server.call(Method::POST, "/v1/burn", None, burn).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    assert_eq!(server.info_of(&other).await.supply.burned, 8);
    assert_eq!(server.info().await.supply.burned, 0);

    // One transfer cannot spend notes of two mints.
    let theirs = server.mint_in(&other, 8, &alice).await;
    let request = TransferRequest::sign(
        &[(ours, &alice), (theirs, &alice)],
        vec![Output {
            amount: 16,
            holder: bob.public_key(),
        }],
    )
    .unwrap();
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["error"], "invalid_note");

    // Rotating one mint leaves the other alone.
    let (code, _) = server.admin(&other, "rotate").await;
    assert_eq!(code, StatusCode::OK);
    let (_, keys) = server
        .call(Method::GET, &other.path("keys"), None, Value::Null)
        .await;
    let keys: KeySet = serde_json::from_value(keys).unwrap();
    assert_eq!(keys.issuer(), &other.key);
    assert_eq!(keys.oldest(), 1);
    assert_eq!(server.info().await.sequence, 1);
}

#[tokio::test]
async fn a_paused_mint_moves_nothing_until_resumed_even_after_a_restart() {
    let server = Server::new().await;
    let other = server.add_mint(Policy::cash_like()).await;
    let alice = HolderKey::generate();
    let note = server.mint(8, &alice).await;
    let (code, answer) = server.admin(&server.first, "pause").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(answer["paused"], true);
    assert!(server.info().await.paused);

    // Nothing changes on a paused mint, but its notes still verify.
    let (request, _) = pay(note, &alice);
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answer["error"], "paused");
    let burn = serde_json::to_value(BurnRequest::single(note, &alice).unwrap()).unwrap();
    let (code, answer) = server.call(Method::POST, "/v1/burn", None, burn).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answer["error"], "paused");
    let refused = server.request(8, &alice);
    let (code, answer) = server.post_mint(&server.first, &refused).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answer["error"], "paused");
    let (code, answer) = server.admin(&server.first, "rotate").await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answer["error"], "paused");
    assert_eq!(server.verify(&note).await["state"], "live");

    // Other mints carry on.
    server.mint_in(&other, 8, &alice).await;
    assert!(!server.info_of(&other).await.paused);

    // The pause outlives a restart, and lifts only when asked.
    let server = server.restart().await;
    assert!(server.info().await.paused);
    let (code, _) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    let (code, answer) = server.admin(&server.first, "resume").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(answer["paused"], false);
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    // The mint refused while paused was never recorded, so it goes through now.
    let (code, answer) = server.post_mint(&server.first, &refused).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    let info = server.info().await;
    assert!(!info.paused);
    // Two mints and a transfer, with the pause and resume logged between.
    assert_eq!(info.sequence, 5);
    // Resuming a running mint changes nothing.
    let (code, answer) = server.admin(&server.first, "resume").await;
    assert_eq!((code, &answer["paused"]), (StatusCode::OK, &json!(false)));
}

#[tokio::test]
async fn a_pause_an_older_server_kept_in_mint_json_is_logged() {
    let server = Server::opened_after(None, |dir| {
        fs::write(dir.join("mint.json"), r#"{"paused": true}"#).unwrap();
    })
    .await;
    let info = server.info().await;
    assert!(info.paused);
    assert_eq!(info.sequence, 1, "the pause is an event");
    let dir = server.dir.path().join("mints").join(server.key.to_string());
    let settings: Value =
        serde_json::from_slice(&fs::read(dir.join("mint.json")).unwrap()).unwrap();
    assert_eq!(settings, json!({"rotate_every": null}));

    // Resumed, it stays resumed across a restart.
    let (code, _) = server.admin(&server.first, "resume").await;
    assert_eq!(code, StatusCode::OK);
    let server = server.restart().await;
    assert!(!server.info().await.paused);
    server.mint(8, &HolderKey::generate()).await;
}

#[tokio::test]
async fn an_older_servers_last_rotation_file_is_a_floor_for_the_schedule() {
    let period = 86_400;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let floor = (now / period + 4) * period;
    let server = Server::opened_after(Some(period), |dir| {
        fs::write(dir.join("last_rotation"), format!("{floor}\n")).unwrap();
    })
    .await;
    assert_eq!(
        server.info().await.rotation.unwrap().next_at,
        floor + period
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_logged_scheduled_rotation_retires_the_last_rotation_file() {
    let mut dir = None;
    let server = Server::opened_after(Some(1), |mint| {
        fs::write(mint.join("last_rotation"), "1\n").unwrap();
        dir = Some(mint.to_owned());
    })
    .await;
    let legacy = dir.unwrap().join("last_rotation");
    for _ in 0..100 {
        if server.info().await.sequence > 0 && !legacy.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no scheduled rotation removed {}", legacy.display());
}

#[tokio::test]
async fn the_state_directory_keeps_a_private_kek() {
    let server = Server::new().await;
    let path = server.dir.path().join("kek.key");
    let kek = fs::read_to_string(&path).unwrap();
    assert_eq!(kek.trim().len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Rotations seal their keys with it, and the mint opens again after them.
    let (code, _) = server.admin(&server.first, "rotate").await;
    assert_eq!(code, StatusCode::OK);
    let server = server.restart().await;
    assert_eq!(fs::read_to_string(&path).unwrap(), kek);
    server.mint(8, &HolderKey::generate()).await;

    // Without it the state directory does not open, and no new key is made.
    fs::remove_file(&path).unwrap();
    let error = AppState::open(server.dir.path(), 1000).err().unwrap();
    assert!(error.to_string().contains("kek.key is missing"), "{error}");
    assert!(!path.exists());
}

#[tokio::test]
async fn export_log_prints_each_entry_as_json() {
    let server = Server::new().await;
    let alice = HolderKey::generate();
    server.mint(8, &alice).await;
    server.mint(4, &alice).await;
    let dir = server.dir.path().join("mints").join(server.key.to_string());
    let export = |dir: &Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_worgl-server"))
            .args(["export-log", "--mint"])
            .arg(dir)
            .output()
            .unwrap()
    };
    let output = export(&dir);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        (&lines[1]["index"], &lines[1]["term"], &lines[1]["first"]),
        (&json!(1), &json!(0), &json!(2))
    );
    assert_eq!(lines[1]["events"].as_array().unwrap().len(), 1);
    assert!(!export(server.dir.path()).status.success());
}

#[tokio::test]
async fn info_reports_the_rotation_schedule() {
    let server = Server::with_policy(Policy::cash_like(), Some(86_400)).await;
    let schedule = server.info().await.rotation.unwrap();
    assert_eq!(schedule.period_secs, 86_400);
    assert_eq!(schedule.next_at % 86_400, 0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now < schedule.next_at && schedule.next_at <= now + 86_400);
    let other = server.add_mint(Policy::cash_like()).await;
    assert_eq!(server.info_of(&other).await.rotation, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn info_answers_its_layers_and_schedule_from_one_state() {
    let server = Server::with_policy(Policy::cash_like(), Some(1)).await;
    let first = server.info().await.layers[0].generation;
    // Polled from all sides while the schedule rotates twice.
    let mut pollers = Vec::new();
    for _ in 0..8 {
        let (app, path) = (server.app.clone(), server.path("info"));
        pollers.push(tokio::spawn(async move {
            let mut seen = BTreeSet::new();
            loop {
                let (code, answer) =
                    call(app.clone(), Method::GET, path.clone(), None, Value::Null).await;
                assert_eq!(code, StatusCode::OK, "{answer}");
                let info: Info = serde_json::from_value(answer).unwrap();
                let oldest = info.layers[0].generation;
                seen.insert((oldest, info.rotation.unwrap().next_at));
                if oldest >= first + 2 {
                    return seen;
                }
            }
        }));
    }
    let mut seen = BTreeSet::new();
    for poller in pollers {
        let answers = tokio::time::timeout(Duration::from_secs(10), poller)
            .await
            .expect("the schedule never rotated twice");
        seen.extend(answers.unwrap());
    }
    // A rotation moves the first layer and the next rotation together, so no
    // first layer comes with two schedules, nor one schedule with two.
    let pairs: Vec<_> = seen.into_iter().collect();
    for pair in pairs.windows(2) {
        let [(oldest, next_at), (later, then)] = pair else {
            unreachable!()
        };
        assert!(
            oldest < later && next_at < then,
            "answers mix layers and schedules: {pairs:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_mint_skips_its_scheduled_rotations() {
    let server = Server::with_policy(Policy::cash_like(), Some(1)).await;
    let rotated_past = |sequence: u64| {
        let server = &server;
        async move {
            for _ in 0..100 {
                if server.info().await.sequence > sequence {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            false
        }
    };
    assert!(rotated_past(0).await, "the schedule never rotated");

    let (code, _) = server.admin(&server.first, "pause").await;
    assert_eq!(code, StatusCode::OK);
    let paused = server.info().await;
    tokio::time::sleep(Duration::from_millis(3000)).await;
    let later = server.info().await;
    assert_eq!(later.sequence, paused.sequence);
    // The boundaries passed while paused are skipped, not owed.
    assert!(later.rotation.unwrap().next_at > paused.rotation.unwrap().next_at);

    let (code, _) = server.admin(&server.first, "resume").await;
    assert_eq!(code, StatusCode::OK);
    assert!(
        rotated_past(later.sequence).await,
        "no rotation after resuming"
    );
}

#[tokio::test]
async fn burning_destroys_a_note_once_even_in_the_last_layer() {
    let server = Server::new().await;
    let alice = HolderKey::generate();
    let note = server.mint(8, &alice).await;
    let (request, bob) = pay(note, &alice);
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    let bobs: Note = serde_json::from_value(answer["notes"][0].clone()).unwrap();

    // Bob's note is frozen in the last layer, but it can still be burned.
    let burn = serde_json::to_value(BurnRequest::single(bobs, &bob).unwrap()).unwrap();
    let (code, answer) = server
        .call(Method::POST, "/v1/burn", None, burn.clone())
        .await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    assert_eq!(answer["value"], 8);
    assert_eq!(server.verify(&bobs).await["state"], "spent");

    // Sending the same burn again destroys nothing more.
    let (code, again) = server.call(Method::POST, "/v1/burn", None, burn).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(again, answer);

    // Only the holder can burn, and a spent note cannot be burned.
    let mut forged = BurnRequest::single(bobs, &bob).unwrap();
    forged.inputs[0].signature = BurnRequest::single(note, &alice).unwrap().inputs[0].signature;
    let forged = serde_json::to_value(forged).unwrap();
    let (code, answer) = server.call(Method::POST, "/v1/burn", None, forged).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{answer}");
    assert_eq!(answer["error"], "unauthorized");
    let burn = serde_json::to_value(BurnRequest::single(note, &alice).unwrap()).unwrap();
    let (code, answer) = server.call(Method::POST, "/v1/burn", None, burn).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(answer["error"], "spent");

    let info = server.info().await;
    assert_eq!(info.supply.minted, 8);
    assert_eq!(info.supply.burned, 8);
    assert_eq!(info.supply.outstanding, 0);
}

#[tokio::test]
async fn each_rotation_publishes_a_new_generation_key_that_verifies_at_once() {
    let server = Server::new().await;
    let (code, answer) = server
        .call(Method::GET, &server.path("keys"), None, Value::Null)
        .await;
    assert_eq!(code, StatusCode::OK);
    let keys: KeySet = serde_json::from_value(answer).unwrap();
    assert_eq!(keys.issuer(), &server.key);
    assert_eq!(keys.certificates().len(), 4);

    let alice = HolderKey::generate();
    let note = server.mint(8, &alice).await;
    assert_eq!(note.verify(&keys), Ok(()));
    let (code, report) = server.admin(&server.first, "rotate").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(report["key"]["generation"], 4);

    // The note moves twice, into the generation the rotation created.
    let (request, bob) = pay(note, &alice);
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    let note: Note = serde_json::from_value(answer["notes"][0].clone()).unwrap();
    let (request, carol) = pay(note, &bob);
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    let note: Note = serde_json::from_value(answer["notes"][0].clone()).unwrap();
    assert_eq!(note.serial().generation, 4);

    // Its key is published, and the server verifies it straight away.
    let (_, answer) = server
        .call(Method::GET, &server.path("keys"), None, Value::Null)
        .await;
    let keys: KeySet = serde_json::from_value(answer).unwrap();
    assert_eq!(keys.oldest(), 1);
    assert_eq!(keys.certificates().last().unwrap().generation, 4);
    assert_eq!(note.verify(&keys), Ok(()));
    assert_eq!(server.verify(&note).await["state"], "live");
    let burn = serde_json::to_value(BurnRequest::single(note, &carol).unwrap()).unwrap();
    let (code, answer) = server.call(Method::POST, "/v1/burn", None, burn).await;
    assert_eq!(code, StatusCode::OK, "{answer}");

    // A note whose generation was dropped is reported expired, not forged.
    let dave = HolderKey::generate();
    let old = server.mint(4, &dave).await;
    for _ in 0..3 {
        let (code, _) = server.admin(&server.first, "rotate").await;
        assert_eq!(code, StatusCode::OK);
    }
    assert_eq!(server.verify(&old).await["state"], "expired");
    let (request, _) = pay(old, &dave);
    let (code, answer) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(answer["error"], "expired");
}

#[tokio::test]
async fn a_mint_spec_is_checked_before_anything_is_written() {
    let server = Server::new().await;
    let authority = MintKey::generate().public_key();
    let refused = [
        json!({"ledger": LEDGER}),
        json!({"ledger": LEDGER, "policy": {"mint_authorities": []}}),
        json!({"ledger": {"layers": 4, "layer_size": 100}, "policy": {"mint_authorities": [authority]}}),
        json!({"ledger": {"layers": 1, "layer_size": 64}, "policy": {"mint_authorities": [authority]}}),
        json!({"ledger": LEDGER, "policy": {"mint_authorities": [authority], "mint_layer": 4}}),
        json!({"ledger": LEDGER, "policy": {"mint_authorities": [authority]}, "rotate_every": 0}),
        json!({"policy": {"mint_authorities": [authority]}}),
    ];
    for spec in refused {
        let (code, answer) = server
            .call(
                Method::POST,
                "/v1/admin/mints",
                Some(&server.operator),
                spec.clone(),
            )
            .await;
        assert_eq!(code.as_u16() / 100, 4, "{spec}: {answer}");
        assert_eq!(answer["error"], "malformed", "{spec}");
    }
    let mints = fs::read_dir(server.dir.path().join("mints"))
        .unwrap()
        .count();
    assert_eq!(mints, 1);
}

#[tokio::test]
async fn opening_checks_tokens_and_clears_half_made_mints() {
    let dir = tempfile::tempdir().unwrap();
    init_state(dir.path()).unwrap();
    assert!(init_state(dir.path()).is_err());

    // A mint made offline is served once the state is opened.
    let spec = MintSpec {
        ledger: LEDGER,
        policy: Policy::cash_like(),
        rotate_every: None,
    };
    let created = create_mint(dir.path(), &spec).unwrap();
    let half_made = dir.path().join("mints").join(".new-0123");
    fs::create_dir(&half_made).unwrap();
    fs::write(half_made.join("issuer.key"), "00").unwrap();
    let state = AppState::open(dir.path(), 1000).unwrap();
    assert_eq!(state.mints(), [created.mint]);
    assert!(!half_made.exists());
    drop(state);

    // Tokens too short to be secret are refused.
    let token = dir.path().join("operator.token");
    let saved = fs::read_to_string(&token).unwrap();
    fs::write(&token, "short\n").unwrap();
    let error = AppState::open(dir.path(), 1000).err().unwrap();
    assert!(error.to_string().contains("operator.token"), "{error:#}");
    fs::write(&token, saved).unwrap();

    // A directory from before mints is pointed at its new home.
    let legacy = tempfile::tempdir().unwrap();
    fs::write(legacy.path().join("issuer.key"), "00").unwrap();
    let error = AppState::open(legacy.path(), 1000).err().unwrap();
    assert!(error.to_string().contains("mints/"), "{error:#}");
}
