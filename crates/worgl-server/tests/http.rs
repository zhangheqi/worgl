//! End-to-end tests of the HTTP API, in process, against a real store on disk.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use worgl::{
    BurnRequest, HolderKey, Issuer, IssuerKey, LedgerConfig, MintKey, MintRequest, Note, Output,
    PersistentIssuer, Policy, PublicKey, TransferRequest,
};
use worgl_server::{AppState, Info, RotationSchedule, router};

const TOKEN: &str = "test-admin-token-0123456789abcdef";

struct Server {
    app: Router,
    state: AppState,
    issuer: PublicKey,
    authority: MintKey,
    sequence: AtomicU64,
    _dir: tempfile::TempDir,
}

impl Server {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ledger = LedgerConfig {
            layers: 4,
            layer_size: 1024,
        };
        let authority = MintKey::generate();
        let policy = Policy {
            mint_authorities: vec![authority.public_key()],
            ..Policy::cash_like()
        };
        let issuer = Issuer::new(IssuerKey::generate(), ledger, policy).unwrap();
        let public = *issuer.public_key();
        let issuer = PersistentIssuer::create(dir.path(), issuer).unwrap();
        let state = AppState::new(issuer, TOKEN, 1000).unwrap();
        Self {
            app: router(state.clone()),
            state,
            issuer: public,
            authority,
            sequence: AtomicU64::new(0),
            _dir: dir,
        }
    }

    /// A mint request signed with the next sequence number.
    fn mint_request(&self, amount: u64, holder: &HolderKey) -> MintRequest {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        MintRequest::sign(
            &self.authority,
            &self.issuer,
            sequence,
            amount,
            holder.public_key(),
        )
    }

    async fn post_mint(&self, request: &MintRequest) -> (StatusCode, Value) {
        let body = serde_json::to_value(request).unwrap();
        self.call(Method::POST, "/v1/mint", None, body).await
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn mint(&self, amount: u64, holder: &HolderKey) -> Note {
        let (status, answer) = self.post_mint(&self.mint_request(amount, holder)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        serde_json::from_value(answer["note"].clone()).unwrap()
    }

    async fn transfer(&self, request: &TransferRequest) -> (StatusCode, Value) {
        let body = serde_json::to_value(request).unwrap();
        self.call(Method::POST, "/v1/transfer", None, body).await
    }

    async fn verify(&self, note: &Note) -> Value {
        let (status, answer) = self
            .call(Method::POST, "/v1/verify", None, json!({"note": note}))
            .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        answer
    }
}

fn pay(note: Note, key: &HolderKey) -> (TransferRequest, HolderKey) {
    let next = HolderKey::generate();
    let output = Output {
        amount: note.amount(),
        holder: next.public_key(),
    };
    (TransferRequest::single(note, key, output).unwrap(), next)
}

#[test]
fn short_admin_tokens_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = LedgerConfig {
        layers: 2,
        layer_size: 64,
    };
    let issuer = Issuer::new(IssuerKey::generate(), ledger, Policy::cash_like()).unwrap();
    let issuer = PersistentIssuer::create(dir.path(), issuer).unwrap();
    assert!(AppState::new(issuer, "", 1000).is_err());
}

#[tokio::test]
async fn admin_routes_need_the_token() {
    let server = Server::new();
    for token in [None, Some("wrong")] {
        let (status, answer) = server
            .call(Method::POST, "/v1/admin/rotate", token, Value::Null)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(answer["error"], "admin_unauthorized");
    }
    // The admin token cannot mint: there is no admin mint route.
    let body = json!({"amount": 8, "holder": HolderKey::generate().public_key()});
    let (status, _) = server
        .call(Method::POST, "/v1/admin/mint", Some(TOKEN), body)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn minting_needs_a_listed_authority_and_fresh_numbers() {
    let server = Server::new();
    let alice = HolderKey::generate();
    let request = server.mint_request(8, &alice);
    let (code, answer) = server.post_mint(&request).await;
    assert_eq!(code, StatusCode::OK, "{answer}");
    let note: Note = serde_json::from_value(answer["note"].clone()).unwrap();
    assert_eq!(note.holder(), &alice.public_key());
    assert_eq!(server.verify(&note).await["state"], "live");

    // A resent request gets the same note back; a copied one under a used
    // number mints nothing.
    let (code, again) = server.post_mint(&request).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(again, answer);
    let used = MintRequest::sign(
        &server.authority,
        &server.issuer,
        request.sequence,
        16,
        alice.public_key(),
    );
    let (code, answer) = server.post_mint(&used).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(answer["error"], "stale_mint");

    // Only listed keys can mint, and only with a good signature.
    let stranger = MintKey::generate();
    let foreign = MintRequest::sign(&stranger, &server.issuer, 1, 8, alice.public_key());
    let (code, answer) = server.post_mint(&foreign).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_eq!(answer["error"], "unknown_authority");
    let mut forged = server.mint_request(8, &alice);
    forged.amount = 1024;
    let (code, answer) = server.post_mint(&forged).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_eq!(answer["error"], "unauthorized");

    let (_, answer) = server
        .call(Method::GET, "/v1/info", None, Value::Null)
        .await;
    let info: Info = serde_json::from_value(answer).unwrap();
    assert_eq!(info.supply.minted, 8);
    assert_eq!(
        info.policy.mint_authorities,
        [server.authority.public_key()]
    );
}

#[tokio::test]
async fn a_note_moves_hand_to_hand_and_cannot_be_spent_twice() {
    let server = Server::new();
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

    let (code, report) = server
        .call(Method::POST, "/v1/admin/rotate", Some(TOKEN), Value::Null)
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(report["dropped"], 0);
    let (code, _) = server.transfer(&request).await;
    assert_eq!(code, StatusCode::OK);
}

#[tokio::test]
async fn bad_requests_get_stable_error_codes() {
    let server = Server::new();
    let alice = HolderKey::generate();
    let note = server.mint(8, &alice).await;

    let (code, answer) = server
        .call(Method::POST, "/v1/transfer", None, json!({"inputs": 1}))
        .await;
    assert_eq!(code.as_u16() / 100, 4);
    assert_eq!(answer["error"], "malformed");

    // 3 is not a power of two under the default policy.
    let (code, answer) = server.post_mint(&server.mint_request(3, &alice)).await;
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

    // A note from another issuer.
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
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(answer["error"], "invalid_note");
}

#[tokio::test]
async fn info_reports_layers_and_supply() {
    let server = Server::new();
    let alice = HolderKey::generate();
    server.mint(8, &alice).await;
    server.mint(4, &alice).await;
    let (code, answer) = server
        .call(Method::GET, "/v1/info", None, Value::Null)
        .await;
    assert_eq!(code, StatusCode::OK);
    let info: Info = serde_json::from_value(answer).unwrap();
    assert_eq!(info.sequence, 2);
    assert_eq!(info.mint_layer, 2);
    assert_eq!(info.layers[2].live, 2);
    assert_eq!(info.layers[2].value, 12);
    assert_eq!(info.supply.outstanding, 12);
}

#[tokio::test]
async fn burning_destroys_a_note_once_even_in_the_last_layer() {
    let server = Server::new();
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

    let (_, answer) = server
        .call(Method::GET, "/v1/info", None, Value::Null)
        .await;
    let info: Info = serde_json::from_value(answer).unwrap();
    assert_eq!(info.supply.minted, 8);
    assert_eq!(info.supply.burned, 8);
    assert_eq!(info.supply.outstanding, 0);
}

#[tokio::test]
async fn info_reports_the_rotation_schedule() {
    let server = Server::new();
    let (_, answer) = server
        .call(Method::GET, "/v1/info", None, Value::Null)
        .await;
    assert_eq!(answer["rotation"], Value::Null);

    let schedule = RotationSchedule {
        period_secs: 86_400,
        next_at: 1_792_281_600,
    };
    server.state.set_rotation_schedule(Some(schedule));
    let (_, answer) = server
        .call(Method::GET, "/v1/info", None, Value::Null)
        .await;
    assert_eq!(answer["rotation"]["period_secs"], 86_400);
    let info: Info = serde_json::from_value(answer).unwrap();
    assert_eq!(info.rotation, Some(schedule));
}
