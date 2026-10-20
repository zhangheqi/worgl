//! The client and purse against a real server on a local port.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use worgl::{
    HolderKey, Issuer, IssuerKey, LedgerConfig, MintKey, MintRequest, Note, NoteStatus, Output,
    Policy, PublicKey, TransferRequest,
};
use worgl_client::{Client, Error, MemoryStore, Outcome, Purse, Retry, Store, Submitted};
use worgl_sdk::{PayError, PaymentRequest, ScheduledTransfer, Spacing};
use worgl_server::{AppState, MintSpec, init_state, router};

/// No waiting between transfers.
const NOW: Spacing = Spacing {
    min_secs: 0,
    max_secs: 0,
};

/// A day, in seconds: a period whose rotations, at midnight UTC, no test
/// waits for.
const DAY: u64 = 86_400;

/// Fast retries, so a refused transfer does not hold a test up.
const QUICK: Retry = Retry {
    attempts: 2,
    first_delay: Duration::from_millis(10),
};

/// A server on a local port with one mint.
struct Server {
    url: String,
    address: SocketAddr,
    mint: PublicKey,
    token: String,
    authority: MintKey,
    sequence: AtomicU64,
    /// Stops serving, and the task that ends once the state is let go.
    serving: Option<(oneshot::Sender<()>, JoinHandle<()>)>,
    dir: tempfile::TempDir,
}

/// Serves `state` on `listener` until told to stop.
fn serve(listener: TcpListener, state: AppState) -> (oneshot::Sender<()>, JoinHandle<()>) {
    let (stop, stopped) = oneshot::channel::<()>();
    let serving = tokio::spawn(async move {
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async {
                stopped.await.ok();
            })
            .await
            .unwrap()
    });
    (stop, serving)
}

impl Server {
    async fn start() -> Self {
        Self::start_with(Policy::cash_like()).await
    }

    async fn start_with(policy: Policy) -> Self {
        Self::create(policy, None).await
    }

    /// A server whose mint rotates every `period` seconds, besides on request.
    async fn rotating_every(period: u64) -> Self {
        Self::create(Policy::cash_like(), Some(period)).await
    }

    async fn create(mut policy: Policy, rotate_every: Option<u64>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        init_state(dir.path()).unwrap();
        let state = AppState::open(dir.path(), 1000).unwrap();
        let authority = MintKey::generate();
        policy.mint_authorities = vec![authority.public_key()];
        let created = state
            .create_mint(MintSpec {
                ledger: LedgerConfig {
                    layers: 4,
                    layer_size: 1024,
                },
                policy,
                rotate_every,
            })
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        Self {
            url: format!("http://{address}"),
            address,
            mint: created.mint,
            token: created.token,
            authority,
            sequence: AtomicU64::new(0),
            serving: Some(serve(listener, state)),
            dir,
        }
    }

    /// Stops serving, changes the mint's policy on disk as its operator
    /// would, and serves again at the same address.
    async fn restart_with(&mut self, edit: impl FnOnce(&mut Policy)) {
        let (stop, serving) = self.serving.take().unwrap();
        stop.send(()).unwrap();
        serving.await.unwrap();
        let file = self
            .dir
            .path()
            .join("mints")
            .join(self.mint.to_string())
            .join("policy.json");
        let mut policy: Policy =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        edit(&mut policy);
        std::fs::write(&file, serde_json::to_string(&policy).unwrap()).unwrap();
        // Each committer lets go of its store once it sees its mint dropped.
        for _ in 0..200 {
            if let Ok(state) = AppState::open(self.dir.path(), 1000) {
                let listener = TcpListener::bind(self.address).await.unwrap();
                self.serving = Some(serve(listener, state));
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the stores were never released");
    }

    fn client(&self) -> Client {
        Client::new(&self.url).unwrap()
    }

    fn purse(&self) -> Purse<MemoryStore> {
        Purse::new(self.client(), Default::default(), MemoryStore::new()).with_retry(QUICK)
    }

    /// Mints `amount` to `holder` through the API.
    async fn mint_to(&self, amount: u64, holder: PublicKey) -> Note {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let request = MintRequest::sign(&self.authority, &self.mint, sequence, amount, holder);
        self.client().mint(&self.mint, &request).await.unwrap()
    }

    /// Gives `purse` `amount` in notes of at most `largest`.
    async fn fund<S: Store>(&self, purse: &Purse<S>, amount: u64, largest: u64) {
        let request = purse
            .request_at_most(&self.mint, amount, largest)
            .await
            .unwrap();
        let mut notes = Vec::new();
        for output in &request.outputs {
            notes.push(self.mint_to(output.amount, output.holder).await);
        }
        assert_eq!(purse.accept(&notes).await.unwrap(), u128::from(amount));
    }

    async fn rotate(&self) {
        self.client().rotate(&self.mint, &self.token).await.unwrap();
    }
}

fn now(transfers: &[ScheduledTransfer]) -> Vec<ScheduledTransfer> {
    transfers
        .iter()
        .map(|t| ScheduledTransfer {
            after_secs: 0,
            request: t.request.clone(),
        })
        .collect()
}

fn all_answered(submitted: &Submitted, count: usize) {
    assert!(submitted.refused.is_empty(), "{:?}", submitted.refused);
    assert_eq!(submitted.answered, count);
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A server that answers `server`'s mint info as a machine whose clock is
/// `skew` seconds behind the server's would read it, and returns a client
/// for it: the next rotation and the answer's `Date` move on by `skew`,
/// which is negative for a clock ahead. It serves nothing else.
async fn skewed(server: &Server, skew: i64) -> Client {
    let (real, mint) = (server.client(), server.mint);
    let info = move || async move {
        let mut info = real.info(&mint).await.unwrap();
        let rotation = info.rotation.as_mut().unwrap();
        rotation.next_at = rotation.next_at.checked_add_signed(skew).unwrap();
        let shift = Duration::from_secs(skew.unsigned_abs());
        let date = if skew < 0 {
            SystemTime::now() - shift
        } else {
            SystemTime::now() + shift
        };
        let date = httpdate::fmt_http_date(date);
        ([(axum::http::header::DATE, date)], axum::Json(info))
    };
    let route = format!("/v1/mints/{mint}/info");
    let app = axum::Router::new().route(&route, axum::routing::get(info));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Client::new(&url).unwrap()
}

#[tokio::test]
async fn typed_calls_reach_every_route() {
    let server = Server::start().await;
    let client = server.client();

    let info = client.info(&server.mint).await.unwrap();
    assert_eq!(info.issuer, server.mint);
    assert_eq!(info.layers.len(), 4);
    assert!(!info.paused);
    assert_eq!(info.supply.minted, 0);
    assert_eq!(
        client.keys(&server.mint).await.unwrap().issuer(),
        &server.mint
    );

    let alice = worgl::HolderKey::generate();
    let note = server.mint_to(8, alice.public_key()).await;
    let verified = client.verify(&note).await.unwrap();
    assert_eq!(verified.amount, 8);
    assert!(matches!(
        verified.status,
        NoteStatus::Live { position: 2, .. }
    ));

    let bob = worgl::HolderKey::generate();
    let output = worgl::Output {
        amount: 8,
        holder: bob.public_key(),
    };
    let transfer = worgl::TransferRequest::single(note, &alice, output).unwrap();
    let paid = client.transfer(&transfer).await.unwrap();
    assert_eq!(client.transfer(&transfer).await.unwrap(), paid);
    assert_eq!(
        client.verify(&note).await.unwrap().status,
        NoteStatus::Spent
    );

    server.rotate().await;
    let burn = worgl::BurnRequest::single(paid[0], &bob).unwrap();
    assert_eq!(client.burn(&burn).await.unwrap(), 8);
    assert_eq!(client.info(&server.mint).await.unwrap().supply.burned, 8);

    client.pause(&server.mint, &server.token).await.unwrap();
    assert!(client.info(&server.mint).await.unwrap().paused);
    client.resume(&server.mint, &server.token).await.unwrap();

    let error = client.rotate(&server.mint, "wrong").await.unwrap_err();
    assert_eq!(error.code(), Some("admin_unauthorized"));
    assert!(!error.is_transient());
    let other = worgl::IssuerKey::generate().public_key();
    let error = client.info(&other).await.unwrap_err();
    assert_eq!(error.code(), Some("unknown_mint"));
}

#[tokio::test]
async fn errors_say_what_went_wrong() {
    let server = Server::start().await;
    assert!(matches!(
        Client::new("ftp://example.com"),
        Err(Error::Url(_))
    ));
    assert!(matches!(Client::new("not a url"), Err(Error::Url(_))));

    // A path the server does not serve answers without an error code.
    let lost = Client::new(&format!("{}/nowhere/", server.url)).unwrap();
    match lost.info(&server.mint).await.unwrap_err() {
        Error::Refused(refusal) => {
            assert_eq!(refusal.status, 404);
            assert_eq!(refusal.code, "http");
        }
        error => panic!("{error}"),
    }

    // Nothing listening: the request never completed, so it may be retried.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let error = Client::new(&closed)
        .unwrap()
        .info(&server.mint)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Http(_)));
    assert!(error.is_transient());
}

#[tokio::test]
async fn a_payment_goes_through_the_server() {
    let server = Server::start().await;
    let (alice, bob) = (server.purse(), server.purse());
    server.fund(&alice, 8, 8).await;

    let request = bob.request(&server.mint, 5).await.unwrap();
    let payment = alice.pay(&request, NOW).await.unwrap();
    assert_eq!(alice.balance(&server.mint).await.unwrap().outgoing, 8);

    let received = bob.receive(&request, &payment).await.unwrap();
    all_answered(&received, 1);
    assert_eq!(received.value, 5);

    // Alice resends to learn her change; the mint answers from its receipt.
    let settled = alice.settle(NOW).await.unwrap();
    all_answered(&settled, 1);
    assert_eq!(settled.value, 3);
    assert_eq!(settled.notes, received.notes);

    // Both landed in the last layer: frozen until the next rotation.
    assert_eq!(bob.balance(&server.mint).await.unwrap().frozen, 5);
    assert_eq!(alice.balance(&server.mint).await.unwrap().frozen, 3);
    server.rotate().await;
    assert_eq!(bob.balance(&server.mint).await.unwrap().spendable, 5);
    let alice_now = alice.balance(&server.mint).await.unwrap();
    assert_eq!((alice_now.spendable, alice_now.outgoing), (3, 0));
}

#[tokio::test]
async fn the_payer_can_submit_and_hand_the_notes_over() {
    let server = Server::start().await;
    let (alice, bob) = (server.purse(), server.purse());
    server.fund(&alice, 8, 4).await;

    let request = bob.request_at_most(&server.mint, 6, 2).await.unwrap();
    let payment = alice.pay(&request, NOW).await.unwrap();
    assert_eq!(payment.transfers.len(), 2);
    let submitted = alice.submit(&payment.transfers).await.unwrap();
    all_answered(&submitted, 2);
    assert_eq!(submitted.value, 2);
    // What Alice hands over is Bob's notes only, none of her change.
    assert_eq!(submitted.theirs.len(), request.outputs.len());
    assert!(submitted.theirs.iter().all(|n| {
        request
            .outputs
            .iter()
            .any(|o| o.holder == *n.holder() && o.amount == n.amount())
    }));

    // A note with a forged signature is refused, and nothing is taken in.
    let mut forged = submitted.notes[0].to_bytes();
    let last = forged.len() - 1;
    forged[last] ^= 1;
    let forged = Note::from_bytes(&forged).unwrap();
    assert!(matches!(bob.accept(&[forged]).await, Err(Error::Note(_))));
    assert_eq!(bob.balance(&server.mint).await.unwrap().frozen, 0);

    assert_eq!(bob.accept(&submitted.notes).await.unwrap(), 6);
    assert_eq!(bob.accept(&submitted.notes).await.unwrap(), 0);
}

#[tokio::test]
async fn the_payee_hands_back_change_the_mint_no_longer_answers_for() {
    // Two receipts, kept no minimum time, stand in for a mint whose receipts
    // many transfers pushed out.
    let mut policy = Policy::cash_like();
    policy.receipt_capacity = 2;
    policy.receipt_min_age_secs = 0;
    let server = Server::start_with(policy).await;
    let (alice, bob, carol) = (server.purse(), server.purse(), server.purse());
    server.fund(&alice, 8, 8).await;
    server.fund(&carol, 3, 1).await;

    let request = bob.request(&server.mint, 3).await.unwrap();
    let payment = alice.pay(&request, NOW).await.unwrap();
    let received = bob.receive(&request, &payment).await.unwrap();
    all_answered(&received, 1);
    assert_eq!(received.value, 3);
    assert_eq!(
        received.theirs.len(),
        received.notes.len() - request.outputs.len()
    );

    // Other holders' transfers push Alice's receipt out.
    for _ in 0..3 {
        let dave = server.purse();
        let asked = dave.request(&server.mint, 1).await.unwrap();
        let paid = carol.pay(&asked, NOW).await.unwrap();
        all_answered(&dave.receive(&asked, &paid).await.unwrap(), 1);
    }
    let settled = alice.settle(NOW).await.unwrap();
    assert_eq!(settled.answered, 0);
    let [refused] = settled.refused.as_slice() else {
        panic!("{:?}", settled.refused)
    };
    assert_eq!(refused.error.code(), Some("spent"));
    assert_eq!(refused.at_stake, Some(5));
    // The note is gone either way, so the transfer is dropped; its change
    // keys stay.
    assert_eq!(refused.outcome, Outcome::Dropped);
    let balance = alice.balance(&server.mint).await.unwrap();
    assert_eq!(
        (balance.outgoing, balance.expected, balance.unconfirmed),
        (0, 0, 5)
    );
    assert_eq!(alice.holder().outgoing().count(), 0);
    let again = alice.settle(NOW).await.unwrap();
    assert_eq!((again.answered, again.refused.len()), (0, 0));

    // A forged note is refused and takes nothing in.
    let mut forged = received.theirs[0].to_bytes();
    let last = forged.len() - 1;
    forged[last] ^= 1;
    let forged = Note::from_bytes(&forged).unwrap();
    assert!(matches!(alice.accept(&[forged]).await, Err(Error::Note(_))));
    assert_eq!(alice.balance(&server.mint).await.unwrap().unconfirmed, 5);

    // The change Bob hands back is still taken in.
    assert_eq!(alice.accept(&received.theirs).await.unwrap(), 5);
    let balance = alice.balance(&server.mint).await.unwrap();
    assert_eq!(
        (
            balance.frozen,
            balance.outgoing,
            balance.expected,
            balance.unconfirmed
        ),
        (5, 0, 0, 0)
    );
}

#[tokio::test]
async fn a_request_for_a_note_the_mint_would_not_make_is_refused_before_signing() {
    let server = Server::start().await;
    let alice = Purse::new(server.client(), Default::default(), Flaky::default());
    server.fund(&alice, 8, 8).await;
    let saved = alice.store().inner.saved().unwrap();

    let request = PaymentRequest {
        mint: server.mint,
        outputs: vec![Output {
            amount: 3,
            holder: HolderKey::generate().public_key(),
        }],
    };
    assert!(matches!(
        alice.pay(&request, NOW).await,
        Err(Error::Pay(PayError::Denomination(3)))
    ));
    // Nothing is parked: no transfer to resend, nothing saved.
    let balance = alice.balance(&server.mint).await.unwrap();
    assert_eq!(
        (balance.spendable, balance.outgoing, balance.expected),
        (8, 0, 0)
    );
    assert_eq!(alice.holder().payments().count(), 0);
    assert_eq!(alice.store().inner.saved().unwrap(), saved);
    let settled = alice.settle(NOW).await.unwrap();
    assert_eq!((settled.answered, settled.refused.len()), (0, 0));
}

/// A server that answers every transfer with `status` and the error
/// `code`, as a gateway or a server of another version might, and returns
/// its URL.
async fn refusing(status: u16, code: &'static str) -> String {
    let answer = move || async move {
        let body = format!(r#"{{"error":"{code}","message":"sign in first"}}"#);
        (axum::http::StatusCode::from_u16(status).unwrap(), body)
    };
    let app = axum::Router::new().route("/v1/transfer", axum::routing::post(answer));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
async fn a_refusal_the_mint_would_not_give_drops_or_cancels_nothing() {
    let server = Server::start().await;
    let (alice, bob) = (server.purse(), server.purse());
    server.fund(&alice, 8, 8).await;
    let request = bob.request(&server.mint, 5).await.unwrap();
    alice.pay(&request, NOW).await.unwrap();

    // The mint answers `spent` and `expired` with 409, and never answers
    // `unauthorized` or `duplicate_input` to a transfer a holder signed.
    for (status, code) in [
        (401, "spent"),
        (401, "expired"),
        (403, "unauthorized"),
        (400, "duplicate_input"),
    ] {
        let client = Client::new(&refusing(status, code).await).unwrap();
        let lost = Purse::new(client, alice.holder(), MemoryStore::new()).with_retry(QUICK);
        let settled = lost.settle(NOW).await.unwrap();
        let [refused] = settled.refused.as_slice() else {
            panic!("{:?}", settled.refused)
        };
        assert_eq!(refused.error.code(), Some(code));
        assert_eq!(refused.outcome, Outcome::Kept, "{code}");
        let holder = lost.holder();
        assert_eq!(
            (holder.outgoing().count(), holder.notes().count()),
            (1, 0),
            "{code}"
        );
    }

    // The mint's own server still answers it.
    all_answered(&alice.settle(NOW).await.unwrap(), 1);
    assert_eq!(alice.balance(&server.mint).await.unwrap().frozen, 3);
}

#[tokio::test]
async fn a_transfer_another_server_does_not_know_stays_outgoing() {
    let (server, other) = (Server::start().await, Server::start().await);
    let (alice, bob) = (server.purse(), server.purse());
    server.fund(&alice, 8, 8).await;
    let request = bob.request(&server.mint, 5).await.unwrap();
    alice.pay(&request, NOW).await.unwrap();

    // Pointed at a server that is not the mint's, the purse keeps it.
    let lost = Purse::new(other.client(), alice.holder(), MemoryStore::new()).with_retry(QUICK);
    let settled = lost.settle(NOW).await.unwrap();
    let [refused] = settled.refused.as_slice() else {
        panic!("{:?}", settled.refused)
    };
    assert_eq!(refused.error.code(), Some("unknown_mint"));
    assert_eq!(refused.outcome, Outcome::Kept);
    assert_eq!(lost.holder().outgoing().count(), 1);

    // The mint's own server still answers it.
    let back = Purse::new(server.client(), lost.holder(), MemoryStore::new());
    all_answered(&back.settle(NOW).await.unwrap(), 1);
    assert_eq!(back.balance(&server.mint).await.unwrap().frozen, 3);
}

#[tokio::test]
async fn a_transfer_the_policy_now_refuses_is_cancelled() {
    let mut server = Server::start().await;
    let (alice, bob) = (server.purse(), server.purse());
    server.fund(&alice, 8, 8).await;
    let request = bob.request(&server.mint, 5).await.unwrap();
    let payment = alice.pay(&request, NOW).await.unwrap();

    // The operator allows one output per transfer from now on.
    server.restart_with(|policy| policy.max_outputs = 1).await;
    let alice = Purse::new(server.client(), alice.holder(), MemoryStore::new()).with_retry(QUICK);
    let settled = alice.settle(NOW).await.unwrap();
    let [refused] = settled.refused.as_slice() else {
        panic!("{:?}", settled.refused)
    };
    assert_eq!(refused.error.code(), Some("policy"));
    assert_eq!(refused.outcome, Outcome::Cancelled);
    // The note is back; the change keys stay in case the transfer went
    // through under the old policy.
    let balance = alice.balance(&server.mint).await.unwrap();
    assert_eq!(
        (balance.spendable, balance.outgoing, balance.unconfirmed),
        (8, 0, 3)
    );
    let spends = payment.transfers[0].request.inputs[0].note;
    assert_eq!(alice.holder().notes().collect::<Vec<_>>(), [&spends]);
    assert_eq!(alice.holder().payments().count(), 0);
}

#[tokio::test]
async fn a_settle_that_carries_out_a_payment_keeps_the_payees_notes() {
    let server = Server::start().await;
    let (alice, bob) = (server.purse(), server.purse());
    server.fund(&alice, 8, 8).await;
    let request = bob.request(&server.mint, 5).await.unwrap();
    let payment = alice.pay(&request, NOW).await.unwrap();
    let holder = alice.holder();
    let [made] = holder.payments().collect::<Vec<_>>()[..] else {
        panic!("one payment is kept")
    };
    assert_eq!((&made.request, &made.payment), (&request, &payment));

    // Bob never submits; Alice's settle carries the payment out.
    let settled = alice.settle(NOW).await.unwrap();
    all_answered(&settled, 1);
    assert_eq!(settled.value, 3);
    let owed = alice.holder().payments().next().unwrap().owed.clone();
    assert_eq!(owed, settled.theirs);
    assert_eq!(owed.len(), request.outputs.len());

    // Handed to Bob, who keeps them, and then no longer kept by Alice.
    assert_eq!(bob.accept(&owed).await.unwrap(), 5);
    assert_eq!(alice.forget_owed(&owed).unwrap(), 5);
    assert_eq!(alice.holder().payments().count(), 0);
    assert_eq!(alice.forget_owed(&owed).unwrap(), 0);
    let balance = alice.balance(&server.mint).await.unwrap();
    assert_eq!((balance.frozen, balance.outgoing), (3, 0));
}

#[tokio::test]
async fn refreshing_resends_what_the_next_rotation_would_cut_off() {
    let server = Server::start().await;
    let alice = Purse::new(server.client(), Default::default(), Flaky::default());
    let bob = server.purse();
    server.fund(&alice, 12, 8).await;
    server.rotate().await;
    server.rotate().await;
    // Alice pays with her 4, which dies at the next rotation like her 8.
    let request = bob.request(&server.mint, 3).await.unwrap();
    let payment = alice.pay(&request, NOW).await.unwrap();

    // A plan that fails to save takes back the refresh, not the payment.
    alice.store().failing.store(true, Ordering::Relaxed);
    assert!(matches!(
        alice.plan_refresh(&server.mint).await,
        Err(Error::Store(_))
    ));
    let holder = alice.holder();
    let outgoing: Vec<_> = holder.outgoing().collect();
    assert_eq!(outgoing, [&payment.transfers[0].request]);
    let balance = alice.balance(&server.mint).await.unwrap();
    assert_eq!(
        (balance.spendable, balance.outgoing, balance.expected),
        (8, 4, 1)
    );
    alice.store().failing.store(false, Ordering::Relaxed);

    let plan = alice.plan_refresh(&server.mint).await.unwrap();
    assert_eq!(plan.len(), 2);
    let done = alice.submit(&now(&plan)).await.unwrap();
    all_answered(&done, 2);
    assert_eq!(done.value, 9);
    assert_eq!(alice.holder().outgoing().count(), 0);
    // Bob, submitting later, gets the same notes.
    let received = bob.receive(&request, &payment).await.unwrap();
    all_answered(&received, 1);
    assert_eq!(received.value, 3);
}

#[tokio::test]
async fn refreshing_keeps_notes_alive_across_rotations() {
    let server = Server::start().await;
    let (kept, idle) = (server.purse(), server.purse());
    server.fund(&kept, 4, 4).await;
    server.fund(&idle, 4, 4).await;
    server.rotate().await;
    server.rotate().await;

    // Both notes are now in the first layer and die at the next rotation.
    assert_eq!(kept.balance(&server.mint).await.unwrap().expiring, 4);
    let plan = kept.plan_refresh(&server.mint).await.unwrap();
    assert_eq!(plan.len(), 1);
    let refreshed = kept.submit(&now(&plan)).await.unwrap();
    all_answered(&refreshed, 1);
    assert_eq!(refreshed.value, 4);

    server.rotate().await;
    assert_eq!(kept.balance(&server.mint).await.unwrap().spendable, 4);
    assert_eq!(kept.prune(&server.mint).await.unwrap(), 0);
    assert_eq!(idle.prune(&server.mint).await.unwrap(), 4);
    assert_eq!(idle.holder().notes().count(), 0);
}

#[tokio::test]
async fn a_clock_ahead_of_the_mints_drops_nothing_live() {
    let server = Server::rotating_every(DAY).await;
    let alice = server.purse();
    server.fund(&alice, 12, 8).await;
    server.rotate().await;
    server.rotate().await;
    let notes: Vec<Note> = alice.holder().notes().copied().collect();

    // Ten periods ahead, the clock has passed the next rotation by far, but
    // the mint has not made it.
    let ahead = Purse::new(
        skewed(&server, -10 * DAY as i64).await,
        alice.holder(),
        MemoryStore::new(),
    );
    let info = server.client().info(&server.mint).await.unwrap();
    let view = ahead.client().view(&server.mint).await.unwrap();
    assert_eq!(view.oldest, info.layers[0].generation);
    assert_eq!(ahead.prune(&server.mint).await.unwrap(), 0);
    assert_eq!(ahead.holder().notes().count(), 2);
    assert_eq!(ahead.balance(&server.mint).await.unwrap().expiring, 12);
    for note in &notes {
        let status = server.client().verify(note).await.unwrap().status;
        assert!(matches!(status, NoteStatus::Live { position: 0, .. }));
    }

    // They die at the next rotation the mint makes, so they are refreshed,
    // and the mint takes the refreshes.
    let plan = ahead.plan_refresh(&server.mint).await.unwrap();
    assert_eq!(plan.len(), 2);
    let back = Purse::new(server.client(), ahead.holder(), MemoryStore::new()).with_retry(QUICK);
    let refreshed = back.submit(&now(&plan)).await.unwrap();
    all_answered(&refreshed, 2);
    assert_eq!(refreshed.value, 12);
}

#[tokio::test]
async fn a_clock_behind_the_mints_still_refreshes_before_the_rotation() {
    let server = Server::rotating_every(DAY).await;
    let alice = server.purse();
    server.fund(&alice, 8, 2).await;
    server.rotate().await;
    server.rotate().await;

    // A year behind, the clock would put the next rotation a year further.
    let behind = Purse::new(
        skewed(&server, 365 * DAY as i64).await,
        alice.holder(),
        MemoryStore::new(),
    );
    let plan = behind.plan_refresh(&server.mint).await.unwrap();
    let info = server.client().info(&server.mint).await.unwrap();
    let left = info.rotation.unwrap().next_at.saturating_sub(unix_now());
    let times: Vec<u64> = plan.iter().map(|t| t.after_secs).collect();
    assert_eq!(times.len(), 4);
    assert!(
        times.iter().all(|&after| after < left),
        "{times:?} with {left} s left"
    );
}

#[tokio::test]
async fn a_paused_mint_is_retried_then_reported() {
    let server = Server::start().await;
    let (alice, bob) = (server.purse(), server.purse());
    server.fund(&alice, 8, 8).await;
    let request = bob.request(&server.mint, 8).await.unwrap();
    let payment = alice.pay(&request, NOW).await.unwrap();

    let client = server.client();
    client.pause(&server.mint, &server.token).await.unwrap();
    let received = bob.receive(&request, &payment).await.unwrap();
    assert_eq!((received.answered, received.value), (0, 0));
    assert_eq!(received.refused.len(), 1);
    let refused = &received.refused[0];
    assert_eq!(refused.error.code(), Some("paused"));
    assert!(refused.error.is_transient());

    // Once the mint resumes, the same transfer goes through.
    client.resume(&server.mint, &server.token).await.unwrap();
    let retried = bob.receive(&request, &payment).await.unwrap();
    all_answered(&retried, 1);
    assert_eq!(retried.value, 8);
}

#[tokio::test]
async fn a_busy_mint_is_waited_out_as_it_asks() {
    // The mint keeps one receipt, for at least 3 s.
    let server = Server::start_with(Policy {
        receipt_capacity: 1,
        receipt_min_age_secs: 3,
        ..Policy::cash_like()
    })
    .await;
    let (alice, bob) = (server.purse(), server.purse());
    server.fund(&alice, 8, 4).await;
    let first = bob.request(&server.mint, 4).await.unwrap();
    let paid = alice.pay(&first, NOW).await.unwrap();
    all_answered(&bob.receive(&first, &paid).await.unwrap(), 1);

    // The next transfer would push that receipt out while it is young.
    let second = bob.request(&server.mint, 4).await.unwrap();
    let payment = alice.pay(&second, NOW).await.unwrap();
    let request = &payment.transfers[0].request;
    let error = server.client().transfer(request).await.unwrap_err();
    assert_eq!(error.code(), Some("busy"));
    assert!(error.is_transient());
    let asked = error.retry_after().unwrap();
    assert!((1..=3).contains(&asked.as_secs()), "{asked:?}");

    // The purse waits as long as the mint asks, not its own 10 ms, and the
    // transfer then goes through.
    let started = Instant::now();
    all_answered(&bob.receive(&second, &payment).await.unwrap(), 1);
    assert!(started.elapsed() >= Duration::from_secs(1));
}

/// A server whose answers to `/v1/transfer` are `busy`, asking for a
/// second's wait, and then `503 no_leader`, asking for the same. Returns its
/// URL and how many transfers it was sent.
async fn busy_then_leaderless() -> (String, Arc<AtomicUsize>) {
    let tries = Arc::new(AtomicUsize::new(0));
    let counted = tries.clone();
    let app = axum::Router::new().route(
        "/v1/transfer",
        axum::routing::post(move || {
            let tries = counted.clone();
            async move {
                let unavailable = StatusCode::SERVICE_UNAVAILABLE;
                let wait = [(header::RETRY_AFTER, "1")];
                if tries.fetch_add(1, Ordering::SeqCst) == 0 {
                    let body = json!({"error": "busy", "message": "busy"});
                    (unavailable, wait, Json(body)).into_response()
                } else {
                    let body = json!({"error": "no_leader", "message": "no leader"});
                    (unavailable, wait, Json(body)).into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, tries)
}

#[tokio::test]
async fn a_busy_answer_stretches_the_next_wait_only() {
    let (url, tries) = busy_then_leaderless().await;
    let retry = Retry {
        attempts: 3,
        first_delay: Duration::from_millis(10),
    };
    let purse = Purse::new(
        Client::new(&url).unwrap(),
        Default::default(),
        MemoryStore::new(),
    )
    .with_retry(retry);
    let mut issuer = Issuer::new(
        IssuerKey::generate(),
        LedgerConfig {
            layers: 2,
            layer_size: 64,
        },
        Policy::cash_like(),
    )
    .unwrap();
    let alice = HolderKey::generate();
    let note = issuer.mint(8, alice.public_key()).unwrap();
    let output = Output {
        amount: 8,
        holder: HolderKey::generate().public_key(),
    };
    let request = TransferRequest::single(note, &alice, output).unwrap();

    let started = Instant::now();
    let transfers = [ScheduledTransfer {
        after_secs: 0,
        request,
    }];
    let submitted = purse.submit(&transfers).await.unwrap();
    let waited = started.elapsed();
    assert_eq!(tries.load(Ordering::SeqCst), 3);
    let error = &submitted.refused[0].error;
    assert_eq!(error.code(), Some("no_leader"));
    assert_eq!(error.retry_after(), Some(Duration::from_secs(1)));
    // A second as busy asked, then 20 ms: the second's wait does not
    // double, and no_leader's Retry-After does not stretch it.
    assert!(
        waited >= Duration::from_secs(1) && waited < Duration::from_millis(1900),
        "{waited:?}"
    );
}

#[tokio::test]
async fn a_payment_for_another_request_is_not_submitted() {
    let server = Server::start().await;
    let (alice, bob) = (server.purse(), server.purse());
    server.fund(&alice, 8, 8).await;
    let asked = bob.request(&server.mint, 4).await.unwrap();
    let other = bob.request(&server.mint, 4).await.unwrap();
    let payment = alice.pay(&other, NOW).await.unwrap();

    assert!(matches!(
        bob.receive(&asked, &payment).await,
        Err(Error::Check(_))
    ));
    let spends = payment.transfers[0].request.inputs[0].note;
    let status = server.client().verify(&spends).await.unwrap().status;
    assert!(matches!(status, NoteStatus::Live { .. }));
}

/// A store whose saves can be made to fail.
#[derive(Default)]
struct Flaky {
    failing: AtomicBool,
    inner: MemoryStore,
}

impl Store for Flaky {
    fn save(&self, json: &str) -> io::Result<()> {
        if self.failing.load(Ordering::Relaxed) {
            return Err(io::Error::other("disk full"));
        }
        self.inner.save(json)
    }
}

#[tokio::test]
async fn nothing_is_handed_out_unless_it_is_saved() {
    let server = Server::start().await;
    let alice = Purse::new(server.client(), Default::default(), Flaky::default());
    let bob = Purse::new(server.client(), Default::default(), Flaky::default());
    server.fund(&alice, 8, 8).await;
    let request = bob.request(&server.mint, 5).await.unwrap();
    let saved = alice.store().inner.saved().unwrap();

    alice.store().failing.store(true, Ordering::Relaxed);
    assert!(matches!(
        alice.pay(&request, NOW).await,
        Err(Error::Store(_))
    ));
    let balance = alice.balance(&server.mint).await.unwrap();
    assert_eq!(
        (balance.spendable, balance.outgoing, balance.expected),
        (8, 0, 0)
    );
    assert_eq!(alice.store().inner.saved().unwrap(), saved);

    bob.store().failing.store(true, Ordering::Relaxed);
    assert!(matches!(
        bob.request(&server.mint, 3).await,
        Err(Error::Store(_))
    ));
    assert_eq!(bob.balance(&server.mint).await.unwrap().expected, 5);
}

#[tokio::test]
async fn a_purse_reopens_from_its_file() {
    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alice.json");

    let alice = Purse::open(server.client(), &path).unwrap();
    assert_eq!(alice.holder().notes().count(), 0);
    server.fund(&alice, 8, 2).await;
    drop(alice);

    let alice = Purse::open(server.client(), &path).unwrap();
    assert_eq!(alice.balance(&server.mint).await.unwrap().spendable, 8);
    let mut files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    files.sort();
    assert_eq!(files, [".alice.json.lock", "alice.json"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    std::fs::write(&path, "{").unwrap();
    assert!(matches!(
        Purse::open(server.client(), &path),
        Err(Error::Load(_))
    ));
}

#[tokio::test]
async fn purses_on_one_file_keep_each_others_changes() {
    let server = Server::start().await;
    let alice = server.purse();
    server.fund(&alice, 8, 4).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bob.json");

    // Two processes open Bob's purse, and each changes it after the other.
    let bob = Purse::open(server.client(), &path).unwrap();
    let also_bob = Purse::open(server.client(), &path).unwrap();
    let first = bob.request(&server.mint, 4).await.unwrap();
    let second = also_bob.request(&server.mint, 4).await.unwrap();
    let first_paid = alice.pay(&first, NOW).await.unwrap();
    let second_paid = alice.pay(&second, NOW).await.unwrap();
    all_answered(&bob.receive(&first, &first_paid).await.unwrap(), 1);
    all_answered(&also_bob.receive(&second, &second_paid).await.unwrap(), 1);

    let reopened = Purse::open(server.client(), &path).unwrap();
    let balance = reopened.balance(&server.mint).await.unwrap();
    assert_eq!((balance.frozen, balance.expected), (8, 0));
}
