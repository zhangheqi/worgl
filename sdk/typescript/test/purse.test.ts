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
