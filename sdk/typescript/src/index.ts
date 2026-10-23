/**
 * Hold, pay and receive Worgl notes from TypeScript.
 *
 * The privacy rules and all signing run in the same Rust core as the Rust
 * SDK, compiled to WebAssembly; this package moves its JSON between the
 * holder, the server and a store. Amounts are `bigint`.
 *
 * @example
 * ```ts
 * import { Client, Purse } from 'worgl';
 * import { FileStore } from 'worgl/node';
 *
 * const client = new Client('https://mint.example.com');
 * const alice = await Purse.open(client, { store: new FileStore('alice.json') });
 * const bob = await Purse.open(client, { store: new FileStore('bob.json') });
 *
 * const request = await bob.request(mint, 5n);        // Bob → Alice: request.json
 * const payment = await alice.pay(request);           // Alice → Bob: payment.json
 * const received = await bob.receive(request, payment);
 * await alice.accept(received.theirs);                // Bob → Alice: her change
 * ```
 *
 * @module
 */

export {
  Client,
  type ClientOptions,
  type FaucetInfo,
  type MintInfo,
  MintError,
  type NoteState,
  isTransient,
} from './client.ts';
export {
  type Balance,
  DEFAULT_RETRY,
  DEFAULT_SPACING,
  MAX_DELAY_SECS,
  type MadePayment,
  NO_SPACING,
  type Outcome,
  Payment,
  PaymentRequest,
  Purse,
  type PurseOptions,
  type Refused,
  type Retry,
  type ScheduledTransfer,
  type Spacing,
  type Submitted,
} from './purse.ts';
export {
  type WorglError,
  type WorglErrorCode,
  type WorglErrorDetails,
  isWorglError,
} from './error.ts';
export { MemoryStore, type Store } from './store.ts';
export { Holder } from './core.ts';
