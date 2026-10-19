// The TypeScript SDK against a real server on a local port.

import assert from 'node:assert/strict';
import { mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer as createHttpServer } from 'node:http';
import { type AddressInfo, createServer } from 'node:net';
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

/** A day, in seconds: a period whose rotations, at midnight UTC, no test waits for. */
const DAY = 86_400;

/**
 * Serves `server`'s mint info as a machine whose clock is `skew` seconds
 * behind the server's would read it: the next rotation and the answer's
 * `Date` move on by `skew`, which is negative for a clock ahead. It serves
 * nothing else.
 */
async function skewed(
  server: TestServer,
  skew: number,
): Promise<{ client: Client; close: () => Promise<void> }> {
  const stub = createHttpServer(async (_, response) => {
    const info = await server.client().infoText(server.mint);
    const moved = info.replace(
      /"next_at":(\d+)/,
      (_, at: string) => `"next_at":${BigInt(at) + BigInt(skew)}`,
    );
    response.writeHead(200, {
      'content-type': 'application/json',
      date: new Date(Date.now() + skew * 1000).toUTCString(),
    });
    response.end(moved);
  });
  await new Promise<void>((resolve) => stub.listen(0, '127.0.0.1', resolve));
  const { port } = stub.address() as AddressInfo;
  return {
    client: new Client(`http://127.0.0.1:${port}`),
    close: () => new Promise((resolve) => stub.close(() => resolve())),
  };
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
    // Two receipts stand in for a mint whose receipts many transfers pushed out.
    const own = await TestServer.start({ receipt_capacity: 2 });
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
      // The note is gone either way, so the transfer is dropped; its change
      // keys stay.
      assert.equal(refused.outcome, 'dropped');
      const dropped = await alice.balance(own.mint);
      assert.deepEqual([dropped.outgoing, dropped.expected, dropped.unconfirmed], [0n, 0n, 5n]);
      assert.deepEqual(alice.outgoing(), []);

      // A forged note is refused and takes nothing in.
      const genuine = received.theirs[0]!;
      const forged = genuine.slice(0, 190) + (genuine[190] === 'A' ? 'B' : 'A') + genuine.slice(191);
      await assert.rejects(alice.accept([forged]), /signature/);
      assert.equal((await alice.balance(own.mint)).unconfirmed, 5n);

      // The change Bob hands back is still taken in.
      assert.equal(await alice.accept(received.theirs), 5n);
      const balance = await alice.balance(own.mint);
      assert.deepEqual(
        [balance.frozen, balance.outgoing, balance.expected, balance.unconfirmed],
        [5n, 0n, 0n, 0n],
      );
    } finally {
      await own.stop();
    }
  });

  test('a request for a note the mint would not make is refused before signing', async () => {
    const store = new MemoryStore();
    const [alice, bob] = [await purse(store), await purse()];
    await server.fund(alice, 8n, 8n);
    const saved = store.saved;

    const asked = JSON.parse((await bob.request(server.mint, 4n)).json) as {
      outputs: { amount: number }[];
    };
    asked.outputs[0]!.amount = 3;
    const request = PaymentRequest.from(JSON.stringify(asked));
    await assert.rejects(alice.pay(request, NO_SPACING), /a note of 3, which is not one of/);
    // Nothing is parked: no transfer to resend, nothing saved.
    const balance = await alice.balance(server.mint);
    assert.deepEqual([balance.spendable, balance.outgoing, balance.expected], [8n, 0n, 0n]);
    assert.deepEqual(alice.outgoing(), []);
    assert.deepEqual(alice.payments(), []);
    assert.equal(store.saved, saved);
    const settled = await alice.settle(NO_SPACING);
    assert.deepEqual([settled.answered, settled.refused.length], [0, 0]);
  });

  test('a transfer another server does not know stays outgoing', async () => {
    const other = await TestServer.start();
    try {
      const store = new MemoryStore();
      const [alice, bob] = [await purse(store), await purse()];
      await server.fund(alice, 8n, 8n);
      const request = await bob.request(server.mint, 5n);
      await alice.pay(request, NO_SPACING);

      // Pointed at a server that is not the mint's, the purse keeps it.
      const lost = await Purse.open(other.client(), { store, ...quick });
      const settled = await lost.settle(NO_SPACING);
      assert.equal(settled.refused.length, 1);
      const refused = settled.refused[0]!;
      assert.ok(refused.error instanceof MintError);
      assert.equal(refused.error.code, 'unknown_mint');
      assert.equal(refused.outcome, 'kept');
      assert.equal(lost.outgoing().length, 1);

      // The mint's own server still answers it.
      const back = await purse(store);
      allAnswered(await back.settle(NO_SPACING), 1);
      assert.equal((await back.balance(server.mint)).frozen, 3n);
    } finally {
      await other.stop();
    }
  });

  test('a refusal the mint would not give drops or cancels nothing', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 8n);
    const request = await bob.request(server.mint, 5n);
    await alice.pay(request, NO_SPACING);

    // The mint answers `spent` and `expired` with 409, and never answers
    // `unauthorized` or `duplicate_input` to a transfer a holder signed.
    const answers = [
      [401, 'spent'],
      [401, 'expired'],
      [403, 'unauthorized'],
      [400, 'duplicate_input'],
    ] as const;
    for (const [status, code] of answers) {
      // As a gateway or a server of another version might answer.
      const stub = createHttpServer((_, response) => {
        response.writeHead(status, { 'content-type': 'application/json' });
        response.end(JSON.stringify({ error: code, message: 'sign in first' }));
      });
      await new Promise<void>((resolve) => stub.listen(0, '127.0.0.1', resolve));
      try {
        const { port } = stub.address() as AddressInfo;
        const lost = await Purse.open(new Client(`http://127.0.0.1:${port}`), {
          store: new MemoryStore(alice.toJson()),
          ...quick,
        });
        const settled = await lost.settle(NO_SPACING);
        assert.equal(settled.refused.length, 1);
        const refused = settled.refused[0]!;
        assert.ok(refused.error instanceof MintError);
        assert.equal(refused.error.code, code);
        assert.equal(refused.outcome, 'kept', code);
        assert.deepEqual([lost.outgoing().length, lost.notes().length], [1, 0], code);
      } finally {
        await new Promise((resolve) => stub.close(resolve));
      }
    }

    // The mint's own server still answers it.
    allAnswered(await alice.settle(NO_SPACING), 1);
    assert.equal((await alice.balance(server.mint)).frozen, 3n);
  });

  test('a transfer the policy now refuses is cancelled', async () => {
    const own = await TestServer.start();
    try {
      const open = () => Purse.open(own.client(), quick);
      const [alice, bob] = [await open(), await open()];
      await own.fund(alice, 8n, 8n);
      const request = await bob.request(own.mint, 5n);
      const payment = await alice.pay(request, NO_SPACING);

      // The operator allows one output per transfer from now on.
      await own.restart({ max_outputs: 1 });
      const settled = await alice.settle(NO_SPACING);
      assert.equal(settled.refused.length, 1);
      const refused = settled.refused[0]!;
      assert.ok(refused.error instanceof MintError);
      assert.equal(refused.error.code, 'policy');
      assert.equal(refused.outcome, 'cancelled');
      // The note is back; the change keys stay in case the transfer went
      // through under the old policy.
      const balance = await alice.balance(own.mint);
      assert.deepEqual([balance.spendable, balance.outgoing, balance.unconfirmed], [8n, 0n, 3n]);
      assert.equal(alice.notes().length, 1);
      assert.deepEqual(alice.payments(), []);
      // Bob's submission is refused the same way.
      const received = await bob.receive(request, payment);
      assert.equal(received.refused[0]?.outcome, 'kept');
    } finally {
      await own.stop();
    }
  });

  test('a settle that carries out a payment keeps the payee\'s notes', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 8n);
    const request = await bob.request(server.mint, 5n);
    const payment = await alice.pay(request, NO_SPACING);
    const made = alice.payments();
    assert.equal(made.length, 1);
    assert.equal(made[0]!.request.json, request.json);
    assert.equal(made[0]!.payment.json, payment.json);
    assert.deepEqual(made[0]!.owed, []);

    // Bob never submits; Alice's settle carries the payment out.
    const settled = await alice.settle(NO_SPACING);
    allAnswered(settled, 1);
    assert.equal(settled.value, 3n);
    const owed = alice.payments()[0]!.owed;
    assert.deepEqual(owed, settled.theirs);
    assert.equal(owed.length, request.notes.length);

    // Handed to Bob, who keeps them, and then no longer kept by Alice.
    assert.equal(await bob.accept(owed), 5n);
    assert.equal(await alice.forgetOwed(owed), 5n);
    assert.deepEqual(alice.payments(), []);
    assert.equal(await alice.forgetOwed(owed), 0n);
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

describe("a clock off the mint's", () => {
  // Its own server, whose mint rotates once a day besides on request.
  let own: TestServer;
  before(async () => {
    own = await TestServer.start({}, DAY);
  });
  after(async () => {
    await own.stop();
  });

  test('ahead of it, drops nothing live', async () => {
    const alice = await Purse.open(own.client(), quick);
    await own.fund(alice, 12n, 8n);
    await own.rotate();
    await own.rotate();
    const notes = alice.notes();

    // Ten periods ahead, the clock has passed the next rotation by far, but
    // the mint has not made it.
    const stub = await skewed(own, -10 * DAY);
    try {
      const ahead = await Purse.open(stub.client, {
        store: new MemoryStore(alice.toJson()),
        ...quick,
      });
      assert.equal(await ahead.prune(own.mint), 0n);
      assert.equal(ahead.notes().length, 2);
      assert.equal((await ahead.balance(own.mint)).expiring, 12n);
      for (const note of notes) {
        assert.equal((await own.client().verify(note)).live?.position, 0);
      }

      // They die at the next rotation the mint makes, so they are
      // refreshed, and the mint takes the refreshes.
      const plan = await ahead.planRefresh(own.mint);
      assert.equal(plan.length, 2);
      const back = await Purse.open(own.client(), {
        store: new MemoryStore(ahead.toJson()),
        ...quick,
      });
      const refreshed = await back.submit(now(plan));
      allAnswered(refreshed, 2);
      assert.equal(refreshed.value, 12n);
    } finally {
      await stub.close();
    }
  });

  test('behind it, still refreshes before the rotation', async () => {
    const alice = await Purse.open(own.client(), quick);
    await own.fund(alice, 8n, 2n);
    await own.rotate();
    await own.rotate();

    // A year behind, the clock would put the next rotation a year further.
    const stub = await skewed(own, 365 * DAY);
    try {
      const behind = await Purse.open(stub.client, {
        store: new MemoryStore(alice.toJson()),
        ...quick,
      });
      const plan = await behind.planRefresh(own.mint);
      const nextAt = (await own.client().info(own.mint)).rotation?.nextAt ?? 0n;
      const left = Number(nextAt) - Math.floor(Date.now() / 1000);
      const times = plan.map((transfer) => transfer.afterSecs);
      assert.equal(times.length, 4);
      assert.ok(
        times.every((after) => after < left),
        `${times.join(', ')} with ${left} s left`,
      );
    } finally {
      await stub.close();
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
