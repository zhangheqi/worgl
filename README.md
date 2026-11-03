# Worgl

Worgl is an open-source issuer for tokens that cost one bit to track, need
no accounts, and expire if they sit still. It is meant for the credits,
quotas and points that services and agents hand each other. Identity checks
and regulation are out of scope.

An issuer signs notes with Ed25519 and keeps one bit per note in a layered
bit array: 1 while the note is live, 0 once it is spent. A transfer clears
the bits of the notes it spends and issues new notes to the receivers, so no
note can be spent twice. There are no accounts: each note is bound to a fresh
one-time key of whoever receives it. At every rotation the issuer drops its
oldest layer, and a note nobody moves dies within as many rotations as there
are layers.

The issuer stores no notes. It keeps the bits, a few counters, and a bounded
list of recent transfers so that a client can safely resend a request whose
answer it lost.

The name comes from Wörgl, the Austrian town whose 1932 stamp scrip cost its
holders 1% a month to keep valid.

> **Status:** 0.1. The design and code have not had an independent security
> review. Do not use it to hold anything you cannot afford to lose.

## Try it

```sh
cargo build --release
./target/release/worgl-server init        --state ./server
./target/release/worgl-server create-mint --state ./server --layers 4 --layer-size 1048576 --rotate-every 86400
./target/release/worgl-server serve       --state ./server --listen 127.0.0.1:8080
```

The [quickstart](site/src/content/docs/docs/quickstart.md) continues from
there with the `worgl` command: two purses, a mint, a payment and a rotation.

## Documentation

The documentation is published at <https://zhangheqi.github.io/worgl/docs/>.
Its source is in [`site/src/content/docs/docs`](site/src/content/docs/docs),
and the links below point there.

- **Start:** [Quickstart](site/src/content/docs/docs/quickstart.md) ·
  [Concepts](site/src/content/docs/docs/concepts.md) ·
  [Choosing a period and layer count](site/src/content/docs/docs/choosing-a-period.md)
- **Build:** [Rust SDK](site/src/content/docs/docs/rust-sdk.mdx) ·
  [TypeScript SDK](site/src/content/docs/docs/typescript-sdk.md) ·
  [The worgl command](site/src/content/docs/docs/command-line.md) ·
  [Embedding the library](site/src/content/docs/docs/embedding.mdx)
- **Operate:** [Running a server](site/src/content/docs/docs/running-a-server.md) ·
  [Running a cluster](site/src/content/docs/docs/running-a-cluster.md) ·
  [Running a faucet](site/src/content/docs/docs/running-a-faucet.md) ·
  [Mint authorities and keys](site/src/content/docs/docs/keys.md) ·
  [Pausing a mint](site/src/content/docs/docs/pausing.md) ·
  [Backups](site/src/content/docs/docs/backups.md) ·
  [Exporting a log for audit](site/src/content/docs/docs/audit.md)
- **Reference:** [HTTP API](site/src/content/docs/docs/http-api.md) ·
  [Error codes](site/src/content/docs/docs/error-codes.md) ·
  [Note format](site/src/content/docs/docs/note-format.md) ·
  [Policy fields](site/src/content/docs/docs/policy.md) ·
  [Privacy model](site/src/content/docs/docs/privacy.md) ·
  [Changelog](CHANGELOG.md)

## The workspace

Eight crates and a TypeScript package. All crates except `worgl-faucet` and
`worgl-wasm` are on crates.io. The TypeScript package is not on npm yet.

- [`worgl`](crates/worgl): the library. Ledger, note format, transfer
  protocol, policy, issuer state machine and crash-safe storage.
- [`worgl-server`](crates/worgl-server): an HTTP server that hosts many
  issuers, called mints, side by side.
- [`worgl-raft`](crates/worgl-raft): Raft replication. A group of replicas
  keeps each mint's log, and any of them can take over as leader; a registry
  group lists the cluster's mints. `worgl-server` uses it to run as a
  cluster.
- [`worgl-sdk`](crates/worgl-sdk): the holder's side. A note store that
  requests, pays, receives and refreshes notes and follows the privacy rules.
  It does no networking itself.
- [`worgl-client`](crates/worgl-client): an HTTP client for the server, with
  a typed call for every route and a purse that runs a holder over HTTP and
  saves it.
- [`worgl-cli`](crates/worgl-cli): the `worgl` command, a command-line purse
  built on `worgl-client` and stored in a passphrase-encrypted file.
- [`worgl-faucet`](crates/worgl-faucet): gives out notes from a playground
  mint to anyone who asks, with limits per address and per day.
- [`worgl-wasm`](crates/worgl-wasm): `worgl-sdk` compiled to WebAssembly.
- [`sdk/typescript`](sdk/typescript): the TypeScript SDK for Node and
  browsers, a client and purse like `worgl-client`'s built on `worgl-wasm`.

## Privacy

The issuer learns no identities and keeps no accounts. It does see each
transfer's inputs die and its outputs appear, so it can follow every note's
lineage. It cannot tell who holds a note, or that two unrelated lineages
share an owner, as long as holders follow two rules:

- Use a fresh key for every note received.
- Never spend two notes in one transfer, since that tells the issuer they had
  one owner. The default policy enforces this rule.

Timing is still visible. A payer who sends three notes to one payee in the
same second invites the guess that they are one payment. Spacing or batching
transfers makes this harder.

This is weaker than blind-signature schemes such as Chaum's, Cashu and GNU
Taler, where the issuer cannot link a note to the one it replaced, and
stronger than any account system. Blind signatures do not fit this design,
because the issuer must choose each note's serial: the serial names the
note's bit.

The [privacy model](site/src/content/docs/docs/privacy.md) describes what
each party to a payment sees.

## Efficiency

- **Storage:** one bit per note. Four layers of 2²⁰ bits take 512 KiB and
  hold over four million notes in flight.
- **Status reads and writes:** O(1). A serial maps directly to its bit: the
  layer's slot is `generation mod layers`, its position is
  `generation − oldest generation`, and the bit is the index run back through
  a ten-round keyed permutation. This takes about 0.65 µs in a release build.
- **Rotation:** one word write per 64 bits the dropped layer used.
- **Per transfer:** two signature checks per input (issuer and holder) and one
  signature per output.
- **Durability:** one flush per batch of changes, not per change. On a 4-core
  cloud VM this took the server from about 4,000 transfers/s, whatever the
  number of clients, to about 8,800 with 8 concurrent clients and 17,000 with
  64, where signature checks become the limit. The library alone commits
  about 41,000 transfers/s in batches of 256.
- **Rate per mint:** a mint keeps each answer for at least 128 seconds so
  that clients can resend, so it sustains at most
  `receipt_capacity / receipt_min_age_secs` transfers and burns a second,
  about 8,192 at the defaults. Bursts of up to 1,048,576 are not held back,
  except for up to 128 seconds after a restart if the mint already holds that
  many answers. The limit applies to each mint on its own, not to a server's
  mints together.

## How the design maps onto the code

| Design point | Where it lives |
|---|---|
| Centralized, no accounts | One [`Issuer`](crates/worgl/src/issuer.rs). It keeps no accounts and stores no notes; holders keep their own notes. |
| Asymmetric signatures issue notes | Ed25519 via `ed25519-dalek`, strict verification ([`keys.rs`](crates/worgl/src/keys.rs)); each generation signs with its own key, certified by the issuer key ([`keyset.rs`](crates/worgl/src/keyset.rs)). |
| Serial numbers | [`Serial`](crates/worgl/src/ledger.rs) is `generation:index`; the index is a keyed permutation of the note's bit ([`permutation.rs`](crates/worgl/src/permutation.rs)). |
| Bit array of valid notes | [`Ledger`](crates/worgl/src/ledger.rs): one flat array, slot-major, `u64` words. |
| Transfer invalidates and reissues | [`Issuer::plan_transfer`](crates/worgl/src/issuer.rs) and `apply`. |
| Last-used index | Each layer counts the bits it has handed out; issuing sets the next one. |
| Layers and rotation | Notes are reissued one layer on; the last layer cannot transfer; rotation drops the first layer, wipes its slot in place and reuses it as the new last layer. |
| Expiry by neglect | A note dies in the rotation that drops its layer. |

## Running a cluster

Three servers can run as a cluster that keeps every mint on all three, so
each mint keeps working when one server fails. Each mint is a Raft group of
the members, built on [`worgl-raft`](crates/worgl-raft).

- A change is answered once two of the three members hold it on disk.
- When a leader fails, the others elect a new one within a few seconds. Until
  then, requests are answered `503 no_leader`, and sending the same request
  again is safe.
- A leader cut off from the other members stops taking requests after two
  seconds without hearing from them. A member that cannot reach the leader
  says so within a few seconds.
- Clients need one address that covers all three members, such as a proxy in
  front of them.
- Members talk to each other in plain HTTP, tokens included, so they need a
  private network between them.
- A mint's policy and rotation period are not replicated. Each member reads
  its own files, and the member leading the mint applies its own, so change
  them the same way on every member while the mint is paused.

[Running a cluster](site/src/content/docs/docs/running-a-cluster.md) covers
starting a cluster, keeping it private, pointing clients at it, creating
mints, changing their rules, and its limits.

## Limitations

- **The issuer sees lineage.** This follows from the design; see
  [Privacy](#privacy).
- **No backing.** A mint authority decides what to mint. Notes cannot be
  deposited for, or redeemed into, anything else.
- **Clusters are fixed and unencrypted.** Members cannot be added or removed,
  and they send each other Raft messages and client requests, tokens
  included, without TLS.
- **Issuer keys do not rotate.** A new issuer key means a new mint.
- **SDKs for Rust and TypeScript only.** Scripts can use the `worgl` command;
  other languages call the HTTP API directly.

## Development

```sh
cargo test --workspace          # unit, property and HTTP tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

`cargo test -p worgl-raft --test simulation` runs a mint's three-replica group
through seeded faults on a simulated clock: replicas cut off, split apart,
crashed and restarted, and messages lost and delayed. It then checks that the
replicas agree, that every answered change is there, that no note was spent
twice and that supply balances. It runs 32 seeds by default. Set
`WORGL_SIM_SEEDS=1000` to run more, or `WORGL_SIM_SEED=<n>` to repeat one.

The minimum supported Rust version is 1.89. On every push and pull request,
CI runs all of the above, builds the docs, runs the example, and runs the
TypeScript SDK's tests against a real server. To run those tests locally:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked  # the version Cargo.lock pins
cargo build -p worgl-server -p worgl-faucet -p worgl-cli
cd sdk/typescript && npm ci && npm run build && npm run check && npm test
```

The website, including the docs, is in [`site`](site); see its
[README](site/README.md).

## Contributing

Commit messages and pull request titles follow
[Conventional Commits](https://www.conventionalcommits.org/).

Please report security issues privately through
[GitHub](https://github.com/zhangheqi/worgl/security/advisories/new), not in a
public issue.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
