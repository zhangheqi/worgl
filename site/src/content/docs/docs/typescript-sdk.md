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
await alice.settle();                                 // later: Alice takes in her change
```

The other side reads what it is sent with `PaymentRequest.from(json)` and
`Payment.from(json)`. For a fuller worked example, the SDK's tests in
[`sdk/typescript/test/purse.test.ts`](https://github.com/zhangheqi/worgl/blob/main/sdk/typescript/test/purse.test.ts)
run purses against a real server.

## Purse

- `Purse` reads mint info itself, submits each transfer at its time, retries
  what fails on the way (a resent transfer gets the same answer) and saves
  after every change, before handing out anything that depends on it. If a
  save fails, the change is undone and the call rejects. After a `busy`
  answer it waits as long as the mint's `Retry-After` asks, up to 30
  seconds, before the next try.
- `purse.refresh(mint)` pays the notes about to expire back to the purse,
  each at a random moment before the next rotation. `planRefresh` and
  `submit` split it for applications with their own scheduler.
- When the payer submits instead (`purse.submit(payment.transfers)`), it
  hands `submitted.notes` to the payee, who checks their signatures against
  the mint's keys and keeps them with `purse.accept(notes)`.
- `purse.takeFromFaucet(new Client(faucetUrl))` asks a playground
  [faucet](../running-a-faucet/) for a note, to a fresh key like any other,
  and keeps it.

## Client and errors

`Client` makes one call per route and rejects with a `MintError` carrying the
mint's code (`spent`, `frozen`, `paused`…). `isTransient(error)` says whether
trying again may work, and `retryAfterMs` how long a `busy` mint asked to
wait first. The codes are listed in [Error codes](../error-codes/).

## Stores

`MemoryStore`, and `FileStore` from `worgl/node`, which replaces its file
atomically, readable by its owner only. The JSON holds every note's secret
key, so keep it private and encrypted at rest. Implement `Store` to keep it
elsewhere.

## Testing it yourself

The tests run against a real server on a local port, so build the server
first:

```sh
cargo build -p worgl-server -p worgl-faucet   # from the repository root
cd sdk/typescript
npm run check   # type-checks the package and its tests
npm test
```
