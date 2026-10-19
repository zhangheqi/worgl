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
await alice.settle();                                 // later: Alice takes in her change
```

The other side reads what it is sent with `PaymentRequest.from(json)` and
`Payment.from(json)`.

- `Purse` reads mint info itself, submits each transfer at its time, retries
  what fails on the way (a resent transfer gets the same answer) and saves
  after every change, before handing out anything that depends on it. If a
  save fails, the change is undone and the call rejects.
- `purse.refresh(mint)` pays the notes about to expire back to the purse,
  each at a random moment before the next rotation. `planRefresh` and
  `submit` split it for applications with their own scheduler.
- When the payer submits instead (`purse.submit(payment.transfers)`), it
  hands `submitted.notes` to the payee, who checks their signatures against
  the mint's keys and keeps them with `purse.accept(notes)`.
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
