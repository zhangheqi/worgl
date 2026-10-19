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
  save fails, the change is undone and the call rejects.
- `purse.refresh(mint)` pays the notes about to expire back to the purse,
  each at a random moment before the next rotation, and resends the
  outgoing transfers that must land before it. `planRefresh` and `submit`
  split it for applications with their own scheduler. The moments are
  counted by the server's clock, from the `Date` of its info answer
  (`client.infoTextAndTime`), so this machine's clock, slow or fast, moves
  none past the rotation. A browser reads that `Date` from a mint on
  another origin only if the answer carries
  `Access-Control-Expose-Headers: Date`, so the proxy that adds the CORS
  headers must send it too; without it, the browser's clock counts the
  moments. `purse.prune(mint)` drops the notes whose layer the mint's info
  says it has dropped, never by this machine's clock.
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
  dropped, since its note is gone, and one refused as `policy` that the
  policy the mint publishes refuses too is cancelled and its note taken
  back; both keep the keys of its change, so `accept` still takes in change
  handed over later. Any other refusal leaves it outgoing.
  `purse.cancel(transfer)` and `purse.forget(transfer)` do the same by hand,
  for a transfer from `purse.outgoing()`, which lists those not yet answered.
- `purse.pay` refuses, before signing, a request for a note the mint does
  not make, or to a key the purse has paid already, such as one request
  twice. `purse.payments()` lists the payments it made and keeps, each with
  the payment to send again, less any transfer cancelled or forgotten, and
  `owed`, the payee's notes from every answer the purse got itself by
  settling, refreshing or submitting; `purse.forgetOwed(notes)` stops
  keeping them once they are handed over.
- `purse.takeFromFaucet(new Client(faucetUrl))` asks a playground faucet
  (`worgl-faucet`) for a note, to a fresh key like any other, and keeps it.
- `Client` makes one call per route and rejects with a `MintError` carrying
  the mint's code (`spent`, `frozen`, `paused`…); `isTransient(error)` says
  whether trying again may work.
- Stores: `MemoryStore`, and `FileStore` from `worgl/node`, which replaces
  its file atomically, readable by its owner only. The JSON holds every
  note's secret key, so keep it private and encrypted at rest. Implement
  `Store` to keep it elsewhere.

Importing the package loads the WebAssembly module: from disk under Node,
through `fetch` relative to the module in a browser or bundler. It needs
Node 22.18 or later, or a browser with top-level `await`.

## Building and testing

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked  # the version Cargo.lock pins
cargo build -p worgl-server -p worgl-faucet   # the tests run against them
npm ci
npm run build   # compiles the Rust core to wasm/ and the package to dist/
npm run check   # type-checks the package and its tests
npm test        # runs the tests against a real server on a local port
```

The package is not published.
