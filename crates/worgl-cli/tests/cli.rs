//! The `worgl` binary against a real server on a local port.

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;

use worgl::{LedgerConfig, MintKey, Policy, PublicKey};
use worgl_client::Client;
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
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        init_state(&state).unwrap();
        let app = AppState::open(&state, 1000).unwrap();
        let authority = MintKey::generate();
        let mut policy = Policy::cash_like();
        policy.mint_authorities = vec![authority.public_key()];
        let created = app
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_worgl"));
        command
            .current_dir(self.dir.path())
            .env("WORGL_SERVER", &self.url)
            .env("WORGL_PURSE", purse)
            .env("WORGL_PASSPHRASE", passphrase)
            .args(args);
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
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = worgl_faucet::router(faucet).into_make_service_with_connect_info::<SocketAddr>();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
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
    assert!(alice.contains("outgoing   0"), "{alice}");

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
    let accepted = setup.ok("bob.worgl", &["accept", "notes.json"]).await;
    assert!(accepted.stderr.contains("Kept 6"), "{}", accepted.stderr);
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
    assert!(setup.balance("alice.worgl").await.contains("expected   0 "));
}
