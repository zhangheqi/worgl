# Worgl for TypeScript

Hold, pay and receive [Worgl](../../README.md) notes from Node or a browser.

The privacy rules and all signing run in the same Rust core as the Rust SDK
(`worgl-sdk`), compiled to WebAssembly. This package moves its JSON between
the holder, the server and a store: a fresh key for every note, one note per
transfer, and the transfers of one payment, refreshes and resends spread out
at random times all come from the core. Amounts are `bigint`, and requests,
payments and transfers travel as JSON text, so no amount loses precision.

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
the mint answers with the same notes while it keeps its answers.

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
  guess that they have one owner.
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
  is. `outgoing` and `expected` sum some of those parts, `expiring` is part
  of what is held, and `unchecked` is the part of the total a restore took
  in from another copy of the purse, until the mint says it was not spent
  since. `unconfirmed`, change of transfers dropped or cancelled, is in no
  part: after a cancel it comes only if the mint spent the note taken back.
- `purse.takeFromFaucet(new Client(faucetUrl))` asks a playground faucet
  (`worgl-faucet`) for a note, to a fresh key like any other, and keeps it.
  A page on another origin than the faucet's can ask it only if the faucet
  names that origin with `--allow-origin`.
- `purse.backup(passphrase)` seals the purse into a backup, the file the
  `worgl` command line keeps its purse in, naming the purse's server for
  each mint; `{ purses, mints }` adds the purses of other servers, and what
  the application keeps of each mint, such as a name, which every reader
  gives back in `backup.mints`. `Backup.open(sealed, passphrase)` opens one,
  or a purse file of `worgl`, and `purse.restore(backup)` merges it in: a
  note in both is held once, and a transfer either copy settled, cancelled
  or forgot stays ended. What only the backup had may have been spent since
  by another copy, so it comes in unchecked (`purse.unchecked()`) and
  counts until the mint says otherwise: `purse.checkNotes()` asks it about
  each, at random gaps, discards the spent ones and keeps the rest, which
  shows the mint those notes one after another. Deriving a backup's key
  takes 64 MiB of memory and about a quarter of a second, and an empty
  passphrase is refused.
- `Client` makes one call per route and rejects with a `MintError` carrying
  the mint's code (`spent`, `frozen`, `paused`…); `isTransient(error)` says
  whether trying again may work. It counts `layer_full` out: a transfer
  refused so is refused every time, though a mint request may pass after
  the next rotation. What the SDK refuses itself is a `WorglError`, with a
  code of its own (see [Errors](#errors)).
- Stores: `MemoryStore`; `FileStore` from `worgl/node`, which replaces
  its file atomically, readable by its owner only; and `IndexedDbStore`
  from `worgl/browser` (see [In a browser](#in-a-browser)). The JSON holds
  every note's secret key, so keep it private and encrypted at rest.
  Implement `Store` to keep it elsewhere.
- Several purses can share a store that has a `lock`, as tabs of one
  browser share an `IndexedDbStore`. Before each change a purse takes the
  lock, reads what is saved, makes the change to that and saves before
  letting go, so none saves over a note another took in; it never holds
  the lock across a request to the mint, and `settle` resends what is
  saved now. Reads such as `notes` and `balance` show what the purse last
  read or saved. `MemoryStore` and `FileStore` have no lock, so give each
  purse one of its own; a store that wraps another passes its `lock` on.

Importing the package loads the WebAssembly module: from disk under Node,
through `fetch` relative to the module in a browser or bundler. It needs
Node 22.18 or later, or a browser with top-level `await`.

## In a browser

```ts
import { Client, Purse } from 'worgl';
import { IndexedDbStore } from 'worgl/browser';

const server = 'https://mint.example.com';
const purse = await Purse.open(new Client(server), { store: new IndexedDbStore(server) });
```

`IndexedDbStore` keeps a purse in the browser's IndexedDB, in the database
`worgl:<name>`. Name each store after its server, as above, and the purses
of several servers live side by side. Each save is one transaction with
strict durability: it resolves only once the browser has flushed it to
disk, not once it has handed it to the operating system, which a power cut
can undo and which Chrome does by default.

Every tab and worker of the page's origin can open a purse on the store,
and purses can share one `IndexedDbStore`. Each change takes the Web Lock
of the same name, `worgl:<name>`, so an application that holds a lock of
its own, such as for the one tab that keeps notes alive, names it
otherwise, or every change waits for it. A change that loses the lock
meanwhile, as when another tab takes it with `steal`, saves nothing and
rejects with `store_changed`, as does one whose save finds something saved
since the change read the store; the change is undone, and the next one
reads the store again. A submit keeps what the mint answered all the same:
it makes that change again on what is saved now, since a resend gets an
answer back only while the mint keeps it. The store saves only under its
lock, as a purse does: a `save` outside it throws a `DOMException` named
`InvalidStateError`.

Web Locks exist only in a secure context, so the page must be served over
https or from localhost; elsewhere `new IndexedDbStore` throws a
`DOMException` named `NotSupportedError`. A save the browser refuses
rejects with its `DOMException`, such as a `QuotaExceededError` when the
origin is out of space, and the change is undone. An open that another tab
blocks, holding the database open in an older version, rejects with
`store_blocked` rather than waiting. What the store holds lasts until the
site's data is cleared or a private window closes, and the browser may
clear it to free space unless the page holds persistent storage
(`navigator.storage.persist()`).

## Following a submit

`submit`, and `receive`, `settle` and `refresh`, which submit too, take
options for a screen that shows a payment or a refresh as it goes:

```ts
const rest = new AbortController();  // the "Send the rest now" button calls rest.abort()
const leave = new AbortController();
addEventListener('pagehide', () => leave.abort());

const submitted = await alice.submit(payment.transfers, {
  onProgress: ({ step, answered, refused, total, sendAt }) => {
    // Draws "2 of 3", and while a transfer is waiting, when it goes.
  },
  sendNow: rest.signal,
  signal: leave.signal,
});
```

- `onProgress` hears of each transfer, in the order they go, as it is
  `waiting` for its time (until `sendAt`, in `Date.now()` milliseconds),
  `sending`, then `answered` or `refused`, or `unsent`, with the counts so
  far. A `refused` one carries its entry of `submitted.refused` as
  `refusal`, whose `outcome` tells a transfer kept for a later `settle`
  from one dropped or cancelled for good. The purse goes on past an
  exception it throws and reports it as uncaught, as an event listener's
  is: a browser logs it, and Node ends the process unless the application
  handles `uncaughtException`.
- `sendNow` cuts the waits short once it aborts: the transfers left go one
  after another. That gives up the spacing that keeps the mint from linking
  their notes by when they arrive (see the
  [Privacy model](../../site/src/content/docs/docs/privacy.md#timing)), so
  offer it rather than make it the default. Retries after a failure still
  wait.
- `signal` stops the call once it aborts: no wait goes on, nothing more is
  sent, and it resolves with what was done. A transfer on its way is
  treated as one whose answer was lost, since the mint may have applied it:
  it is in `submitted.refused` with outcome `kept` and the signal's reason
  as its error. An answer the mint has given already is still checked and
  kept, since a resend gets it again only while the mint keeps it, so the
  stop takes effect from the next transfer. The transfers not sent are in
  `submitted.unsent`. Those the purse made stay outgoing, so `settle` sends
  them again later, and the mint answers one it applied with the same notes
  while it keeps its answer. A change being saved is saved first, so the
  purse is never left half-saved. A `refresh` stopped before it has read the
  mint's info plans nothing.

Every `Client` call takes `{ signal }` too, and rejects with the signal's
reason once it aborts.

## Errors

A refusal from the mint rejects with a `MintError`, which carries the mint's
code (see
[Error codes](../../site/src/content/docs/docs/error-codes.md)). What the SDK
refuses itself, in its Rust core or around it, such as a payment the purse
cannot make, throws or rejects with a `WorglError`: an `Error` named
`WorglError`, with a stable `code` and that code's fields in `details`.
`message` is for people and may change. `isWorglError(error)` tells one
apart, and checking `code` then gives `details` their type:

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
| `store_blocked` | | `Purse.open`, or a change, on an `IndexedDbStore`: another tab holds its database open in an older version and did not close it when asked to. The next call tries again. |
| `store_changed` | | A change of a purse on an `IndexedDbStore`: the change lost the store's lock while it was made, as when another tab took it with `steal`, or another writer saved since the change read the store. The change is undone, and the next one reads the store again. |
| `wrong_passphrase` | | `Backup.open`: the passphrase is wrong, or the backup was altered after it was sealed: the two look the same. |
| `not_sealed` | | `Backup.open`: the text is not a backup or purse file. |
| `unknown_version` | `version` | `Backup.open`: the backup is of `version`, a newer one this SDK does not read. |
| `damaged` | | `Backup.open`: a part of the backup does not read, such as its salt, settings or ciphertext, or it opened to no purse. The message says which. |
| `out_of_memory` | | `Backup.open`, `backup`: too little memory to derive the key: 64 MiB, or up to 256 MiB for a backup whose settings ask for more. |
| `no_randomness` | | `backup`: the system's random number generator failed. |
| `empty_passphrase` | | `backup`: the passphrase is empty, which would let anyone who gets the backup spend its notes. |
| `unknown_format` | `format` | `Purse.open`, `Backup.open`: the saved purse, or the one a backup holds, is in `format`, a newer one this SDK does not read. |
| `malformed` | | Any call: something it was given is not the JSON it takes, such as a payment request, a payment, notes, a mint's info or keys, or, for `Purse.open`, a saved purse; or a transfer spends no notes. |
| `no_layers` | | Any call that reads a mint's info: the info lists no layers. |

## Building and testing

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked  # the version Cargo.lock pins
cargo build -p worgl-server -p worgl-faucet -p worgl-cli   # the tests run against them
npm ci
npm run build   # compiles the Rust core to wasm/ and the package to dist/
npm run check   # type-checks the package and its tests
npm test        # runs the tests against a real server on a local port
```

The tests of `IndexedDbStore` run under Node, on an IndexedDB in memory
(`fake-indexeddb`) and Web Locks as a browser grants them. What only a
browser shows, such as durability on disk, quota and private windows, they
do not cover.

The package is not published.
