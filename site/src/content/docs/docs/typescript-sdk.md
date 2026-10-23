---
title: TypeScript SDK
description: Hold, pay and receive notes from Node or a browser, on the same Rust core as the Rust SDK.
---

The TypeScript SDK holds, pays and receives notes from Node or a browser.
The privacy rules and all signing run in the same Rust core as the Rust SDK,
`worgl-sdk`, compiled to WebAssembly. The package moves its JSON between the
holder, the server and a store.

Amounts are `bigint`, and requests, payments and transfers travel as JSON
text, so no amount loses precision.

## Build it

The package is not published on npm. Build it from `sdk/typescript` in the
repository:

```sh
git clone https://github.com/zhangheqi/worgl
cd worgl
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked  # the version Cargo.lock pins
cd sdk/typescript
npm ci
npm run build   # compiles the Rust core to wasm/ and the package to dist/
```

Then point your project at the built folder, for instance with
`npm install ../worgl/sdk/typescript`. The package is named `worgl`, with
`worgl/node` for what needs Node.

It needs Node 22.18 or later, or a browser with top-level `await`.
Importing the package loads the WebAssembly module: from disk under Node,
through `fetch` relative to the module in a browser or bundler.

## A payment

```ts
import { Client, Purse } from 'worgl';
import { FileStore } from 'worgl/node';

const client = new Client('https://mint.example.com');
const alice = await Purse.open(client, { store: new FileStore('alice.json') });
const bob = await Purse.open(client, { store: new FileStore('bob.json') });

const request = await bob.request(mint, 5n);          // Bob sends request.json to Alice
const payment = await alice.pay(request);             // Alice sends payment.json to Bob
const received = await bob.receive(request, payment); // checks, submits at its times, keeps
await alice.accept(received.theirs);                  // Bob sends Alice her change, she keeps it
```

The other side reads what it is sent with `PaymentRequest.from(json)` and
`Payment.from(json)`. `received.theirs` holds the notes of the mint's answers
that are not Bob's: Alice's change. Keeping it with `accept` also settles
Alice's transfer. Without it, `alice.settle()` sends her transfers again, and
the mint answers with the same notes while it keeps its answers (see
[Error codes](../error-codes/)).

For a fuller worked example, the SDK's tests in
[`sdk/typescript/test/purse.test.ts`](https://github.com/zhangheqi/worgl/blob/main/sdk/typescript/test/purse.test.ts)
run purses against a real server.

## Purse

- `Purse` reads mint info itself, submits each transfer at its time, retries
  what fails on the way (a resent transfer gets the same answer) and saves
  after every change, before handing out anything that depends on it. If a
  save fails, the change is undone and the call rejects. After a `busy`
  answer it waits as long as the mint's `Retry-After` asks, up to 30
  seconds, before the next try.
- Before it keeps an answer, the purse checks it against the keys of the
  transfer's mint, read after the answer: one note for each of the
  transfer's outputs, with that output's amount and key, signed by the mint.
  It keeps none of an answer that is not the mint's, such as a forged note
  from a server that is not the mint's, nor of one whose keys it cannot
  read. The transfer is listed in `submitted.refused` with outcome `kept`,
  and one the purse made stays outgoing, for `settle` to send again. Its
  error is a `WorglError` that says what is wrong with the answer
  (`miscounted`, `unasked`, `unsigned` or `wrong_issuer`), or why the keys
  could not be read.
- `purse.refresh(mint)` pays the notes about to expire back to the purse,
  each at a random moment before the next rotation, and resends the
  outgoing transfers that must land before it. `planRefresh` and `submit`
  split it for applications with their own scheduler. The moments are
  counted by the server's clock, from the `Date` of its info answer
  (`client.infoTextAndTime`), so this machine's clock, slow or fast, moves
  none past the rotation. A browser reads that `Date` from a mint on
  another origin only if the answer carries
  `Access-Control-Expose-Headers: Date`, as `worgl-server`'s do unless it
  runs with `--no-cors`; without it, the browser's clock counts the
  moments. `purse.prune(mint)` drops the notes whose layer the mint's info
  says it has dropped, never by this machine's clock.
- `purse.notesAt(mint)` lists the notes held at a mint, each with its value,
  its layer, what it can do now (`spendable`, `expiring` at the next
  rotation, `frozen` in the last layer until then, `expired` or `unknown`),
  when a frozen one can move (`spendableAt`) and when it dies unless moved
  (`expiresAt`), in Unix seconds by the mint's clock. `purse.firstExpiry(mint)`
  is the first time the mint would take a note of the purse's, counting
  those its outgoing transfers spend: an application that keeps notes alive
  only while it runs should run again before then, less a margin of its own.
  Both read the mint's info and nothing else. `client.verify(note)` asks the
  mint about one note instead, so asking about several in a row invites the
  guess that they have one owner (see the [Privacy model](../privacy/)).
- When the payer submits instead (`purse.submit(payment.transfers)`), it
  hands `submitted.theirs`, the payee's notes, to the payee, who checks their
  signatures against the mint's keys and keeps them with
  `purse.accept(notes)`. The payer's purse keeps them for the payee too,
  until `purse.forgetOwed(submitted.theirs)` once they are handed over.
- A transfer refused as `spent` or `expired` that the purse has something
  in carries `atStake` in `submitted.refused`: it may have been applied,
  with an answer the mint no longer keeps, and that much may be lost unless
  the other side hands the notes over.
- Each refused transfer carries `outcome`, what the purse did with it. One
  it made that was refused as `spent` or `expired` with status 409 is
  dropped, since its note is gone. One refused as `layer_full` with status
  503, which the mint never applied and never will, is cancelled and its
  note taken back, and so is one refused as `policy` that the policy the
  mint publishes refuses too. All keep the keys of their change, so `accept`
  still takes in change handed over later. Any other refusal leaves it
  outgoing. `purse.cancel(transfer)` and `purse.forget(transfer)` do the
  same by hand, for a transfer from `purse.outgoing()`, which lists those
  not yet answered.
- `purse.pay` refuses, before signing, a request for a note the mint does
  not make, or to a key the purse has paid already, such as one request
  twice. `purse.payments()` lists the payments it made and keeps, each with
  the payment to send again, less any transfer cancelled or forgotten, and
  `owed`, the payee's notes from every answer the purse got itself by
  settling, refreshing or submitting; `purse.forgetOwed(notes)` stops
  keeping them once they are handed over.
- `purse.balance(mint)` splits what the purse has at a mint into parts that
  do not overlap: `held` now (`spendable`, `frozen` and `unknown`),
  `sending` to payees in transfers not yet answered, `change` and
  `refreshing` on their way back from them, and `requested`. Its `total`,
  held plus change and refreshing, counts no note twice: paying lowers it by
  the amount paid, and submitting, settling and refreshing leave it as it
  is. `outgoing` and `expected` sum some of those parts, and `expiring` is
  part of what is held. `unconfirmed`, change of transfers dropped or
  cancelled, is in no part: after a cancel it comes only if the mint spent
  the note taken back.
- `purse.takeFromFaucet(new Client(faucetUrl))` asks a playground
  [faucet](../running-a-faucet/) for a note, to a fresh key like any other,
  and keeps it. A page on another origin than the faucet's can ask it only
  if the faucet [names that origin](../running-a-faucet/#pages-in-a-browser).

## Client and errors

`Client` makes one call per route and rejects with a `MintError` carrying the
mint's code (`spent`, `frozen`, `paused`…). `isTransient(error)` says whether
trying again may work, and `retryAfterMs` how long a `busy` mint asked to
wait first. `isTransient` counts `layer_full` out: a transfer refused so is
refused every time, though a mint request may pass after the next rotation.
The codes are listed in [Error codes](../error-codes/).

What the SDK refuses itself, in its Rust core or around it, such as a
payment the purse cannot make, throws or rejects with a `WorglError`: an
`Error` named `WorglError`, with a stable `code` and that code's fields in
`details`. `message` is for people and may change. `isWorglError(error)`
tells one apart, and checking `code` then gives `details` their type:

```ts
try {
  await alice.pay(request);
} catch (error) {
  if (
    !(isWorglError(error) && error.code === 'fragmented') ||
    error.details.atMost === undefined
  ) {
    throw error;
  }
  // Alice's notes are too small for the ones Bob asked for: he drops
  // that request and asks again, in notes she can pay.
  await bob.withdraw(request);
  await alice.pay(await bob.requestAtMost(mint, request.amount, error.details.atMost));
}
```

In `details`, amounts and generations are `bigint`, counts and limits
`number`, keys hex strings, and a field with no value `undefined`.
`Holder` and the other exports of the core throw the same codes as the
purse calls built on them.

| Code | `details` | Meaning |
|---|---|---|
| `unaffordable` | `spendable`, `needed` | `pay`: only `spendable` can be spent now, and the request asks for `needed`. Frozen notes become spendable at the next rotation. |
| `fragmented` | `largest`, `atMost` | `pay`: enough can be spent, but in notes too small for those the request asks for; the largest is `largest`. A request for the same amount in notes of at most `atMost`, which the payee makes with `requestAtMost` after withdrawing the first, can be paid now, and none in larger notes can. `atMost` is `undefined` when no such request can be paid, as when it would take too many notes: ask for less, or wait for frozen notes to become spendable. |
| `denomination` | `amount` | `pay`: the request asks for a note of `amount`, which is not one of the mint's denominations. |
| `paid` | `holder` | `pay`: the request asks for a note to `holder`, a key the purse has paid already, such as one request twice. Paying again takes a new request. |
| `too_many_outputs` | `maxOutputs` | `pay`: the change does not fit in the `maxOutputs` notes one transfer may make. |
| `empty_request` | | `pay`: the request asks for nothing. |
| `wrong_mint` | | `pay`: the request is for another mint than the info read for it. `receive`: the payment spends notes of another mint than the request's. |
| `policy` | | `pay`: a transfer it planned breaks the mint's policy, which is a bug. The message says how. |
| `sign` | | `pay`: signing failed, which means the saved purse is corrupt. |
| `zero_amount` | | `request`, `requestAtMost`: the amount is zero. |
| `unsplittable` | `amount` | `request`, `requestAtMost`: `amount` cannot be split into the mint's denominations, or into those no larger than asked. |
| `too_many_notes` | `max` | `request`, `requestAtMost`: the request would take more than `max` notes. |
| `unpaid` | `amount`, `holder` | `receive`: the payment does not pay the note of `amount` to `holder` that the request asks for. |
| `overpaid` | `amount`, `holder` | `receive`: the payment pays `amount` to `holder`, a key the request asks a note for, more than once or with another amount. |
| `not_one_input` | | `receive`: a transfer of the payment spends more or fewer than one note. |
| `bad_signature` | | `accept`: a note's signature does not verify: it is forged or altered. |
| `unknown_generation` | `generation` | `accept`: the mint's keys hold none for a note's `generation`, because its layer was dropped or the keys predate it. |
| `wrong_issuer` | | `accept`: the keys the server gave for a note's mint are another mint's. `Submitted.refused`: the keys it gave for a transfer's mint are another mint's, so the purse kept none of the answer. |
| `miscounted` | `outputs`, `answered` | `Submitted.refused`, from `submit`, `receive`, `settle` or `refresh`: an answer to a transfer holds `answered` notes for its `outputs` outputs. It is not the mint's, and the purse kept none of it. |
| `unasked` | `index` | `Submitted.refused`: note `index` of an answer is not the transfer's output in its place, but another mint's, or of another amount or key. The purse kept none of the answer. |
| `unsigned` | `index` | `Submitted.refused`: note `index` of an answer does not carry the mint's signature: it is forged or altered, or the keys do not know its generation. The purse kept none of the answer. |
| `faucet_amount` | `amount` | `takeFromFaucet`: the faucet hands out notes of `amount`, which its mint cannot make as one note. |
| `wrong_passphrase` | | `Backup.open`: the passphrase is wrong, or the backup was altered after it was sealed: the two look the same. |
| `not_sealed` | | `Backup.open`: the text is not a backup or purse file. |
| `unknown_version` | `version` | `Backup.open`: the backup is of `version`, a newer one this SDK does not read. |
| `damaged` | | `Backup.open`: a part of the backup does not read, such as its salt, settings or ciphertext, or it opened to no purse. The message says which. |
| `out_of_memory` | | `Backup.open`, `backup`: too little memory to derive the key: 64 MiB, or up to 256 MiB for a backup whose settings ask for more. |
| `no_randomness` | | `backup`: the system's random number generator failed. |
| `unknown_format` | `format` | `Purse.open`, `Backup.open`: the saved purse, or the one a backup holds, is in `format`, a newer one this SDK does not read. |
| `malformed` | | Any call: something it was given is not the JSON it takes, such as a payment request, a payment, notes, a mint's info or keys, or, for `Purse.open`, a saved purse; or a transfer spends no notes. |
| `no_layers` | | Any call that reads a mint's info: the info lists no layers. |

## Stores

`MemoryStore`, and `FileStore` from `worgl/node`, which replaces its file
atomically, readable by its owner only. The JSON holds every note's secret
key, so keep it private and encrypted at rest. Implement `Store` to keep it
elsewhere.

## Backups

`purse.backup(passphrase)` seals the purse into a backup: the file the
[`worgl` command](../command-line/#the-purse-file) keeps its purse in, which
it opens as a purse, with the purse's server named for each mint.
`Backup.open(sealed, passphrase)` opens a backup, or a purse file of `worgl`,
and `purse.restore(backup)` merges it into a purse:

```ts
import { Backup } from 'worgl';

const sealed = await alice.backup(passphrase); // keep it apart from the store
// On another device, or once the store is lost:
const restored = await alice.restore(await Backup.open(sealed, passphrase));
await alice.settle(); // resends the transfers it took in
```

A backup holds every note's secret key, like the store, sealed with
XChaCha20-Poly1305 under a key derived from the passphrase with Argon2id
over 64 MiB, three passes. Deriving it takes about a quarter of a second in
Node and in Chromium on a desktop processor, longer on a phone, and blocks
the thread meanwhile; the WebAssembly memory grows by 64 MiB the first time
and keeps that size. A wrong passphrase and a backup altered after it was
sealed are refused alike, with `wrong_passphrase`.

Restoring keeps what is the purse's in either copy and counts nothing twice
that the two can tell apart: a note in both is held once, a request of
either is expected until it is paid, and a transfer the backup shows
answered, or cancelled or forgotten, is settled or ended here too.
`restored` lists what changed. Some of it only the mint can tell:

- A note only the backup holds may be one the purse spent since. It counts
  until the mint says otherwise. `purse.prune(mint)` drops the expired ones
  from the mint's info alone, and `purse.checkNotes(restored.notes)` asks
  the mint about each and discards those it reports spent. Each question
  shows the mint one note, from this machine's address and at its time, so
  it asks at random gaps, 2 to 60 seconds by default (see the
  [Privacy model](../privacy/#timing)). Asking nothing leaves the transfers
  that next spend the notes to find the spent ones, which the mint refuses.
- A transfer only the backup has outgoing may be one the purse settled and
  forgot. `purse.settle()` resends it: the mint answers with the same notes
  while it keeps the answer, and refuses it once its note is spent.
- Two transfers that spend one note, in `restored.conflicts`, are a note
  each copy spent. The mint applies at most one, and `settle` drops the
  other.

`restore` takes the mints the backup names this purse's server for, or no
server, as in a purse file of `worgl`. `restored.others` lists the rest, to
restore into a purse of their server; `purse.restore(backup, [mint])` names
the mints to take instead.

## Testing it yourself

The tests run against a real server on a local port, and open backups with
the `worgl` command line, so build both first:

```sh
cargo build -p worgl-server -p worgl-faucet -p worgl-cli   # from the repository root
cd sdk/typescript
npm run check   # type-checks the package and its tests
npm test
```
