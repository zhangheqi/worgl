//! The `worgl` binary against a real server on a local port.

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use worgl::{LedgerConfig, MintKey, MintRequest, Note, Policy, PublicKey};
use worgl_client::{Client, MemoryStore, Purse};
use worgl_sdk::vault::{MintEntry, Vault};
use worgl_sdk::{Holder, MintView, PaymentRequest, Spacing};
use worgl_server::{AppState, MintSpec, init_state, router};

const PASSPHRASE: &str = "correct horse battery staple";

/// A server with one mint, and a directory to run `worgl` in.
struct Setup {
    url: String,
    mint: PublicKey,
    token: String,
    dir: tempfile::TempDir,
}

/// What a run of `worgl` printed.
struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Setup {
    async fn start() -> Self {
        Self::with(Policy::cash_like(), None).await
    }

    /// A mint with `policy`, rotating every `rotate_every` seconds if given.
    async fn with(policy: Policy, rotate_every: Option<u64>) -> Self {
        Self::create(policy, rotate_every, 1024).await
    }

    /// A mint whose layers hold `layer_size` notes each.
    async fn with_layer_size(layer_size: u32) -> Self {
        Self::create(Policy::cash_like(), None, layer_size).await
    }

    async fn create(mut policy: Policy, rotate_every: Option<u64>, layer_size: u32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        init_state(&state).unwrap();
        let app = AppState::open(&state, 1000).unwrap();
        let authority = MintKey::generate();
        policy.mint_authorities = vec![authority.public_key()];
        let created = app
            .create_mint(MintSpec {
                ledger: LedgerConfig {
                    layers: 4,
                    layer_size,
                },
                policy,
                rotate_every,
            })
            .await
            .unwrap();
        fs::write(
            dir.path().join("authority.key"),
            hex::encode(authority.to_secret_bytes()),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(app)).await.unwrap() });
        Self {
            url,
            mint: created.mint,
            token: created.token,
            dir,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Runs `worgl` with `purse` and the test passphrase.
    async fn worgl(&self, purse: &str, args: &[&str]) -> Run {
        self.worgl_with(purse, PASSPHRASE, args).await
    }

    async fn worgl_with(&self, purse: &str, passphrase: &str, args: &[&str]) -> Run {
        let mut command = self.command(purse, passphrase, args);
        let output = tokio::task::spawn_blocking(move || command.output())
            .await
            .unwrap()
            .unwrap();
        Run {
            ok: output.status.success(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    /// `worgl` with `purse` and `passphrase`, to run in the directory.
    fn command(&self, purse: &str, passphrase: &str, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_worgl"));
        command
            .current_dir(self.dir.path())
            .env("WORGL_SERVER", &self.url)
            .env("WORGL_PURSE", purse)
            .env("WORGL_PASSPHRASE", passphrase)
            .args(args);
        command
    }

    /// Runs `worgl` and expects it to succeed.
    async fn ok(&self, purse: &str, args: &[&str]) -> Run {
        let run = self.worgl(purse, args).await;
        assert!(run.ok, "worgl {args:?} failed:\n{}", run.stderr);
        run
    }

    async fn mint_into(&self, purse: &str, amount: u64) {
        let mint = self.mint.to_string();
        let amount = amount.to_string();
        let args = ["mint", &mint, &amount, "--authority-key", "authority.key"];
        let run = self.ok(purse, &args).await;
        assert!(
            run.stderr.contains(&format!("Minted {amount}")),
            "{}",
            run.stderr
        );
    }

    /// Serves a faucet for the mint, signing with the authority key, and
    /// returns its URL.
    async fn faucet(&self) -> String {
        let text = fs::read_to_string(self.path("authority.key")).unwrap();
        let mut secret = [0u8; 32];
        hex::decode_to_slice(text.trim(), &mut secret).unwrap();
        let faucet = worgl_faucet::Faucet::new(worgl_faucet::Config {
            servers: vec![Client::new(&self.url).unwrap()],
            mint: self.mint,
            authority: MintKey::from_secret_bytes(&secret),
            limits: worgl_faucet::Limits::default(),
            behind_proxy: false,
            allow_origins: Vec::new(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = worgl_faucet::router(faucet).into_make_service_with_connect_info::<SocketAddr>();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// Makes `count` transfers between purses of the library's own.
    async fn traffic(&self, count: usize) {
        let client = Client::new(&self.url).unwrap();
        let purse = || Purse::new(client.clone(), Default::default(), MemoryStore::new());
        let payer = purse();
        let request = payer
            .request_at_most(&self.mint, count as u64, 1)
            .await
            .unwrap();
        let text = fs::read_to_string(self.path("authority.key")).unwrap();
        let mut secret = [0u8; 32];
        hex::decode_to_slice(text.trim(), &mut secret).unwrap();
        let authority = MintKey::from_secret_bytes(&secret);
        // Above the sequence numbers `worgl mint` took from the clock.
        let first = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        let mut notes = Vec::new();
        for (sequence, output) in (first..).zip(&request.outputs) {
            let signed = MintRequest::sign(
                &authority,
                &self.mint,
                sequence,
                output.amount,
                output.holder,
            );
            notes.push(client.mint(&self.mint, &signed).await.unwrap());
        }
        payer.accept(&notes).await.unwrap();
        let none = Spacing {
            min_secs: 0,
            max_secs: 0,
        };
        for _ in 0..count {
            let payee = purse();
            let asked = payee.request(&self.mint, 1).await.unwrap();
            let paid = payer.pay(&asked, none).await.unwrap();
            let received = payee.receive(&asked, &paid).await.unwrap();
            assert!(received.refused.is_empty(), "{:?}", received.refused);
        }
    }

    async fn rotate(&self) {
        let client = Client::new(&self.url).unwrap();
        client.rotate(&self.mint, &self.token).await.unwrap();
    }

    async fn balance(&self, purse: &str) -> String {
        self.ok(purse, &["balance", &self.mint.to_string()])
            .await
            .stdout
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_from_the_command_line() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;

    setup
        .ok("bob.worgl", &["request", &mint, "5", "-o", "request.json"])
        .await;
    let pay = [
        "pay",
        "request.json",
        "--spacing",
        "0..0",
        "-o",
        "payment.json",
    ];
    setup.ok("alice.worgl", &pay).await;
    let received = setup
        .ok("bob.worgl", &["receive", "request.json", "payment.json"])
        .await;
    assert!(
        received.stderr.contains("Received 5"),
        "{}",
        received.stderr
    );
    let settled = setup
        .ok("alice.worgl", &["settle", "--spacing", "0..0"])
        .await;
    assert!(settled.stderr.contains("Took in 3"), "{}", settled.stderr);

    assert!(setup.balance("bob.worgl").await.contains("frozen     5"));
    setup.rotate().await;
    assert!(setup.balance("bob.worgl").await.contains("spendable  5"));
    let alice = setup.balance("alice.worgl").await;
    assert!(alice.contains("spendable  3"), "{alice}");
    assert!(alice.contains("sending    0"), "{alice}");

    let notes = setup.ok("bob.worgl", &["notes"]).await.stdout;
    assert!(notes.contains(&mint), "{notes}");
    assert!(
        notes.contains("spendable, dies in 3 rotation(s)"),
        "{notes}"
    );
    let info = setup.ok("bob.worgl", &["info", &mint]).await.stdout;
    assert!(info.contains("8 outstanding (8 minted"), "{info}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_payer_can_submit_and_hand_the_notes_over() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;

    setup
        .ok("bob.worgl", &["request", &mint, "6", "-o", "request.json"])
        .await;
    let pay = [
        "pay",
        "request.json",
        "--spacing",
        "0..0",
        "-o",
        "payment.json",
    ];
    setup.ok("alice.worgl", &pay).await;
    let submitted = setup
        .ok(
            "alice.worgl",
            &["submit", "payment.json", "-o", "notes.json"],
        )
        .await;
    assert!(
        submitted.stderr.contains("Kept in change 2"),
        "{}",
        submitted.stderr
    );
    // Only Bob's notes: Alice's change stays with her.
    let request: PaymentRequest =
        serde_json::from_str(&fs::read_to_string(setup.path("request.json")).unwrap()).unwrap();
    let notes: Vec<Note> =
        serde_json::from_str(&fs::read_to_string(setup.path("notes.json")).unwrap()).unwrap();
    assert_eq!(notes.len(), request.outputs.len());
    for note in &notes {
        assert!(
            request
                .outputs
                .iter()
                .any(|o| o.holder == *note.holder() && o.amount == note.amount())
        );
    }
    let accepted = setup.ok("bob.worgl", &["accept", "notes.json"]).await;
    assert!(accepted.stderr.contains("Kept 6"), "{}", accepted.stderr);
    // Handed over, so Alice keeps nothing for Bob.
    let outgoing = setup.ok("alice.worgl", &["outgoing"]).await;
    assert_eq!(outgoing.stdout, "");
    assert!(
        outgoing.stderr.contains("Nothing is outgoing."),
        "{}",
        outgoing.stderr
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_payees_notes_stay_kept_when_submit_cannot_write_them() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;
    setup
        .ok("bob.worgl", &["request", &mint, "6", "-o", "request.json"])
        .await;
    let pay = [
        "pay",
        "request.json",
        "--spacing",
        "0..0",
        "-o",
        "payment.json",
    ];
    let id = paid_id(&setup.ok("alice.worgl", &pay).await);

    let lost = setup
        .worgl(
            "alice.worgl",
            &["submit", "payment.json", "-o", "missing/notes.json"],
        )
        .await;
    assert!(!lost.ok);
    assert!(
        lost.stderr.contains(&format!(
            "the payee's notes are kept in the purse; write them with `worgl export {id} \
             --notes -o FILE`"
        )),
        "{}",
        lost.stderr
    );
    let exported = setup
        .ok(
            "alice.worgl",
            &["export", &id, "--notes", "-o", "notes.json"],
        )
        .await;
    assert!(
        exported.stderr.contains(&format!(
            "Wrote 2 note(s) worth 6 for the payee of payment {id}"
        )),
        "{}",
        exported.stderr
    );
    let accepted = setup.ok("bob.worgl", &["accept", "notes.json"]).await;
    assert_eq!(accepted.stderr.trim(), "Kept 6.");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_payee_hands_back_the_change() {
    // Two receipts, kept no minimum time, stand in for a mint whose receipts
    // many transfers pushed out.
    let mut policy = Policy::cash_like();
    policy.receipt_capacity = 2;
    policy.receipt_min_age_secs = 0;
    let setup = Setup::with(policy, None).await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;

    setup
        .ok("bob.worgl", &["request", &mint, "3", "-o", "request.json"])
        .await;
    let pay = [
        "pay",
        "request.json",
        "--spacing",
        "0..0",
        "-o",
        "payment.json",
    ];
    let paid = setup.ok("alice.worgl", &pay).await;
    // The mint rotates only on request: there is no deadline to give.
    assert!(!paid.stderr.contains("Settle"), "{}", paid.stderr);
    let received = setup
        .ok(
            "bob.worgl",
            &[
                "receive",
                "request.json",
                "payment.json",
                "-o",
                "change.json",
            ],
        )
        .await;
    assert!(
        received.stderr.contains("Received 3"),
        "{}",
        received.stderr
    );
    assert!(
        received
            .stderr
            .contains("who keeps the change with `worgl accept`"),
        "{}",
        received.stderr
    );

    // Too late to settle: the mint no longer has the answer. A copy of
    // Alice's purse, made first, takes the change too late, below.
    setup.traffic(3).await;
    fs::copy(setup.path("alice.worgl"), setup.path("late.worgl")).unwrap();
    let settled = setup
        .ok("alice.worgl", &["settle", "--spacing", "0..0"])
        .await;
    assert!(
        settled
            .stderr
            .contains("possibly applied: the mint no longer has its answer, 5 may be lost"),
        "{}",
        settled.stderr
    );
    assert!(
        settled.stderr.contains(
            "dropped: the note it spends is gone either way; its change is still kept if \
             handed back"
        ),
        "{}",
        settled.stderr
    );
    assert!(
        settled.stderr.contains("`worgl accept FILE`"),
        "{}",
        settled.stderr
    );
    assert!(
        !settled.stderr.contains("try again later"),
        "{}",
        settled.stderr
    );
    let alice = setup.balance("alice.worgl").await;
    assert!(alice.contains("sending    0"), "{alice}");
    assert!(alice.contains("unconfirmed 5"), "{alice}");

    // The change Bob sent back is still taken in.
    let accepted = setup.ok("alice.worgl", &["accept", "change.json"]).await;
    assert_eq!(accepted.stderr.trim(), "Kept 5.");
    let alice = setup.balance("alice.worgl").await;
    assert!(alice.contains("frozen     5"), "{alice}");
    assert!(alice.contains("sending    0"), "{alice}");
    assert!(!alice.contains("unconfirmed"), "{alice}");
    let nothing = setup.ok("alice.worgl", &["settle"]).await;
    assert!(
        nothing.stderr.contains("Nothing to settle"),
        "{}",
        nothing.stderr
    );

    // Change handed back after its layer is dropped is skipped, not refused.
    for _ in 0..4 {
        setup.rotate().await;
    }
    let late = setup.ok("late.worgl", &["accept", "change.json"]).await;
    assert!(
        late.stderr.contains("Skipped 2 note(s) worth 5"),
        "{}",
        late.stderr
    );
    assert!(
        late.stderr
            .contains("Kept 0. 1 transfer(s) still outgoing; `worgl settle` resends them."),
        "{}",
        late.stderr
    );
    // Notes that were never a purse's are not counted as lost to it.
    setup.ok("carol.worgl", &["init"]).await;
    let carol = setup.ok("carol.worgl", &["accept", "change.json"]).await;
    assert_eq!(carol.stderr.trim(), "Kept 0.");
}

/// The payment id `worgl pay` printed.
fn paid_id(paid: &Run) -> String {
    let (_, rest) = paid.stderr.split_once(", payment ").unwrap();
    rest[..12].to_owned()
}

/// The transfer ids `worgl outgoing` printed.
fn transfer_ids(outgoing: &Run) -> Vec<String> {
    outgoing
        .stdout
        .lines()
        .filter_map(|line| line.trim().strip_prefix("transfer "))
        .map(|rest| rest.split(' ').next().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_settled_too_late_is_dropped() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;
    setup
        .ok("bob.worgl", &["request", &mint, "5", "-o", "request.json"])
        .await;
    let pay = [
        "pay",
        "request.json",
        "--spacing",
        "0..0",
        "-o",
        "payment.json",
    ];
    setup.ok("alice.worgl", &pay).await;

    // Bob never submits, and the note it spends expires.
    for _ in 0..3 {
        setup.rotate().await;
    }
    let settled = setup
        .ok("alice.worgl", &["settle", "--spacing", "0..0"])
        .await;
    assert!(settled.stderr.contains("expired"), "{}", settled.stderr);
    assert!(settled.stderr.contains("dropped: "), "{}", settled.stderr);
    let again = setup
        .ok("alice.worgl", &["settle", "--spacing", "0..0"])
        .await;
    assert!(
        again.stderr.contains("Nothing to settle."),
        "{}",
        again.stderr
    );
    let alice = setup.balance("alice.worgl").await;
    assert!(alice.contains("sending    0"), "{alice}");
    assert!(alice.contains("unconfirmed 3"), "{alice}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transfer_into_a_full_layer_is_cancelled() {
    // Layers of 64 notes. Carol's 64 notes of 1, paid from Dave's four of
    // 16, fill the last layer, where a hop from the mint layer lands.
    let setup = Setup::with_layer_size(64).await;
    let mint = setup.mint.to_string();
    for purse in ["alice.worgl", "bob.worgl", "carol.worgl", "dave.worgl"] {
        setup.ok(purse, &["init"]).await;
    }
    let minted = [
        "mint",
        &mint,
        "64",
        "--authority-key",
        "authority.key",
        "--at-most",
        "16",
    ];
    setup.ok("dave.worgl", &minted).await;
    let asked = ["request", &mint, "64", "--at-most", "1", "-o", "fill.json"];
    setup.ok("carol.worgl", &asked).await;
    let pay = ["pay", "fill.json", "--spacing", "0..0", "-o", "filled.json"];
    setup.ok("dave.worgl", &pay).await;
    setup
        .ok("carol.worgl", &["receive", "fill.json", "filled.json"])
        .await;

    setup.mint_into("alice.worgl", 8).await;
    setup
        .ok("bob.worgl", &["request", &mint, "5", "-o", "request.json"])
        .await;
    let pay = [
        "pay",
        "request.json",
        "--spacing",
        "0..0",
        "-o",
        "payment.json",
    ];
    setup.ok("alice.worgl", &pay).await;
    let received = setup
        .worgl("bob.worgl", &["receive", "request.json", "payment.json"])
        .await;
    assert!(!received.ok);
    assert!(
        received.stderr.contains("layer_full"),
        "{}",
        received.stderr
    );
    assert!(
        received.stderr.contains("1 transfer(s) did not go through"),
        "{}",
        received.stderr
    );
    assert!(
        !received.stderr.contains("try again later"),
        "{}",
        received.stderr
    );

    // The mint never applied it, and never will: settling cancels it.
    let settled = setup
        .ok("alice.worgl", &["settle", "--spacing", "0..0"])
        .await;
    assert!(
        settled.stderr.contains(
            "cancelled: the mint will not apply it, so the note it spends is back in the purse"
        ),
        "{}",
        settled.stderr
    );
    let alice = setup.balance("alice.worgl").await;
    assert!(alice.starts_with("total      8  "), "{alice}");
    assert!(alice.contains("spendable  8"), "{alice}");
    assert!(alice.contains("change     0"), "{alice}");
    assert!(alice.contains("sending    0"), "{alice}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_is_paid_once() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;
    setup.mint_into("alice.worgl", 8).await;
    setup
        .ok("bob.worgl", &["request", &mint, "3", "-o", "request.json"])
        .await;

    // The payment cannot be written, but it is made and saved.
    let pay = |out: &'static str| ["pay", "request.json", "--spacing", "0..0", "-o", out];
    let lost = setup
        .worgl("alice.worgl", &pay("missing/payment.json"))
        .await;
    assert!(!lost.ok);
    assert!(
        lost.stderr
            .contains("the payment is recorded in the purse; write it again with `worgl export "),
        "{}",
        lost.stderr
    );
    let twice = setup.worgl("alice.worgl", &pay("payment.json")).await;
    assert!(!twice.ok);
    assert!(
        twice.stderr.contains("has paid already"),
        "{}",
        twice.stderr
    );
    let (_, kept) = twice.stderr.split_once("error: payment ").unwrap();
    let id = &kept[..12];
    assert!(
        twice.stderr.contains(&format!(
            "payment {id} pays this request already; `worgl export {id} -o FILE` writes it again"
        )),
        "{}",
        twice.stderr
    );
    assert!(!setup.path("payment.json").exists());
    let alice = setup.balance("alice.worgl").await;
    assert!(alice.starts_with("total      13  "), "{alice}");
    assert!(alice.contains("spendable  8"), "{alice}");
    assert!(alice.contains("change     5"), "{alice}");
    assert!(alice.contains("sending    3"), "{alice}");

    // `outgoing` names it, and `export` writes it again.
    let outgoing = setup.ok("alice.worgl", &["outgoing"]).await.stdout;
    let line = outgoing
        .lines()
        .find(|line| line.starts_with("  payment "))
        .unwrap();
    assert!(
        line.ends_with("pays 3, 1 of 1 transfer(s) outgoing"),
        "{outgoing}"
    );
    assert_eq!(&line["  payment ".len()..][..12], id);
    let exported = setup
        .ok("alice.worgl", &["export", &id[..6], "-o", "payment.json"])
        .await;
    assert!(
        exported
            .stderr
            .contains(&format!("Wrote payment {id} again")),
        "{}",
        exported.stderr
    );
    let received = setup
        .ok("bob.worgl", &["receive", "request.json", "payment.json"])
        .await;
    assert!(
        received.stderr.contains("Received 3"),
        "{}",
        received.stderr
    );
    // Nothing kept for Bob: he submitted it himself.
    let missing = setup.worgl("alice.worgl", &["export", id, "--notes"]).await;
    assert!(!missing.ok);
    assert!(
        missing.stderr.contains("no notes are kept for the payee"),
        "{}",
        missing.stderr
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transfer_is_cancelled_or_forgotten() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;
    setup.mint_into("alice.worgl", 8).await;
    for name in ["first", "second"] {
        let request = format!("{name}.json");
        setup
            .ok("bob.worgl", &["request", &mint, "3", "-o", &request])
            .await;
        let pay = ["pay", &request, "--spacing", "0..0"];
        setup.ok("alice.worgl", &pay).await;
    }
    let ids = transfer_ids(&setup.ok("alice.worgl", &["outgoing"]).await);
    let [first, second] = &ids[..] else {
        panic!("{ids:?}")
    };

    let cancelled = setup.ok("alice.worgl", &["cancel", first]).await;
    assert_eq!(
        cancelled.stderr.trim(),
        format!("Cancelled transfer {first}: its note of 8 is back in the purse.")
    );
    let alice = setup.balance("alice.worgl").await;
    assert!(alice.starts_with("total      13  "), "{alice}");
    assert!(alice.contains("spendable  8"), "{alice}");
    assert!(alice.contains("change     5"), "{alice}");
    assert!(alice.contains("sending    3"), "{alice}");
    assert!(alice.contains("unconfirmed 5"), "{alice}");

    let forgotten = setup.ok("alice.worgl", &["forget", &second[..4]]).await;
    assert!(
        forgotten
            .stderr
            .starts_with(&format!("Dropped transfer {second} and its note of 8.")),
        "{}",
        forgotten.stderr
    );
    let alice = setup.balance("alice.worgl").await;
    assert!(alice.starts_with("total      8  "), "{alice}");
    assert!(alice.contains("change     0"), "{alice}");
    assert!(alice.contains("sending    0"), "{alice}");
    assert!(alice.contains("unconfirmed 10"), "{alice}");
    let nothing = setup.ok("alice.worgl", &["outgoing"]).await;
    assert!(
        nothing.stderr.contains("Nothing is outgoing."),
        "{}",
        nothing.stderr
    );
    let again = setup.worgl("alice.worgl", &["cancel", first]).await;
    assert!(!again.ok);
    assert!(
        again.stderr.contains("`worgl outgoing` lists them"),
        "{}",
        again.stderr
    );

    // Its help says why a payee's transfer is not to be cancelled.
    let help = setup.ok("alice.worgl", &["cancel", "--help"]).await.stdout;
    assert!(help.contains("claim the same value"), "{help}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_is_not_exported_once_a_transfer_of_it_is_cancelled() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 4).await;
    setup.mint_into("alice.worgl", 4).await;
    setup
        .ok("bob.worgl", &["request", &mint, "6", "-o", "request.json"])
        .await;
    let pay = ["pay", "request.json", "--spacing", "0..0"];
    let id = paid_id(&setup.ok("alice.worgl", &pay).await);
    let ids = transfer_ids(&setup.ok("alice.worgl", &["outgoing"]).await);
    let [first, second] = &ids[..] else {
        panic!("{ids:?}")
    };

    // Exported whole, the payment would hand the cancelled transfer out again.
    setup.ok("alice.worgl", &["cancel", first]).await;
    let outgoing = setup.ok("alice.worgl", &["outgoing"]).await;
    assert_eq!(transfer_ids(&outgoing), std::slice::from_ref(second));
    // Still named as `pay` named it, and not whole.
    let line = outgoing
        .stdout
        .lines()
        .find(|line| line.starts_with("  payment "))
        .unwrap();
    assert!(
        line.starts_with(&format!("  payment {id}  pays ")),
        "{line}"
    );
    assert!(
        line.ends_with(" of 6, the rest cancelled or dropped, 1 of 1 transfer(s) outgoing"),
        "{line}"
    );
    // By its id, or the id of a transfer still in it.
    for name in [&id, second] {
        let export = ["export", name, "-o", "payment.json"];
        let refused = setup.worgl("alice.worgl", &export).await;
        assert!(!refused.ok);
        assert!(
            refused.stderr.contains(&format!(
                "payment {id} no longer pays its whole request, since a transfer of it was \
                 cancelled"
            )),
            "{}",
            refused.stderr
        );
    }
    assert!(!setup.path("payment.json").exists());
}

/// Writes `-o` through links, as a shell's `/dev/stdout` is one to
/// `/proc/self/fd/1`, and keeps the permissions of a file it replaces.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_goes_through_links_and_keeps_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("bob.worgl", &["init"]).await;
    let request = |name: &str| -> PaymentRequest {
        serde_json::from_str(&fs::read_to_string(setup.path(name)).unwrap()).unwrap()
    };
    let is_link = |name: &str| {
        fs::symlink_metadata(setup.path(name))
            .unwrap()
            .file_type()
            .is_symlink()
    };

    // Standard output sent to a file, and `-o` naming a link to it.
    symlink("/proc/self/fd/1", setup.path("stdout")).unwrap();
    let mut command = setup.command(
        "bob.worgl",
        PASSPHRASE,
        &["request", &mint, "5", "-o", "stdout"],
    );
    command.stdout(fs::File::create(setup.path("real.json")).unwrap());
    let status = tokio::task::spawn_blocking(move || command.status())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    assert_eq!(request("real.json").amount(), 5);
    assert!(is_link("stdout"));

    // A link to a file: the file is written, and the link stays.
    fs::write(setup.path("target.json"), "old\n").unwrap();
    symlink("target.json", setup.path("link.json")).unwrap();
    setup
        .ok("bob.worgl", &["request", &mint, "3", "-o", "link.json"])
        .await;
    assert!(is_link("link.json"));
    assert_eq!(request("target.json").amount(), 3);

    // A file replaced keeps its permissions.
    fs::set_permissions(setup.path("target.json"), fs::Permissions::from_mode(0o640)).unwrap();
    setup
        .ok("bob.worgl", &["request", &mint, "6", "-o", "target.json"])
        .await;
    assert_eq!(request("target.json").amount(), 6);
    let mode = fs::metadata(setup.path("target.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o640);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settling_fails_while_a_refused_transfer_is_kept() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;
    setup
        .ok("bob.worgl", &["request", &mint, "3", "-o", "request.json"])
        .await;
    let pay = ["pay", "request.json", "--spacing", "0..0"];
    setup.ok("alice.worgl", &pay).await;
    let settle = ["settle", "--spacing", "0..0"];
    let ids = transfer_ids(&setup.ok("alice.worgl", &["outgoing"]).await);

    // A server that is not the mint's proves nothing: kept, not retried.
    let other = Setup::start().await;
    let alice = setup.path("alice.worgl");
    let lost = other.worgl(alice.to_str().unwrap(), &settle).await;
    assert!(!lost.ok);
    assert!(
        lost.stderr
            .contains(&format!("transfer {} refused: the mint refused: ", ids[0])),
        "{}",
        lost.stderr
    );
    assert!(lost.stderr.contains("unknown_mint"), "{}", lost.stderr);
    assert!(
        lost.stderr
            .contains("1 transfer(s) refused and kept outgoing"),
        "{}",
        lost.stderr
    );
    assert!(!lost.stderr.contains("try again later"), "{}", lost.stderr);

    // A paused mint may pass later.
    let client = Client::new(&setup.url).unwrap();
    client.pause(&setup.mint, &setup.token).await.unwrap();
    let paused = setup.worgl("alice.worgl", &settle).await;
    assert!(!paused.ok);
    assert!(
        paused
            .stderr
            .contains("1 transfer(s) did not go through; try again later"),
        "{}",
        paused.stderr
    );
    client.resume(&setup.mint, &setup.token).await.unwrap();
    let settled = setup.ok("alice.worgl", &settle).await;
    assert!(settled.stderr.contains("Took in 5"), "{}", settled.stderr);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_payees_notes_from_a_settle_are_exported_once() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;
    setup
        .ok("bob.worgl", &["request", &mint, "5", "-o", "request.json"])
        .await;
    let pay = ["pay", "request.json", "--spacing", "0..0"];
    let id = paid_id(&setup.ok("alice.worgl", &pay).await);

    // Bob never submits; Alice's settle carries the payment out.
    let settled = setup
        .ok("alice.worgl", &["settle", "--spacing", "0..0"])
        .await;
    assert!(settled.stderr.contains("Took in 3"), "{}", settled.stderr);
    assert!(
        settled.stderr.contains("Kept 2 note(s) for payees"),
        "{}",
        settled.stderr
    );
    let outgoing = setup.ok("alice.worgl", &["outgoing"]).await.stdout;
    assert!(
        outgoing.contains(&format!(
            "  payment {id}  pays 5, 0 of 1 transfer(s) outgoing, 2 note(s) worth 5 kept for \
             the payee"
        )),
        "{outgoing}"
    );

    let exported = setup
        .ok(
            "alice.worgl",
            &["export", &id, "--notes", "-o", "notes.json"],
        )
        .await;
    assert!(
        exported.stderr.contains(&format!(
            "Wrote 2 note(s) worth 5 for the payee of payment {id}"
        )),
        "{}",
        exported.stderr
    );
    let accepted = setup.ok("bob.worgl", &["accept", "notes.json"]).await;
    assert_eq!(accepted.stderr.trim(), "Kept 5.");
    let again = setup
        .worgl("alice.worgl", &["export", &id, "--notes"])
        .await;
    assert!(!again.ok);
    assert!(
        again.stderr.contains("no payment kept has the id"),
        "{}",
        again.stderr
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paying_says_when_to_settle_by() {
    let setup = Setup::with(Policy::cash_like(), Some(86_400)).await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;
    setup
        .ok("bob.worgl", &["request", &mint, "3", "-o", "request.json"])
        .await;
    let paid = setup
        .ok(
            "alice.worgl",
            &["pay", "request.json", "-o", "payment.json"],
        )
        .await;
    // Minted second to last, the note dies at the third rotation from now.
    assert!(
        paid.stderr
            .contains("before the rotation that drops a note the payment spends"),
        "{}",
        paid.stderr
    );
    let line = paid
        .stderr
        .lines()
        .find(|l| l.starts_with("Settle within "))
        .unwrap();
    let days: u64 = line["Settle within ".len()..]
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=2).contains(&days), "{line}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshing_resends_a_payment_before_its_note_dies() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.ok("bob.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;
    setup.rotate().await;
    setup.rotate().await;
    setup
        .ok("bob.worgl", &["request", &mint, "3", "-o", "request.json"])
        .await;
    let pay = [
        "pay",
        "request.json",
        "--spacing",
        "0..0",
        "-o",
        "payment.json",
    ];
    setup.ok("alice.worgl", &pay).await;

    // Bob has not submitted it yet, and the note it spends dies at the next
    // rotation: the refresh sends it.
    let refreshed = setup.ok("alice.worgl", &["refresh", &mint, "--now"]).await;
    assert!(
        refreshed.stderr.contains("Refreshed 5 in 1 transfer(s)"),
        "{}",
        refreshed.stderr
    );
    assert!(
        refreshed.stderr.contains(
            "Kept 2 note(s) for payees from the mint's answers, in case a payee has not \
             submitted its payment"
        ),
        "{}",
        refreshed.stderr
    );
    assert!(setup.balance("alice.worgl").await.contains("sending    0"));
    setup.rotate().await;
    let received = setup
        .ok("bob.worgl", &["receive", "request.json", "payment.json"])
        .await;
    assert!(
        received.stderr.contains("Received 3"),
        "{}",
        received.stderr
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshing_keeps_notes_alive() {
    let setup = Setup::start().await;
    let mint = setup.mint.to_string();
    setup.ok("alice.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 4).await;
    setup.rotate().await;
    setup.rotate().await;
    assert!(setup.balance("alice.worgl").await.contains("expiring   4"));

    let refreshed = setup.ok("alice.worgl", &["refresh", &mint, "--now"]).await;
    assert!(
        refreshed.stderr.contains("Refreshed 4"),
        "{}",
        refreshed.stderr
    );
    let nothing = setup.ok("alice.worgl", &["refresh", &mint]).await;
    assert!(
        nothing.stderr.contains("No note dies"),
        "{}",
        nothing.stderr
    );

    // The refreshed note lives through the rotation, and is due again after.
    setup.rotate().await;
    let balance = setup.balance("alice.worgl").await;
    assert!(balance.contains("spendable  4"), "{balance}");
    assert!(balance.contains("expiring   4"), "{balance}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_purse_is_encrypted() {
    let setup = Setup::start().await;
    setup.ok("alice.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;

    let saved = fs::read_to_string(setup.path("alice.worgl")).unwrap();
    let sealed: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(sealed["format"], "worgl-purse");
    assert_eq!(sealed["kdf"]["algorithm"], "argon2id");
    assert!(!saved.contains("\"notes\""), "{saved}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(setup.path("alice.worgl"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let wrong = setup
        .worgl_with(
            "alice.worgl",
            "wrong",
            &["balance", &setup.mint.to_string()],
        )
        .await;
    assert!(!wrong.ok);
    assert!(
        wrong.stderr.contains("wrong passphrase"),
        "{}",
        wrong.stderr
    );

    // Weakening the key derivation in the file is caught too.
    let weakened = saved.replace("\"iterations\": 3", "\"iterations\": 1");
    assert_ne!(weakened, saved);
    fs::write(setup.path("weak.worgl"), weakened).unwrap();
    let weak = setup
        .worgl("weak.worgl", &["balance", &setup.mint.to_string()])
        .await;
    assert!(weak.stderr.contains("wrong passphrase"), "{}", weak.stderr);

    let again = setup.worgl("alice.worgl", &["init"]).await;
    assert!(again.stderr.contains("already exists"), "{}", again.stderr);
    let missing = setup.worgl("nobody.worgl", &["notes"]).await;
    assert!(missing.stderr.contains("worgl init"), "{}", missing.stderr);
    let empty = setup.worgl_with("empty.worgl", "", &["init"]).await;
    assert!(!empty.ok);
    assert!(
        empty.stderr.contains("the passphrase is empty"),
        "{}",
        empty.stderr
    );
    assert!(!setup.path("empty.worgl").exists());
}

/// A purse `worgl` 0.1 wrote, under the test passphrase: notes of 8 and 4,
/// and the request below expected, all of a mint no test serves.
const WRITTEN_BY_0_1: &str = include_str!("../../worgl-sdk/tests/fixtures/purse-v1.worgl");
const ITS_MINT: &str = "eefdebef624c7eca2dc7010746dfcb64a7e85459dfe31dd0068da35d208d3104";
const ITS_REQUEST: &str = r#"{
  "mint": "eefdebef624c7eca2dc7010746dfcb64a7e85459dfe31dd0068da35d208d3104",
  "outputs": [
    {"amount": 4, "holder": "83be289f23271b938ab19cffd9c441f288942d07c648ecebdb08cc564ef7d390"},
    {"amount": 1, "holder": "71df4b2c94d1645c5a75b210a5fa934db5bc2073b78859a056f02b1593a54ecc"}
  ]
}"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_purse_worgl_0_1_wrote_opens_and_saves() {
    let setup = Setup::start().await;
    fs::write(setup.path("old.worgl"), WRITTEN_BY_0_1).unwrap();
    fs::write(setup.path("request.json"), ITS_REQUEST).unwrap();
    let run = setup.ok("old.worgl", &["withdraw", "request.json"]).await;
    assert!(
        run.stderr.contains("No longer expecting 5"),
        "{}",
        run.stderr
    );

    let saved = fs::read_to_string(setup.path("old.worgl")).unwrap();
    assert_ne!(saved, WRITTEN_BY_0_1);
    let (_, opened) = Vault::open(&saved, PASSPHRASE).unwrap();
    assert!(opened.mints.is_empty());
    let view = MintView {
        issuer: ITS_MINT.parse().unwrap(),
        layers: 4,
        oldest: 0,
        policy: Policy::cash_like(),
        schedule: None,
        paused: false,
    };
    let balance = opened.holder.balance(&view);
    assert_eq!((balance.spendable, balance.requested), (12, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mint_list_in_the_purse_is_kept() {
    let setup = Setup::start().await;
    setup.ok("alice.worgl", &["init"]).await;
    setup.mint_into("alice.worgl", 8).await;
    // Restored into a new purse and sealed again with the server of its
    // mint and a name for it, as the TypeScript SDK seals a backup.
    let saved = fs::read_to_string(setup.path("alice.worgl")).unwrap();
    let (vault, opened) = Vault::open(&saved, PASSPHRASE).unwrap();
    assert!(opened.mints.is_empty());
    let mut restored = Holder::new();
    restored.merge(&opened.holder);
    let mut entry = MintEntry::new(setup.mint, &setup.url);
    entry.extra.insert("name".into(), "Playground".into());
    let backup = vault.seal(&restored, std::slice::from_ref(&entry)).unwrap();
    fs::write(setup.path("alice.worgl"), backup).unwrap();

    let mint = setup.mint.to_string();
    setup
        .ok(
            "alice.worgl",
            &["request", &mint, "5", "-o", "request.json"],
        )
        .await;
    let balance = setup.balance("alice.worgl").await;
    assert!(balance.contains("spendable  8\n"), "{balance}");
    assert!(balance.contains("unchecked  8 "), "{balance}");
    assert!(balance.contains("requested  5 "), "{balance}");
    let saved = fs::read_to_string(setup.path("alice.worgl")).unwrap();
    let (_, reopened) = Vault::open(&saved, PASSPHRASE).unwrap();
    assert_eq!(reopened.mints, [entry]);
    assert_eq!(
        reopened.holder.notes().count(),
        opened.holder.notes().count()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_carry_the_mint_code() {
    let setup = Setup::start().await;
    setup.ok("alice.worgl", &["init"]).await;
    let other = worgl::IssuerKey::generate().public_key().to_string();
    let run = setup.worgl("alice.worgl", &["request", &other, "5"]).await;
    assert!(!run.ok);
    assert!(run.stderr.contains("unknown_mint"), "{}", run.stderr);
    let bad = setup
        .worgl("alice.worgl", &["pay", "-", "--spacing", "9..1"])
        .await;
    assert!(bad.stderr.contains("9 is above 1"), "{}", bad.stderr);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_playground_faucet_fills_a_purse() {
    let setup = Setup::start().await;
    let faucet = setup.faucet().await;
    setup.ok("alice.worgl", &["init"]).await;
    let run = setup
        .ok("alice.worgl", &["playground", "--faucet", &faucet])
        .await;
    assert_eq!(run.stdout.trim(), setup.mint.to_string());
    assert!(run.stderr.contains("Kept 64"), "{}", run.stderr);
    assert!(
        setup
            .balance("alice.worgl")
            .await
            .contains("spendable  64\n")
    );

    // Without --faucet it asks the server's URL, which here has none.
    let run = setup.worgl("alice.worgl", &["playground"]).await;
    assert!(!run.ok);
    assert!(run.stderr.contains("asking the faucet"), "{}", run.stderr);
    assert!(setup.balance("alice.worgl").await.contains("requested  0 "));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_at_once_on_one_purse_keep_every_note() {
    let setup = Setup::start().await;
    let faucet = setup.faucet().await;
    setup.ok("alice.worgl", &["init"]).await;
    let args = ["playground", "--faucet", &faucet];
    tokio::join!(
        setup.ok("alice.worgl", &args),
        setup.ok("alice.worgl", &args),
        setup.ok("alice.worgl", &args),
        setup.ok("alice.worgl", &args),
    );
    let balance = setup.balance("alice.worgl").await;
    assert!(balance.contains("spendable  256\n"), "{balance}");
}
