//! A mint whose disk fills up, as a limit on the size of the files the
//! process writes makes it look. The limit holds for the whole process, so
//! this file has one test, and no other test runs beside it.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use rustix::process::{Resource, Rlimit, Signal, getrlimit, setrlimit};
use serde_json::{Value, json};
use tokio::signal::unix::{SignalKind, signal};
use tower::ServiceExt;
use worgl::{HolderKey, LedgerConfig, MintKey, MintRequest, Note, Output, Policy, TransferRequest};
use worgl_server::{AppState, MintSpec, create_mint, init_state, router};

const LEDGER: LedgerConfig = LedgerConfig {
    layers: 4,
    layer_size: 1024,
};

/// Sends one request to the router and parses the JSON answer.
async fn call(
    app: &Router,
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
    let response = app
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

/// Lets no file grow past `bytes`, as a full disk would, or with `None`
/// lifts the limit.
fn limit_files(bytes: Option<u64>) {
    let maximum = getrlimit(Resource::Fsize).maximum;
    let current = bytes.or(maximum);
    setrlimit(Resource::Fsize, Rlimit { current, maximum }).unwrap();
}

/// Opens the server state in `dir` again, as a restart would.
async fn reopen(dir: &Path) -> Router {
    // The committer lets go of the store once it sees its mint dropped.
    let mut last = None;
    for _ in 0..200 {
        match AppState::open(dir, 1000) {
            Ok(state) => return router(state),
            Err(error) => last = Some(error),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the server did not open again: {:#}", last.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mint_whose_disk_fills_up_takes_changes_again_once_it_has_room() {
    // A write past the limit fails with EFBIG once SIGXFSZ, which would end
    // the process, is handled.
    let _xfsz = signal(SignalKind::from_raw(Signal::XFSZ.as_raw())).unwrap();
    let dir = tempfile::tempdir().unwrap();
    init_state(dir.path()).unwrap();
    let authority = MintKey::generate();
    let mut policy = Policy::cash_like();
    policy.mint_authorities.push(authority.public_key());
    let spec = MintSpec {
        ledger: LEDGER,
        policy,
        rotate_every: None,
    };
    let created = create_mint(dir.path(), &spec).unwrap();
    let mint = created.mint;
    let log = dir
        .path()
        .join("mints")
        .join(mint.to_string())
        .join("log.bin");
    let info = format!("/v1/mints/{mint}/info");
    let app = router(AppState::open(dir.path(), 1000).unwrap());

    let alice = HolderKey::generate();
    let request = MintRequest::sign(&authority, &mint, 1, 8, alice.public_key());
    let path = format!("/v1/mints/{mint}/mint");
    let body = serde_json::to_value(&request).unwrap();
    let (status, answer) = call(&app, Method::POST, &path, None, body).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let note: Note = serde_json::from_value(answer["note"].clone()).unwrap();
    let bob = HolderKey::generate();
    let output = Output {
        amount: 8,
        holder: bob.public_key(),
    };
    let transfer = TransferRequest::single(note, &alice, output).unwrap();
    let transfer = serde_json::to_value(&transfer).unwrap();
    let verify = |note: &Note| json!({ "note": note });

    // The disk fills up: the log cannot grow.
    limit_files(Some(fs::metadata(&log).unwrap().len()));
    let (status, answer) = call(&app, Method::POST, "/v1/transfer", None, transfer.clone()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{answer}");
    assert_eq!(answer["error"], "storage");

    // The transfer took effect in memory only, so the mint rereads its
    // state from the disk straight away, which needs nothing written, and
    // answers from that. Readiness says it is failing.
    let (status, unspent) = call(&app, Method::POST, "/v1/verify", None, verify(&note)).await;
    assert_eq!(status, StatusCode::OK, "{unspent}");
    assert_eq!(unspent["state"], "live", "the transfer was not made");
    let (status, answer) = call(&app, Method::GET, &info, None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["sequence"], 1);
    let failing = json!([{"mint": mint, "state": "failing"}]);
    let (status, ready) = call(&app, Method::GET, "/readyz", None, Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{ready}");
    assert_eq!(ready["error"], "storage");
    assert_eq!(ready["mints"], failing);

    // While the disk is still full, the next change fails as the first did.
    let (status, answer) = call(&app, Method::POST, "/v1/transfer", None, transfer.clone()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{answer}");
    assert_eq!(answer["error"], "storage");

    // A change that writes nothing, as resuming a mint that runs, does not
    // tell whether the disk takes writes again.
    let resume = format!("/v1/mints/{mint}/resume");
    let token = Some(created.token.as_str());
    let (status, answer) = call(&app, Method::POST, &resume, token, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let (status, ready) = call(&app, Method::GET, "/readyz", None, Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{ready}");
    assert_eq!(ready["mints"], failing);

    // A mint that cannot reread its state, here as a directory stands where
    // its snapshot was, does not answer reads of its ledger from memory the
    // disk lacks, and readiness says why. The keys never move ahead of the
    // disk.
    let snapshot = log.with_file_name("snapshot.bin");
    let moved = log.with_file_name("moved");
    fs::rename(&snapshot, &moved).unwrap();
    fs::create_dir(&snapshot).unwrap();
    let (status, answer) = call(&app, Method::POST, "/v1/transfer", None, transfer.clone()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{answer}");
    for (method, path, body) in [
        (Method::POST, "/v1/verify", verify(&note)),
        (Method::GET, info.as_str(), Value::Null),
    ] {
        let (status, answer) = call(&app, method, path, None, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
        assert_eq!(answer["error"], "storage");
    }
    let keys = format!("/v1/mints/{mint}/keys");
    let (status, _) = call(&app, Method::GET, &keys, None, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let (status, ready) = call(&app, Method::GET, "/readyz", None, Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(ready["error"], "storage");
    assert_eq!(ready["mints"], json!([{"mint": mint, "state": "ahead"}]));
    let (status, _) = call(&app, Method::GET, "/healthz", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "the server still runs");
    let (status, answer) = call(&app, Method::POST, "/v1/transfer", None, transfer.clone()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{answer}");
    assert_eq!(answer["error"], "storage");

    // Once it can, the next read rereads its state, with no change needed.
    // It is still failing, since no change has reached the disk.
    fs::remove_dir(&snapshot).unwrap();
    fs::rename(&moved, &snapshot).unwrap();
    assert_eq!(
        call(&app, Method::POST, "/v1/verify", None, verify(&note)).await,
        (StatusCode::OK, unspent)
    );
    let (status, ready) = call(&app, Method::GET, "/readyz", None, Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{ready}");
    assert_eq!(ready["mints"], failing);

    // Once the disk has room, the same transfer goes through, and the mint
    // serves again.
    limit_files(None);
    let (status, answer) = call(&app, Method::POST, "/v1/transfer", None, transfer).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let paid: Note = serde_json::from_value(answer["notes"][0].clone()).unwrap();
    let (status, _) = call(&app, Method::GET, "/readyz", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let (_, spent) = call(&app, Method::POST, "/v1/verify", None, verify(&note)).await;
    let (_, live) = call(&app, Method::POST, "/v1/verify", None, verify(&paid)).await;
    assert_eq!(spent["state"], "spent");
    assert_eq!(live["state"], "live");
    let (status, served) = call(&app, Method::GET, &info, None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{served}");
    assert_eq!(served["sequence"], 2, "a mint and one transfer");

    // A restart while the disk is full cannot write a fresh snapshot, so
    // the server opens from the snapshot and log as they are, which hold
    // what it served.
    drop(app);
    limit_files(Some(1));
    let app = reopen(dir.path()).await;
    let (status, _) = call(&app, Method::GET, "/readyz", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let (_, reopened) = call(&app, Method::GET, &info, None, Value::Null).await;
    assert_eq!(reopened, served);
    assert_eq!(
        call(&app, Method::POST, "/v1/verify", None, verify(&note)).await,
        (StatusCode::OK, spent)
    );
    assert_eq!(
        call(&app, Method::POST, "/v1/verify", None, verify(&paid)).await,
        (StatusCode::OK, live)
    );

    // The next snapshot, which a rotation writes, purges the log once the
    // disk has room.
    limit_files(None);
    let logged = fs::metadata(&log).unwrap().len();
    let rotate = format!("/v1/mints/{mint}/rotate");
    let (status, answer) = call(
        &app,
        Method::POST,
        &rotate,
        Some(&created.token),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert!(fs::metadata(&log).unwrap().len() < logged);
}
