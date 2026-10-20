// The TypeScript SDK against a real server on a local port.

import assert from 'node:assert/strict';
import { mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, describe, test } from 'node:test';

import {
  Client,
  Holder,
  MemoryStore,
  MintError,
  NO_SPACING,
  Payment,
  PaymentRequest,
  Purse,
  type ScheduledTransfer,
  type Store,
  type Submitted,
  isTransient,
} from '../src/index.ts';
import { FileStore } from '../src/node.ts';
import { TestServer } from './server.ts';

/** Fast retries, so a refused transfer does not hold a test up. */
const quick = { retry: { attempts: 2, firstDelayMs: 10 } };

function allAnswered(submitted: Submitted, count: number): void {
  assert.deepEqual(submitted.refused, []);
  assert.equal(submitted.answered, count);
}

function now(transfers: ScheduledTransfer[]): ScheduledTransfer[] {
  return transfers.map((transfer) => ({ ...transfer, afterSecs: 0 }));
}

let server: TestServer;
before(async () => {
  server = await TestServer.start();
});
after(async () => {
  await server.stop();
});

async function purse(store: Store = new MemoryStore()): Promise<Purse> {
  return Purse.open(server.client(), { store, ...quick });
}

describe('client', () => {
  test('reads info and verifies notes', async () => {
    const client = server.client();
    const info = await client.info(server.mint);
    assert.equal(info.issuer, server.mint);
    assert.equal(info.layers, 4);
    assert.equal(info.paused, false);
    assert.equal(info.rotation, undefined);
    assert.equal(typeof info.supply.minted, 'bigint');

    const alice = await purse();
    const request = await alice.request(server.mint, 8n);
    const note = await server.mintTo(8n, request.notes[0]!.holder);
    const state = await client.verify(note);
    assert.equal(state.state, 'live');
    assert.equal(state.amount, 8n);
    assert.equal(state.live?.position, 2);
  });

  test('errors keep the mint code', async () => {
    const client = server.client();
    await assert.rejects(client.rotate(server.mint, 'wrong'), (error: unknown) => {
      assert.ok(error instanceof MintError);
      assert.equal(error.code, 'admin_unauthorized');
      assert.equal(isTransient(error), false);
      return true;
    });
    await assert.rejects(client.info('00'.repeat(32)), { code: 'unknown_mint' });
    await assert.rejects(new Client(`${server.url}/nowhere/`).info(server.mint), {
      code: 'http',
      status: 404,
    });
    assert.throws(() => new Client('ftp://example.com'), TypeError);

    const port = await new Promise<number>((resolve) => {
      const probe = createServer().listen(0, '127.0.0.1', () => {
        const address = probe.address();
        probe.close(() => resolve(typeof address === 'object' && address ? address.port : 0));
      });
    });
    await assert.rejects(new Client(`http://127.0.0.1:${port}`).info(server.mint), (error) =>
      isTransient(error),
    );
  });
});

describe('purse', () => {
  test('a payment goes through the server', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 8n);

    // The request and payment travel as JSON between the two sides.
    const request = PaymentRequest.from((await bob.request(server.mint, 5n)).json);
    assert.equal(request.amount, 5n);
    const payment = Payment.from((await alice.pay(request, NO_SPACING)).json);
    assert.equal((await alice.balance(server.mint)).outgoing, 8n);

    const received = await bob.receive(request, payment);
    allAnswered(received, 1);
    assert.equal(received.value, 5n);

    const settled = await alice.settle(NO_SPACING);
    allAnswered(settled, 1);
    assert.equal(settled.value, 3n);
    assert.deepEqual(settled.notes, received.notes);

    assert.equal((await bob.balance(server.mint)).frozen, 5n);
    await server.rotate();
    assert.equal((await bob.balance(server.mint)).spendable, 5n);
    const balance = await alice.balance(server.mint);
    assert.equal(balance.spendable, 3n);
    assert.equal(balance.outgoing, 0n);
  });

  test('the payer can submit and hand the notes over', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 4n);
    const request = await bob.requestAtMost(server.mint, 6n, 2n);
    const payment = await alice.pay(request, NO_SPACING);
    assert.equal(payment.transfers.length, 2);

    const submitted = await alice.submit(payment.transfers);
    allAnswered(submitted, 2);
    assert.equal(submitted.value, 2n);

    // A note whose signature was tampered with is refused.
    const genuine = submitted.notes[0]!;
    const forged = genuine.slice(0, 190) + (genuine[190] === 'A' ? 'B' : 'A') + genuine.slice(191);
    await assert.rejects(bob.accept([forged]), /signature/);
    assert.equal((await bob.balance(server.mint)).frozen, 0n);

    assert.equal(await bob.accept(submitted.notes), 6n);
    assert.equal(await bob.accept(submitted.notes), 0n);
    // What Alice hands over is Bob's notes only, none of her change.
    assert.equal(submitted.theirs.length, request.notes.length);
  });

  test('the payee hands back change the mint no longer answers for', async () => {
    // Two receipts, kept no minimum time, stand in for a mint whose receipts
    // many transfers pushed out.
    const own = await TestServer.start({ receipt_capacity: 2, receipt_min_age_secs: 0 });
    try {
      const open = () => Purse.open(own.client(), quick);
      const [alice, bob, carol] = [await open(), await open(), await open()];
      await own.fund(alice, 8n, 8n);
      await own.fund(carol, 3n, 1n);
      const request = await bob.request(own.mint, 3n);
      const received = await bob.receive(request, await alice.pay(request, NO_SPACING));
      allAnswered(received, 1);
      assert.equal(received.value, 3n);
      assert.equal(received.theirs.length, received.notes.length - request.notes.length);

      // Other holders' transfers push Alice's receipt out.
      for (let i = 0; i < 3; i += 1) {
        const dave = await open();
        const asked = await dave.request(own.mint, 1n);
        allAnswered(await dave.receive(asked, await carol.pay(asked, NO_SPACING)), 1);
      }
      const settled = await alice.settle(NO_SPACING);
      assert.equal(settled.answered, 0);
      assert.equal(settled.refused.length, 1);
      const refused = settled.refused[0]!;
      assert.ok(refused.error instanceof MintError);
      assert.equal(refused.error.code, 'spent');
      assert.equal(refused.atStake, 5n);

      // A forged note is refused and settles nothing.
      const genuine = received.theirs[0]!;
      const forged = genuine.slice(0, 190) + (genuine[190] === 'A' ? 'B' : 'A') + genuine.slice(191);
      await assert.rejects(alice.accept([forged]), /signature/);
      assert.equal((await alice.balance(own.mint)).outgoing, 8n);

      // The change Bob hands back recovers it and settles the transfer.
      assert.equal(await alice.accept(received.theirs), 5n);
      const balance = await alice.balance(own.mint);
      assert.deepEqual([balance.frozen, balance.outgoing, balance.expected], [5n, 0n, 0n]);
    } finally {
      await own.stop();
    }
  });

  test('a payment for another request is not submitted', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 8n);
    const asked = await bob.request(server.mint, 4n);
    const other = await bob.request(server.mint, 4n);
    const payment = await alice.pay(other, NO_SPACING);
    await assert.rejects(bob.receive(asked, payment));
    assert.equal((await alice.balance(server.mint)).outgoing, 8n);
  });

  test('a paused mint is retried, reported, then paid', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 8n);
    const request = await bob.request(server.mint, 8n);
    const payment = await alice.pay(request, NO_SPACING);

    const client = server.client();
    await client.pause(server.mint, server.token);
    try {
      const refused = await bob.receive(request, payment);
      assert.equal(refused.answered, 0);
      assert.equal(refused.refused.length, 1);
      const error = refused.refused[0]!.error;
      assert.ok(error instanceof MintError);
      assert.equal(error.code, 'paused');
      assert.ok(isTransient(error));
    } finally {
      await client.resume(server.mint, server.token);
    }
    const paid = await bob.receive(request, payment);
    allAnswered(paid, 1);
    assert.equal(paid.value, 8n);
  });

  test('a busy answer stretches the next wait only', async () => {
    // The mint's first two answers to the transfer: busy, asking for a
    // second's wait, then without a leader, asking for the same. The third
    // try reaches it.
    const scripted = [
      { status: 503, error: 'busy', retryAfter: '1' },
      { status: 503, error: 'no_leader', retryAfter: '1' },
    ];
    const fetch: typeof globalThis.fetch = (input, init) => {
      const answer = String(input).endsWith('/v1/transfer') ? scripted.shift() : undefined;
      if (answer === undefined) {
        return globalThis.fetch(input, init);
      }
      const headers = new Headers({ 'content-type': 'application/json' });
      if (answer.retryAfter !== undefined) {
        headers.set('retry-after', answer.retryAfter);
      }
      const body = JSON.stringify({ error: answer.error, message: 'scripted' });
      return Promise.resolve(new Response(body, { status: answer.status, headers }));
    };
    const client = new Client(server.url, { fetch });
    const alice = await purse();
    const bob = await Purse.open(client, { retry: { attempts: 3, firstDelayMs: 10 } });
    await server.fund(alice, 8n, 8n);
    const request = await bob.request(server.mint, 8n);
    const payment = await alice.pay(request, NO_SPACING);

    const started = Date.now();
    const received = await bob.receive(request, payment);
    const waited = Date.now() - started;
    allAnswered(received, 1);
    // A second as busy asked, then 20 ms: the second's wait does not
    // double, and no_leader's Retry-After does not stretch it.
    assert.ok(waited >= 1000 && waited < 1900, `waited ${waited} ms`);

    scripted.push({ status: 503, error: 'busy', retryAfter: '7' });
    await assert.rejects(client.transfer(payment.transfers[0]!.request), (error: unknown) => {
      assert.ok(error instanceof MintError);
      assert.equal(error.code, 'busy');
      assert.equal(error.retryAfterMs, 7000);
      assert.ok(isTransient(error));
      return true;
    });
  });

  test('nothing is handed out unless it is saved', async () => {
    let failing = false;
    const inner = new MemoryStore();
    const flaky: Store = {
      load: () => inner.load(),
      save: async (json) => {
        if (failing) {
          throw new Error('disk full');
        }
        await inner.save(json);
      },
    };
    const [alice, bob] = [await purse(flaky), await purse()];
    await server.fund(alice, 8n, 8n);
    const request = await bob.request(server.mint, 5n);
    const saved = inner.saved;

    failing = true;
    await assert.rejects(alice.pay(request, NO_SPACING), /disk full/);
    await assert.rejects(alice.request(server.mint, 2n), /disk full/);
    const balance = await alice.balance(server.mint);
    assert.deepEqual(
      [balance.spendable, balance.outgoing, balance.expected],
      [8n, 0n, 0n],
    );
    assert.equal(inner.saved, saved);
  });
});

describe('refresh', () => {
  test('keeps notes alive across rotations', async () => {
    // Its own server, since it rotates the mint three times.
    const own = await TestServer.start();
    try {
      const open = () => Purse.open(own.client(), quick);
      const [kept, idle] = [await open(), await open()];
      await own.fund(kept, 4n, 4n);
      await own.fund(idle, 4n, 4n);
      await own.rotate();
      await own.rotate();

      assert.equal((await kept.balance(own.mint)).expiring, 4n);
      const plan = await kept.planRefresh(own.mint);
      assert.equal(plan.length, 1);
      const refreshed = await kept.submit(now(plan));
      allAnswered(refreshed, 1);
      assert.equal(refreshed.value, 4n);

      await own.rotate();
      assert.equal((await kept.balance(own.mint)).spendable, 4n);
      assert.equal(await kept.prune(own.mint), 0n);
      assert.equal(await idle.prune(own.mint), 4n);
      assert.deepEqual(idle.notes(), []);
    } finally {
      await own.stop();
    }
  });

  test('resends what the next rotation would cut off', async () => {
    const own = await TestServer.start();
    try {
      let failing = false;
      const inner = new MemoryStore();
      const flaky: Store = {
        load: () => inner.load(),
        save: async (json) => {
          if (failing) {
            throw new Error('disk full');
          }
          await inner.save(json);
        },
      };
      const alice = await Purse.open(own.client(), { store: flaky, ...quick });
      const bob = await Purse.open(own.client(), quick);
      await own.fund(alice, 12n, 8n);
      await own.rotate();
      await own.rotate();
      // Alice pays with her 4, which dies at the next rotation like her 8.
      const request = await bob.request(own.mint, 3n);
      const payment = await alice.pay(request, NO_SPACING);

      // A plan that fails to save takes back the refresh, not the payment.
      failing = true;
      await assert.rejects(alice.planRefresh(own.mint), /disk full/);
      const balance = await alice.balance(own.mint);
      assert.deepEqual(
        [balance.spendable, balance.outgoing, balance.expected],
        [8n, 4n, 1n],
      );
      failing = false;

      const plan = await alice.planRefresh(own.mint);
      assert.equal(plan.length, 2);
      const done = await alice.submit(now(plan));
      allAnswered(done, 2);
      assert.equal(done.value, 9n);
      assert.equal((await alice.balance(own.mint)).outgoing, 0n);
      // Bob, submitting later, gets the same notes.
      const received = await bob.receive(request, payment);
      allAnswered(received, 1);
      assert.equal(received.value, 3n);
    } finally {
      await own.stop();
    }
  });
});

describe('faucet', () => {
  let playground: TestServer;
  let faucet: Client;
  before(async () => {
    playground = await TestServer.start();
    faucet = await playground.startFaucet();
  });
  after(async () => {
    await playground.stop();
  });

  test('reads what the faucet hands out', async () => {
    const about = await faucet.faucet();
    assert.equal(about.mint, playground.mint);
    assert.equal(about.amount, 64n);
    assert.equal(about.perAddressPerHour, 3);
    assert.equal(about.perDay, 10_000);
    assert.equal(about.leftToday, 10_000);
  });

  test('a purse keeps the notes the faucet hands out', async () => {
    const purse = await Purse.open(playground.client(), quick);
    for (let i = 0; i < 2; i += 1) {
      assert.deepEqual(await purse.takeFromFaucet(faucet), { mint: playground.mint, value: 64n });
    }
    const balance = await purse.balance(playground.mint);
    assert.equal(balance.spendable, 128n);
    assert.equal(balance.expected, 0n);
    assert.equal(purse.notes().length, 2);
  });

  test('a refusal leaves nothing expected', async () => {
    const purse = await Purse.open(playground.client(), quick);
    // The address had two of its three notes above.
    await purse.takeFromFaucet(faucet);
    await assert.rejects(purse.takeFromFaucet(faucet), (error: unknown) => {
      assert.ok(error instanceof MintError);
      assert.equal(error.status, 429);
      assert.equal(error.code, 'rate_limited');
      assert.equal(isTransient(error), false);
      return true;
    });
    const balance = await purse.balance(playground.mint);
    assert.equal(balance.spendable, 64n);
    assert.equal(balance.expected, 0n);
  });
});

describe('file store', () => {
  test('reopens a purse, owner-readable only', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'worgl-store-'));
    try {
      const path = join(dir, 'alice.json');
      const alice = await purse(new FileStore(path));
      assert.deepEqual(alice.notes(), []);
      await server.fund(alice, 8n, 2n);

      const reopened = await purse(new FileStore(path));
      assert.equal((await reopened.balance(server.mint)).spendable, 8n);
      assert.deepEqual(readdirSync(dir), ['alice.json']);
      if (process.platform !== 'win32') {
        assert.equal(statSync(path).mode & 0o777, 0o600);
      }

      writeFileSync(path, '{');
      await assert.rejects(purse(new FileStore(path)));
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  test('a holder round-trips through JSON', () => {
    const holder = new Holder();
    const copy = Holder.fromJson(holder.toJson());
    assert.equal(copy.toJson(), holder.toJson());
    assert.throws(() => Holder.fromJson('{"format":9,"notes":[],"expected":[],"outgoing":[]}'));
  });
});
