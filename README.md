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

The workspace has two crates:

- [`worgl`](crates/worgl): the library. Ledger, note format, transfer
  protocol, policy, issuer state machine and crash-safe storage.
- [`worgl-server`](crates/worgl-server): an HTTP issuer built on it.

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

## How the design maps onto the code

| Design point | Where it lives |
|---|---|
| Centralized, no wallets | One [`Issuer`](crates/worgl/src/issuer.rs). It keeps no accounts and stores no notes; holders keep their own notes. |
| Asymmetric crypto issues notes | Ed25519 via `ed25519-dalek`, strict verification ([`keys.rs`](crates/worgl/src/keys.rs)). |
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

**The issuer is an event-sourced state machine.** Every change is an `Event`
that the issuer plans without side effects, the store writes and flushes to a
journal, and only then the issuer applies. Applying is deterministic and
checked, so a snapshot plus its journal rebuilds the exact same state, and a
note is never handed out unless the event that created it is on disk. The
issuer also keeps the live value of each layer and the value minted,
destroyed by rotations and burned, so
`outstanding = minted − destroyed − burned` can be audited at any time.

**Rotation is an admin call or a schedule** aligned to multiples of the
period since the Unix epoch, so restarts do not shift it. When a schedule is
set, `/v1/info` reports its period and the time of the next rotation, so
clients know when frozen notes thaw and when idle ones must be refreshed. The server records
the last scheduled boundary, so a wall clock that steps backwards cannot
rotate twice in one period. Boundaries missed while the server is down are
not made up.

**Storage fails loudly.** The journal is tied to its issuer's key, every
record is checksummed, and a damaged record is only dropped when nothing
readable follows it, which is the one place a crash can leave a torn write.
Damage anywhere else stops the store from opening rather than silently
losing acknowledged transfers.

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

For durability, wrap the issuer in `PersistentIssuer`, which journals every
event before applying it. To keep signature checks out of a lock, verify with
`issuer.verifier()` first and pass the `VerifiedTransfer` in.

`cargo run --example lifecycle` walks through a note's whole life: mint,
split, pay, freeze in the last layer, rotate, replay refused, idle money
expiring and refreshed money living on.

## Running the server

```sh
cargo build --release
./target/release/worgl-server init  --state ./issuer --layers 4 --layer-size 1048576
./target/release/worgl-server serve --state ./issuer --listen 127.0.0.1:8080 --rotate-every 86400
```

`init` writes the issuer key and a random 64-character admin token (both
mode 0600), the policy as `policy.json` and an empty ledger. The server
refuses to start with an admin token shorter than 32 characters. Edit `policy.json` to change the rules;
missing fields take their cash-like defaults:

```json
{ "max_inputs": 16, "denominations": "any" }
```

| Method | Path | Body | Answer |
|---|---|---|---|
| GET | `/healthz` | | `ok` |
| GET | `/v1/info` | | layers, supply, policy, rotation schedule |
| POST | `/v1/verify` | `{"note"}` | serial, amount, state, position, hops left |
| POST | `/v1/transfer` | `{"inputs": [{"note", "signature"}], "outputs": [{"amount", "holder"}]}` | `{"notes"}` |
| POST | `/v1/burn` | `{"inputs": [{"note", "signature"}]}` | `{"value"}` burned |
| POST | `/v1/admin/mint` | `{"amount", "holder"}` | `{"note"}` |
| POST | `/v1/admin/rotate` | | what was destroyed |

Admin routes need `Authorization: Bearer <token>`. Errors carry a stable code,
such as `{"error": "spent", "message": "note 3:17 is already spent"}`; the
codes are listed in [`ApiError::code`](crates/worgl-server/src/lib.rs). The
request types are the library's own (`TransferRequest`, `BurnRequest`, `Note`,
`PublicKey`), so a Rust client signs with the library and posts the JSON it
serializes to.

The server is one process; a lock file stops a second one from opening the
same state. It speaks plain HTTP: put it behind a reverse proxy for TLS and
rate limiting.

## Not covered yet

- **The issuer sees lineage.** Inherent to the design; see above.
- **No backing.** Minting is an admin call; there is no deposit or redemption
  into other money.
- **One issuer key** with no key rotation, and no replication or failover.
- **No client CLI.** Holders use the library, or any HTTP client plus Ed25519.

## Development

```sh
cargo test --workspace          # unit, property and HTTP tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

The minimum supported Rust version is 1.89. CI runs all of the above plus the
docs and the example on every push and pull request.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
