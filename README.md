# Worgl

Worgl is an open-source issuer for tokens that cost one bit to track, need
no accounts, and expire if they sit still.

The name comes from Wörgl, the Austrian town whose 1932 stamp scrip cost its
holders 1% a month to keep valid, so nobody sat on it. Notes here likewise
expire if they sit still.

An issuer signs notes with Ed25519 and keeps a single bit per note in a
layered bit array: 1 while the note is live, 0 once it is spent. A transfer
clears the input notes' bits and issues new notes to the receivers, so no note
can be spent twice. There are no accounts. Each note is bound to a fresh
one-time key of whoever receives it. The issuer stores no notes: only the
bits, a few counters, and a bounded list of recent transfers so that a client
can safely resend a request whose answer it lost.

It is for the credits, quotas and points that services and agents hand each
other. Identity checks and regulation are out of scope.

> **Status:** 0.1. The design and code have not had an independent security
> review. Do not use it to hold anything you cannot afford to lose.

## Documentation

The docs live in [`site/src/content/docs/docs`](site/src/content/docs/docs),
the source of the website's docs pages.

- **Start:** [Quickstart](site/src/content/docs/docs/quickstart.md) ·
  [Concepts](site/src/content/docs/docs/concepts.md) ·
  [Choosing a period and layer count](site/src/content/docs/docs/choosing-a-period.md)
- **Build:** [Rust SDK](site/src/content/docs/docs/rust-sdk.mdx) ·
  [TypeScript SDK](site/src/content/docs/docs/typescript-sdk.md) ·
  [The worgl command](site/src/content/docs/docs/command-line.md) ·
  [Embedding the library](site/src/content/docs/docs/embedding.mdx)
- **Operate:** [Running a server](site/src/content/docs/docs/running-a-server.md) ·
  [Running a cluster](site/src/content/docs/docs/running-a-cluster.md) ·
  [Mint authorities and keys](site/src/content/docs/docs/keys.md) ·
  [Pausing a mint](site/src/content/docs/docs/pausing.md) ·
  [Backups](site/src/content/docs/docs/backups.md) ·
  [Exporting a log for audit](site/src/content/docs/docs/audit.md)
- **Reference:** [HTTP API](site/src/content/docs/docs/http-api.md) ·
  [Error codes](site/src/content/docs/docs/error-codes.md) ·
  [Note format](site/src/content/docs/docs/note-format.md) ·
  [Policy fields](site/src/content/docs/docs/policy.md) ·
  [Changelog](CHANGELOG.md)
- [Privacy model](site/src/content/docs/docs/privacy.md)

## The workspace

Seven crates and a TypeScript package. Nothing is published on crates.io or
npm yet; depend on them from git.

- [`worgl`](crates/worgl): the library. Ledger, note format, transfer
  protocol, policy, issuer state machine and crash-safe storage.
- [`worgl-server`](crates/worgl-server): an HTTP server built on it that
  hosts many issuers, called mints, side by side.
- [`worgl-raft`](crates/worgl-raft): Raft replication for one mint, so a
  group of replicas keeps its log and any of them can take over as leader,
  and a registry group that lists a cluster's mints. The server runs on it
  as a cluster.
- [`worgl-sdk`](crates/worgl-sdk): the holder's side. A note store that
  requests, pays, receives and refreshes notes while keeping the privacy
  rules, with no networking of its own.
- [`worgl-client`](crates/worgl-client): talks to the server. Typed calls
  for every route, and a purse that runs a holder over HTTP and saves it.
- [`worgl-cli`](crates/worgl-cli): `worgl`, a command line purse on
  `worgl-client`, kept in a passphrase-encrypted file.
- [`worgl-wasm`](crates/worgl-wasm): `worgl-sdk` compiled to WebAssembly.
- [`sdk/typescript`](sdk/typescript): the TypeScript SDK, a client and purse
  like `worgl-client`'s on top of `worgl-wasm`, for Node and browsers.

## Try it

```sh
cargo build --release
./target/release/worgl-server init        --state ./server
./target/release/worgl-server create-mint --state ./server --layers 4 --layer-size 1048576 --rotate-every 86400
./target/release/worgl-server serve       --state ./server --listen 127.0.0.1:8080
```

The [quickstart](site/src/content/docs/docs/quickstart.md) goes on from
there: two purses, a mint, a payment and a rotation, with the `worgl`
command.

## Privacy, precisely

The issuer learns no identities and keeps no accounts. It does see each
transfer's inputs die and its outputs appear, so it can follow the lineage of
every note. What it cannot do is:

- tell who holds a note, or
- tell that two unrelated lineages share an owner,

provided holders use a fresh key for every note they receive and never spend
two notes in one transfer. The default policy enforces the second rule.
Spending several notes together would tell the issuer they had one owner.

What remains visible is timing: a payer who sends three notes to one payee in
the same second invites the guess that they are one payment. Spacing or
batching transfers blurs this.

This is weaker than blind-signature schemes (Chaum's, Cashu, GNU Taler),
where the issuer cannot link a note to the one it replaced, and stronger than
any account system. Blind signatures do not fit this design: the issuer must
choose each note's serial, because the serial names the note's bit.

What each party to a payment sees is in the
[privacy model](site/src/content/docs/docs/privacy.md).

## Efficiency

- **Storage:** one bit per note. Four layers of 2²⁰ bits take 512 KiB and
  hold over four million notes in flight.
- **Status reads and writes:** O(1). A serial maps straight to its bit: the
  layer's slot is `generation mod layers`, its position is
  `generation − oldest generation`, and the bit is the index run back through
  a ten-round keyed permutation, about 0.65 µs in a release build.
- **Rotation:** one word write per 64 bits the dropped layer used.
- **Per transfer:** two signature checks per input (issuer and holder) and one
  signature per output.
- **Durability:** one flush per batch of changes, not per change. On a 4-core
  cloud VM the server went from about 4,000 transfers/s, whatever the
  number of clients, to about 8,800 with 8 concurrent clients and 17,000
  with 64, where signature checks become the limit. The library alone
  commits about 41,000 transfers/s in batches of 256.

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

Three servers can run as a cluster that keeps every mint on all three, so each
mint carries on when one server fails. Each mint is a Raft group of the
members, built on [`worgl-raft`](crates/worgl-raft), and a change is answered
once two of the three hold it on disk. When a leader fails, the others elect
a new one within a few seconds; until then, requests are answered
`503 no_leader`, and sending the same request again is safe. A leader cut off
from the other members stops taking requests after two seconds without
hearing from them, and a member that cannot reach the leader says so within a
few seconds.

How to start one, create mints on it and what its limits are:
[Running a cluster](site/src/content/docs/docs/running-a-cluster.md).

## Not covered yet

- **The issuer sees lineage.** Inherent to the design; see above.
- **No backing.** A mint authority decides what to mint; there is no deposit
  or redemption into anything else.
- **Clusters are fixed and their members talk in the clear.** Members cannot
  be added or removed, and the Raft port has no TLS. The issuer key itself
  never rotates: a new one is a new mint.
- **No SDK for other languages yet.** Rust and TypeScript have one, and
  the `worgl` command covers scripts; others call the HTTP API directly.

## Development

```sh
cargo test --workspace          # unit, property and HTTP tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

`cargo test -p worgl-raft --test simulation` runs a mint's three-replica group
through seeded faults on a simulated clock: replicas cut off, split apart,
crashed and restarted, messages lost and delayed. Afterwards it checks that
the replicas agree, that every answered change is there, that no note was
spent twice and that supply balances. It runs 32 seeds by default; set
`WORGL_SIM_SEEDS=1000` for more, or `WORGL_SIM_SEED=<n>` to repeat one.

The minimum supported Rust version is 1.89. CI runs all of the above plus the
docs and the example on every push and pull request, and the TypeScript SDK's
tests against a real server:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked  # the version Cargo.lock pins
cargo build -p worgl-server
cd sdk/typescript && npm ci && npm run build && npm run check && npm test
```

The website, docs included, is in [`site`](site); see its
[README](site/README.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
