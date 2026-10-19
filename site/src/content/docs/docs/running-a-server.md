---
title: Running a server
description: Create a state directory and its mints, serve them over HTTP, change their rules and mint notes.
---

`worgl-server` hosts any number of mints in one process. Each mint is one
issuer, with its own key, ledger, policy, rotation period and directory.

## Build

Nothing is on crates.io yet. Build from the repository:

```sh
cargo build --release
./target/release/worgl-server init        --state ./server
./target/release/worgl-server create-mint --state ./server --layers 4 --layer-size 1048576 --rotate-every 86400
./target/release/worgl-server serve       --state ./server --listen 127.0.0.1:8080
```

Or install it from git with
`cargo install --git https://github.com/zhangheqi/worgl worgl-server --locked`.

## Commands

| Command | What it does |
|---|---|
| `init --state DIR` | Creates a server's state directory: an operator token and no mints. |
| `create-mint --state DIR` | Creates a mint. `--layers` (default 4), `--layer-size` (default 1,048,576, a multiple of 64), `--policy FILE` and `--rotate-every SECS`. A running server sees it once restarted. |
| `serve --state DIR` | Serves every mint in the directory. `--listen` (default `127.0.0.1:8080`) and `--compact-every N`, which writes a fresh snapshot of a mint after N logged events (default 100,000). |
| `sign-mint` | Signs a mint request and prints it as JSON. See [Mint authorities and keys](../keys/). |
| `export-log --mint DIR` | Prints a mint's log for an audit. See [Exporting a log for audit](../audit/). |
| `join-cluster` | Makes a state directory a cluster member's. See [Running a cluster](../running-a-cluster/). |

`--state` can also come from `WORGL_STATE`, and `--listen` from
`WORGL_LISTEN`. Logging follows `RUST_LOG`, at `info` by default.

## The state directory

`init` writes a random 64-character operator token to `operator.token`,
which creates and lists mints, and `kek.key` (mode 0600), the
key-encryption key that seals each mint's new generation keys into their
rotations' log entries.

`create-mint` adds `mints/<issuer key>/` and prints the mint's issuer key.
In it:

| File | What it holds |
|---|---|
| `issuer.key` | The mint's issuer key, its identity. Mode 0600. |
| `admin.token` | The mint's own admin token, which rotates and pauses this mint only. Mode 0600. |
| `mint.key` | A mint authority key, made when the policy lists none. Mode 0600. The server never reads it: move it to wherever you mint from. |
| `policy.json` | The policy, with the mint key listed in `mint_authorities`. |
| `mint.json` | The rotation period. |
| `snapshot.bin`, `log.bin` | The ledger: a snapshot and every change since. |
| `keys.bin` | A generation key for each layer. Mode 0600. |
| `lock` | Held while a process has the mint open, so a second one cannot. |

A mint is built in a hidden directory and renamed into place when complete,
so a crash never leaves one half made. The server refuses to start if any
mint fails to open, if `kek.key` is missing, or if a token is shorter than
32 characters.

### State from earlier builds

A state directory that only builds from before the key-encryption key made
and served has no `kek.key`, and the server refuses it with
`…/kek.key is missing`.
Nothing in such a directory is sealed, so a new key opens it. Give it one,
as 64 hex digits readable by the server's user only:

```sh
(umask 077; openssl rand -hex 32 > ./server/kek.key)
```

Do this only for a single server's directory that never had a `kek.key`.
A directory that lost its key needs the old one back from a backup, and a
cluster member's must match the other members' (see
[Backups](../backups/)).

With a `kek.key`, state written by an earlier build of 0.1 still opens:

- A store from before the log, a snapshot plus a journal, converts to the
  log the first time it is opened.
- An issuer restored from a snapshot written before serial indexes were
  scrambled keeps using bits as indexes, because its notes already carry
  them.
- An issuer from before generation keys keeps its existing generations
  signed by the issuer key until those generations are dropped.

## Changing the rules

Edit a mint's `policy.json` and restart to change its rules. Missing fields
take the defaults of the `cash_like` preset:

```json
{ "max_inputs": 16, "denominations": "any" }
```

Every field is in [Policy fields](../policy/). Input amounts are not checked
against the denominations, so notes issued under an earlier policy still
spend.

## Rotation

With `--rotate-every`, a mint rotates at multiples of its period since the
Unix epoch, so restarts do not shift it. Without it, the mint rotates only
when its admin asks:

```sh
curl -X POST -H "Authorization: Bearer $(cat server/mints/<mint>/admin.token)" \
  http://127.0.0.1:8080/v1/mints/<mint>/rotate
```

A scheduled rotation logs the boundary it was due at, and the mint refuses
one that is not due after the last, so neither a wall clock that steps
backwards nor a restart can rotate twice in one period. Boundaries missed
while the server is down, or while the mint is paused, are not made up. See
[Choosing a period and layer count](../choosing-a-period/).

## Creating mints while serving

A running server takes new mints through `POST /v1/admin/mints` with the
operator token:

```sh
curl -X POST -H "Authorization: Bearer $(cat server/operator.token)" \
  -H 'content-type: application/json' \
  -d '{"ledger": {"layers": 4, "layer_size": 1048576}, "policy": {"mint_authorities": ["<public key>"]}, "rotate_every": 86400}' \
  http://127.0.0.1:8080/v1/admin/mints
```

Its policy must list a mint authority, since the server never holds one. The
answer carries the new mint's issuer key and admin token as
`{"mint", "token"}`.

## Minting

Sign a request with the mint key and post it to the mint:

```sh
./target/release/worgl-server sign-mint --key ./server/mints/<mint>/mint.key --issuer <mint> \
    --sequence 1 --amount 8 --holder <holder public key> \
  | curl -sd @- -H 'content-type: application/json' http://127.0.0.1:8080/v1/mints/<mint>/mint
```

`worgl mint` does the same from a purse; see
[The worgl command](../command-line/).

## In production

- The server speaks plain HTTP. Put it behind a reverse proxy for TLS and
  rate limiting.
- Each mint commits on its own, so a busy mint does not slow the others, and
  an idle one holds no thread.
- Every change is written and flushed to the log before it is answered. One
  flush covers every request waiting at that moment, up to 256.
- Request bodies are limited to 64 KiB.
- Back up the state directory, `keys.bin` included. See
  [Backups](../backups/).
- Three servers can run as a cluster, so each mint carries on when one
  fails. See [Running a cluster](../running-a-cluster/).
