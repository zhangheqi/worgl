# Worgl

Centralized, account-free e-cash where the issuer tracks every note with one
bit.

The name comes from Wörgl, the Austrian town whose 1932 stamp scrip cost its
holders 1% a month to keep valid, so nobody sat on it. Notes here likewise
expire if they sit still.

An issuer signs banknotes with Ed25519 and keeps a single bit per note in a
layered bit array: 1 while the note is live, 0 once it is spent. A transfer
clears the input notes' bits and issues new notes to the receivers, so no note
can be spent twice. There are no accounts. Each note is bound to a fresh
one-time key of whoever receives it. The issuer stores no notes: only the
bits, a few counters, and a bounded list of recent transfers so that a client
can safely resend a request whose answer it lost.

It is a primitive for autonomous systems that pay each other in credits,
quotas or any other unit, rather than a cash product. Identity checks and
regulation are out of scope.

The workspace has seven crates and a TypeScript package:

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

> **Status:** 0.1. The design and code have not had an independent security
> review. Do not use it to hold anything you cannot afford to lose.

## Privacy, precisely

The issuer learns no identities and keeps no accounts. It does see each
transfer's inputs die and its outputs appear, so it can follow the lineage of
every note. What it cannot do is:

- tell who holds a note, or
- tell that two unrelated lineages share an owner,

provided holders use a fresh key for every note they receive and never spend
two notes in one transfer. The default policy enforces the second rule.
Spending several notes together would tell the issuer they had one owner,
the same heuristic that links addresses on public blockchains.

What remains visible is timing: a payer who sends three notes to one payee in
the same second invites the guess that they are one payment. Spacing or
batching transfers blurs this.

This is weaker than blind-signature e-cash (Chaum, Cashu, GNU Taler), where
the issuer cannot link a note to the one it replaced, and stronger than any
account system. Blind signatures do not fit this design: the issuer must
choose each note's serial, because the serial names the note's bit.

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
| Centralized, no wallets | One [`Issuer`](crates/worgl/src/issuer.rs). It keeps no accounts and stores no notes; holders keep their own notes. |
| Asymmetric crypto issues notes | Ed25519 via `ed25519-dalek`, strict verification ([`keys.rs`](crates/worgl/src/keys.rs)); each generation signs with its own key, certified by the issuer key ([`keyset.rs`](crates/worgl/src/keyset.rs)). |
| Serial numbers | [`Serial`](crates/worgl/src/ledger.rs) is `generation:index`; the index is a keyed permutation of the note's bit ([`permutation.rs`](crates/worgl/src/permutation.rs)). |
| Bit array of valid notes | [`Ledger`](crates/worgl/src/ledger.rs): one flat array, slot-major, `u64` words. |
| Transfer invalidates and reissues | [`Issuer::plan_transfer`](crates/worgl/src/issuer.rs) and `apply`. |
| Last-used index | Each layer counts the bits it has handed out; issuing sets the next one. |
| Layers and rotation | Notes are reissued one layer on; the last layer cannot transfer; rotation drops the first layer, wipes its slot in place and reuses it as the new last layer. |
| Expiry by neglect | A note dies in the rotation that drops its layer. |

## Design choices

These are the places where the scheme's description left a choice open. Each
can be changed.

**Serials carry a generation that is never reused.** When a dropped layer's
slot is reused, an old note's serial names a generation that no longer exists,
so it can never match the fresh bit that sits where its bit used to be.

**Serial indexes are scrambled.** A layer hands out its bits in order, but a
note's index is its bit under a keyed permutation that differs in every
generation and is derived from the issuer key. A serial therefore does not
say whether a note was issued before or after another, and the outputs of
one split do not carry consecutive indexes. The generation still tells which
layer, and so roughly which rotation period, a note was issued in. An issuer
restored from a snapshot written before this change keeps using bits as
indexes, because its notes already carry them.

**Notes are 149 signed bytes.** Version, issuer key, serial, amount, holder
key and the issuer's signature. In text they are unpadded URL-safe base64. The
issuer stores no notes: the signature makes a note self-authenticating and the
bit array says whether it is live. See [`note.rs`](crates/worgl/src/note.rs).

**Each generation has its own signing key.** The issuer key is the issuer's
identity, which every note names, but it does not sign notes. At every
rotation the issuer makes a fresh key for the new last layer, certifies it
with the issuer key and signs that generation's notes with it. The key's
secret is erased once its generation is dropped. A leaked generation key
therefore forges notes for at most as many rotations as there are layers, and
a leak exposes no key made after it. The certificates of the live generations
are published at `/v1/mints/<issuer key>/keys`, so anyone who knows the issuer
key can check a note offline. The issuer checks notes against its own keys
only, so a leaked issuer key cannot get a forged note past it; it could
certify a false key that fools someone checking offline, though, so a leaked
issuer key still means moving to a new issuer. Issuers from before this change keep their
generations signed by the issuer key until those are dropped.

**Each note is bound to a one-time key.** To be paid, the receiver makes a
fresh key and hands out its public half. Spending the note needs a signature
from that key over a message naming every input and output, so a payer who
keeps a copy of a note cannot spend it, and nobody can redirect a transfer in
flight. "No wallets" here means no account at the issuer and no key reused
across notes.

**Policy is separate from the ledger.** [`Policy`](crates/worgl/src/policy.rs)
holds the rules an operator may change at any restart without touching stored
state. The default, `Policy::cash_like()`, is chosen for the privacy property
above:

- one input per transfer, so the issuer never sees two lineages join;
- up to 16 outputs, so one note can be split; all outputs descend from one
  note, so a split reveals nothing new;
- powers-of-two denominations, so notes of one size are interchangeable and
  any amount splits into at most one note of each size.

A payment of 5 is then a 4-note transfer and a 1-note transfer to two fresh
payee keys, and change is the payee paying notes back the same way.
`Policy::flexible()` allows up to 16 inputs of any amount for systems that do
not need the privacy property.

**New money lands in the second to last layer.** A note at position `k` of
`n` layers can make `n − 1 − k` more hops right now and, if left untouched,
dies in the `(k + 1)`-th rotation from now. Those two numbers always add up
to `n`, so the mint layer trades spending room for lifetime. The second to last layer allows one hop right
away and the longest life that does.

**Outputs land one layer past the furthest input,** so merging (where allowed)
never gives an input extra hops.

**Keeping money alive is a self-transfer.** Paying a note to a fresh key of
your own moves it one layer on. This is the "regular transfer" the design
encourages and the way to save a note before its layer is dropped.

**A holder can burn a note.** A `BurnRequest` destroys its input notes and
issues nothing in their place, so money that is redeemed leaves circulation
at once instead of waiting for its layer to be dropped. Burns follow the same
input limit as transfers and work on a note frozen in the last layer. They
are signed in their own domain, so a burn signature can never be replayed as
a transfer or the other way round, and a resent burn is answered from its
receipt without destroying anything more.

**A full layer refuses until the next rotation.** Bits are not reused within a
generation, so `layer_size` caps how many notes can land in one layer per
rotation period.

**A layer sized to the supply never fills.** Every hop moves value to a
strictly newer generation, so a unit of value lands in any generation at most
once, and it stays live until that generation is dropped. A layer therefore
never hands out more bits than the value live in it and the layers after it.
While the outstanding value, counted in the smallest amount, stays at or below
`layer_size`, `layer_full` cannot happen. The property tests check both the
bound and the guarantee.

**Resending a request is safe.** The issuer remembers recent transfers (65,536
by default) by the hash of their signed message. If a response is lost, the
client resends the identical request and gets the identical notes back, since
Ed25519 signatures are deterministic.

**Minting takes a mint authority's signature.** The policy lists the public
keys allowed to mint (`mint_authorities`). Each mint request is signed by one
of them and carries a sequence number, and the issuer accepts only a number
above that authority's last, so a copied request cannot mint twice, while
resending the last request returns the same note. Each such mint is logged
with its signature, so the log shows who authorized every unit of supply.
The server mints no other way: no token can mint. The library's
`Issuer::mint` remains for code that embeds the issuer directly.

**The issuer is an event-sourced state machine.** Every change is an `Event`
that the issuer plans without side effects, the store writes and flushes to a
log, and only then the issuer applies. Applying is deterministic and checked,
so a snapshot plus its log rebuilds the exact same state, and a note is never
handed out unless the event that created it is on disk. A `Batch` shares one
log entry and one flush among many events; the server puts every request
waiting at the moment of a flush into the next one. The issuer also keeps the
live value of each layer and the value minted, destroyed by rotations and
burned, so `outstanding = minted − destroyed − burned` can be audited at any
time.

**Rotation is an admin call or a schedule** aligned to multiples of the
period since the Unix epoch, so restarts do not shift it. Each mint has its
own period, and its `info` reports the period and the time of the next
rotation, so clients know when frozen notes thaw and when idle ones must be
refreshed. A scheduled rotation logs the boundary it was due at, and the
issuer refuses one that is not due after the last, so neither a wall clock
that steps backwards nor a restart can rotate twice in one period.
Boundaries missed while the server is down, or while the mint is paused, are
not made up.

**Storage fails loudly.** The log is tied to its issuer's key, every
record is checksummed, and a damaged record is only dropped when nothing
readable follows it, which is the one place a crash can leave a torn write.
Damage anywhere else stops the store from opening rather than silently
losing acknowledged transfers.

**The log is laid out for replication.** Each entry has an index and the
term and node of the leader that wrote it, as Raft numbers its log; the
snapshot records the last entry it holds; and the log can drop a tail that a
new leader overwrote. A store on its own writes every entry under term 0.
Stores from before the log convert the first time they are opened. Pausing,
resuming and the time a scheduled rotation was due are events in the log
too, so every replica of a mint agrees on them.

**Generation keys live in their own file.** The snapshot and log hold
only public keys and certificates, so they can be copied or audited without
exposing a secret. The secrets are in `keys.bin` (mode 0600) next to them. A
rotation writes its new key there before logging the rotation, and keys of
dropped generations leave the file at the next snapshot. Back `keys.bin` up
with the rest of the state: without it the live generations' notes cannot be
signed and the store does not open.

**A rotation's log entry can carry its key, sealed.** Replicas that apply a
rotation they did not make need its key, and the log is all they share. Given
a key-encryption key (`Kek`), a store seals each new generation key with
XChaCha20-Poly1305 into the entry of the rotation that certifies it, bound to
the issuer, the generation and the key, and unseals it on opening if
`keys.bin` lacks it. `FileKek` keeps the KEK in a file for development; a
cloud KMS can stand behind the same trait. `FileLog::export` reads a log
without the sealed keys, and without touching a log in use, for auditors.

### What the rules imply

Once a note reaches the last layer it can move at most once per rotation. With
4 layers and daily rotation, money changes hands at most once a day in steady
state, with a burst allowance for notes in lower layers, and a note left alone
dies within 4 days. The rotation period sets both how fast money can
circulate and how long it can sit still.

## Using the library

```rust
use worgl::{HolderKey, Issuer, IssuerKey, LedgerConfig, NoteStatus, Output, Policy, TransferRequest};

let ledger = LedgerConfig { layers: 4, layer_size: 1 << 16 };
let mut issuer = Issuer::new(IssuerKey::generate(), ledger, Policy::cash_like())?;

// Alice makes a one-time key and is minted a note bound to it.
let alice = HolderKey::generate();
let note = issuer.mint(8, alice.public_key())?;

// Bob makes a one-time key; Alice signs the note over to it.
let bob = HolderKey::generate();
let request = TransferRequest::single(note, &alice, Output { amount: 8, holder: bob.public_key() })?;
let bobs_note = issuer.transfer(&request)?.remove(0);

assert_eq!(issuer.status(&note)?, NoteStatus::Spent);
```

For durability, wrap the issuer in `PersistentIssuer`, which logs every
event before applying it. To keep signature checks out of a lock, verify with
`issuer.verifier()` first and pass the `VerifiedTransfer` in.

`cargo run --example lifecycle` walks through a note's whole life: mint,
split, pay, freeze in the last layer, rotate, replay refused, idle money
expiring and refreshed money living on.

## Holding and paying

The library lets a holder do anything the protocol allows, including things
that leak who owns what. `worgl-sdk` does the right thing by default: a fresh
key for every note it expects, one note per transfer with change back to
fresh keys, the transfers of one payment and the refreshes before a rotation
spread out at random moments, and outputs in random order.

A payment takes one round trip between the two holders:

1. The payee calls `Holder::request` and sends the `PaymentRequest`: the
   amount as notes, each to a fresh key of its own.
2. The payer calls `Holder::pay` and sends back the `Payment`: signed
   transfers, each spending one of its notes, with a time to submit each.
   It prefers one note that covers everything; otherwise it spends several,
   largest first.
3. The payee checks it with `PaymentRequest::check`, submits each transfer
   to the mint at its time, and takes the new notes in with
   `Holder::absorb`. The payer records the same answers with
   `Holder::settle`, which takes in its change. A payer that never saw the
   answer resends its outgoing transfers, spaced out by `Holder::resend`,
   and gets the same notes back.

A `MintView`, read from the mint's info, tells the store which notes can
move now, which are frozen until the next rotation and which die at it, and
follows the rotation schedule without asking again. `Holder::refresh` pays
the notes about to die to the holder itself, each at a random moment before
the rotation. The store saves to JSON with its secret keys, so encrypt it at
rest: the notes are bearer.

`worgl-client` runs it against a server. A `Purse` wraps a holder, a
`Client` and a `Store`: it reads mint info by itself, submits each transfer
at its time, retries what fails on the way (a resent transfer gets the same
answer), and saves after every change, before handing out anything that
depends on it.

```rust
let client = Client::new("https://mint.example.com")?;
let alice = Purse::open(client.clone(), "alice.json")?;
let bob = Purse::open(client, "bob.json")?;

let request = bob.request(&mint, 5).await?;                 // Bob → Alice
let payment = alice.pay(&request, Spacing::default()).await?; // Alice → Bob
bob.receive(&request, &payment).await?;                     // checks, submits, keeps
alice.settle(Spacing::default()).await?;                    // later: her change
```

When the payer submits instead (`Purse::submit`), it hands the notes from the
answers to the payee, who checks their signatures and keeps them with
`Purse::accept`.

## The command line

`worgl` keeps a purse in one file, encrypted with XChaCha20-Poly1305 under a
key derived from a passphrase with Argon2id (64 MiB, three passes). The
passphrase comes from `WORGL_PASSPHRASE` or a prompt; the server and the
file from `--server` and `--purse` or `WORGL_SERVER` and `WORGL_PURSE`.
Requests, payments and handed-over notes are JSON files for the two sides to
exchange.

```sh
worgl init                                     # each side, once
worgl mint $MINT 8 --authority-key mint.key    # as a mint authority, for testing
worgl request $MINT 5 -o request.json          # payee → send request.json
worgl pay request.json -o payment.json         # payer → send payment.json
worgl receive request.json payment.json        # payee: checks, submits, keeps
worgl settle                                   # payer, later: takes in the change
worgl balance $MINT
worgl refresh $MINT                            # once per rotation period
```

`worgl info`, `notes`, `withdraw`, `submit`, `accept` and `prune` cover the
rest; `worgl help <command>` explains each.

## Running the server

```sh
cargo build --release
./target/release/worgl-server init        --state ./server
./target/release/worgl-server create-mint --state ./server --layers 4 --layer-size 1048576 --rotate-every 86400
./target/release/worgl-server serve       --state ./server --listen 127.0.0.1:8080
```

One server hosts any number of mints. Each mint is one issuer, with its own
key, ledger, policy, rotation period and directory under `mints/`, named by
its issuer key. A note names its issuer, so the holder routes take no mint in
their path and find it from the note.

`init` writes a random 64-character operator token, which creates and lists
mints, and `kek.key` (mode 0600), the key-encryption key that seals each
mint's new generation keys into their rotations' log entries. `create-mint` writes the mint's issuer key, its own admin token, which
rotates and pauses that mint only, and a mint authority key `mint.key` (all
mode 0600), the policy as `policy.json` with the mint key listed in
`mint_authorities`, its period in `mint.json`, and an empty ledger with a
generation key for each layer in `keys.bin` (mode 0600). It prints the mint's
issuer key. The server never reads `mint.key`; move it to wherever you mint
from. Edit `policy.json` and restart to change the rules; missing fields take
their cash-like defaults:

```json
{ "max_inputs": 16, "denominations": "any" }
```

A running server takes new mints through `POST /v1/admin/mints` instead, with
a body like `{"ledger": {"layers": 4, "layer_size": 1048576}, "policy":
{"mint_authorities": ["<public key>"]}, "rotate_every": 86400}`. Its policy
must list a mint authority, since the server never holds one, and the answer
carries the new mint's issuer key and admin token. A mint is built in a hidden
directory and renamed into place when complete, so a crash never leaves one
half made. The server refuses to start if any mint fails to open, or if a
token is shorter than 32 characters.

| Method | Path | Token | Body | Answer |
|---|---|---|---|---|
| GET | `/healthz` | | | `ok` |
| POST | `/v1/verify` | | `{"note"}` | serial, amount, state, position, hops left |
| POST | `/v1/transfer` | | `{"inputs": [{"note", "signature"}], "outputs": [{"amount", "holder"}]}` | `{"notes"}` |
| POST | `/v1/burn` | | `{"inputs": [{"note", "signature"}]}` | `{"value"}` burned |
| GET | `/v1/mints/{mint}/info` | | | layers, supply, policy, rotation schedule, paused |
| GET | `/v1/mints/{mint}/keys` | | | the live generations' keys and their certificates |
| POST | `/v1/mints/{mint}/mint` | | `{"authority", "sequence", "amount", "holder", "signature"}` | `{"note"}` |
| POST | `/v1/mints/{mint}/rotate` | mint | | what was destroyed, and the new generation's key |
| POST | `/v1/mints/{mint}/pause` | mint | | `{"paused": true}` |
| POST | `/v1/mints/{mint}/resume` | mint | | `{"paused": false}` |
| GET | `/v1/admin/mints` | operator | | `{"mints"}` |
| POST | `/v1/admin/mints` | operator | `{"ledger", "policy", "rotate_every"}` | `{"mint", "token"}` |

To mint, sign a request with the mint key and post it to the mint `create-mint`
printed:

```sh
./target/release/worgl-server sign-mint --key ./server/mints/<mint>/mint.key --issuer <mint> \
    --sequence 1 --amount 8 --holder <holder public key> \
  | curl -sd @- -H 'content-type: application/json' http://127.0.0.1:8080/v1/mints/<mint>/mint
```

**Pausing stops a mint without costing anyone a note.** A paused mint refuses
transfers, burns, mints and rotations, scheduled or requested, and still
verifies notes, so nothing moves and nothing expires until it resumes. Its
`info` shows the pause to everyone, and the pause is logged, so it survives
restarts. It is for incidents such as a leaked key or an attack on one mint;
the others carry on.

Routes with a token need `Authorization: Bearer <token>`. Errors carry a
stable code, such as `{"error": "spent", "message": "note 3:17 is already
spent"}`; the codes are listed in
[`ApiError::code`](crates/worgl-server/src/lib.rs). The request types are the
library's own (`TransferRequest`, `BurnRequest`, `MintRequest`, `Note`,
`PublicKey`), so a Rust client signs with the library and posts the JSON it
serializes to.

`worgl-server export-log --mint server/mints/<mint>` prints a mint's log for
an audit, one JSON object per entry, with no sealed keys. It is safe to run
while the server serves the mint.

The server is one process. Each mint commits on its own, so a busy mint does
not slow the others, and an idle one holds no thread. A lock file per mint
stops a second process from opening it. The server speaks plain HTTP: put it
behind a reverse proxy for TLS and rate limiting.

### Running a cluster

Three servers can run as a cluster that keeps every mint on all three, so each
mint carries on when one server fails. Each mint is a Raft group of the
members, built on [`worgl-raft`](crates/worgl-raft), and a change is answered
once two of the three hold it on disk. Every member starts from a copy of one
state directory:

```sh
S=./target/release/worgl-server
MEMBERS="--member 1=10.0.0.1:8080,10.0.0.1:7080 --member 2=10.0.0.2:8080,10.0.0.2:7080 --member 3=10.0.0.3:8080,10.0.0.3:7080"
$S init         --state ./server
$S join-cluster --state ./server --node 1 $MEMBERS
# Copy ./server to the other two machines and run join-cluster there with --node 2 and --node 3.
$S serve        --state ./server --listen 0.0.0.0:8080
```

Each `--member` gives a member's id, the address of its public API and the
address of its Raft port. `join-cluster` writes `cluster.json` and, the first
time, `cluster.token` (mode 0600), which the members show each other on their
Raft ports. Copy the directory before any member starts: the copies share the
mints, tokens and key-encryption key, and differ only in `--node`. A directory
a server has already run mints in can join too, and its mints carry on with
every note and setting. On a member, `serve` also listens on its Raft port, or
on `--raft-listen` if given.

Any member takes any request, and one that does not lead the mint passes it on
to the one that does. A read is answered once the leader confirms it still
leads, so no answer is older than one already given. When a leader fails, the
others elect a new one within a few seconds. Until then, requests are answered
`503 no_leader`, and sending the same request again is safe. A leader cut off
from the other members stops taking requests after two seconds without hearing
from them, and a member that cannot reach the leader says so within a few
seconds, so clients that still reach a cut-off member go elsewhere instead of
waiting on it. Only the leader makes scheduled rotations.

With the members running, `POST /v1/admin/mints` to any of them creates a mint
on all three. It goes to the member leading the cluster's registry, a Raft
group of the members that lists the mints created this way. A mint's record
holds its policy, period and first state, with its issuer key, admin token and
generation keys sealed by the key-encryption key the members share. Every
member writes the mint's directory from that record, so all of them start the
mint alike, and a member that was away writes it when it returns. The answer
comes once the mint's group has a leader; a member that has not yet read the
record answers `404 unknown_mint` for a moment. A creation answered `503
no_leader` made nothing, so send it again. One answered `503 timeout` may have
made a mint the answer never named: list the mints before creating another,
and read its admin token from `admin.token` in its directory on any member.

The Raft port speaks plain HTTP and checks only the cluster token, so keep it
on a network that only the members can reach. The members are fixed once the
cluster starts.

## Not covered yet

- **The issuer sees lineage.** Inherent to the design; see above.
- **No backing.** A mint authority decides what to mint; there is no deposit
  or redemption into other money.
- **Clusters are fixed and their members talk in the clear.** Members cannot
  be added or removed, and the Raft port has no TLS. The issuer key itself never rotates: a new one is a
  new mint.
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

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
