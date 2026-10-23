//! What the faucet's command line refuses before it starts.

use std::process::Command;

use worgl::MintKey;

/// Runs `worgl-faucet` with `args` and the environment variables `env`,
/// expects it to stop before it reads its key, and returns what it printed.
fn refused(args: &[&str], env: &[(&str, &str)]) -> String {
    let mint = MintKey::generate().public_key().to_string();
    let output = Command::new(env!("CARGO_BIN_EXE_worgl-faucet"))
        .args(["--server", "http://127.0.0.1:1", "--mint", &mint])
        .args(["--key", "no such file"])
        .args(args)
        .env_remove("WORGL_FAUCET_ALLOW_ORIGIN")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let printed = String::from_utf8(output.stderr).unwrap();
    // 2 is clap's code for arguments it refuses.
    assert_eq!(output.status.code(), Some(2), "{printed}");
    printed
}

#[test]
fn any_origin_is_refused() {
    for printed in [
        refused(&["--allow-origin", "*"], &[]),
        refused(&["--allow-origin", "https://purse.example,*"], &[]),
        refused(&[], &[("WORGL_FAUCET_ALLOW_ORIGIN", "*")]),
    ] {
        assert!(
            printed.contains("would let any website collect"),
            "{printed}"
        );
    }
    let printed = refused(&["--allow-origin", "null"], &[]);
    assert!(printed.contains("sandboxed frames"), "{printed}");
    // A pattern would match no page at all.
    let printed = refused(&["--allow-origin", "https://*.purse.example"], &[]);
    assert!(printed.contains("matches no page"), "{printed}");
}

#[test]
fn an_origin_with_a_path_is_refused() {
    for origin in ["https://purse.example/", "https://Purse.Example:443/purse"] {
        let printed = refused(&["--allow-origin", origin], &[]);
        assert!(printed.contains("no path"), "{printed}");
        assert!(printed.contains("give https://purse.example"), "{printed}");
    }
}
