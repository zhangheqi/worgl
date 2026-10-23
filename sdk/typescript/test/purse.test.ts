// The TypeScript SDK against a real server on a local port.

import assert from 'node:assert/strict';
import { mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer as createHttpServer } from 'node:http';
import { type AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, describe, test } from 'node:test';

import {
  Client,
  type HeldNote,
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
  type WorglErrorCode,
  isTransient,
  isWorglError,
} from '../src/index.ts';
import { IndexedDbStore } from '../src/browser.ts';
import { FileStore } from '../src/node.ts';
import { keptPort, TestServer } from './server.ts';

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

/**
 * A client of `server` that hands on the notes of an answer to a transfer as
 * `tamper` changes them: a server that is not the mint's, or one between the
 * purse and the mint.
 */
function tampering(server: TestServer, tamper: (notes: string[]) => string[]): Client {
  const fetch: typeof globalThis.fetch = async (input, init) => {
    const answer = await globalThis.fetch(input, init);
    if (!String(input).endsWith('/v1/transfer') || !answer.ok) {
      return answer;
    }
    const { notes } = (await answer.json()) as { notes: string[] };
    return new Response(JSON.stringify({ notes: tamper(notes) }), {
      headers: { 'content-type': 'application/json' },
    });
  };
  return new Client(server.url, { fetch });
}

/**
 * Checks that `lost`, which held what `purse` holds, kept nothing of the one
 * answer in `submitted`, and reported it with an error that `why` matches: a
 * WorglError of `code` when one is given.
 */
function keptNothing(
  lost: Purse,
  purse: Purse,
  submitted: Submitted,
  why: RegExp,
  code?: WorglErrorCode,
): void {
  assert.deepEqual(
    [submitted.answered, submitted.value, submitted.notes, submitted.theirs],
    [0, 0n, [], []],
  );
  assert.equal(submitted.refused.length, 1);
  const refused = submitted.refused[0]!;
  assert.match(String(refused.error), why);
  assert.equal(isWorglError(refused.error) ? refused.error.code : undefined, code);
  assert.equal(isTransient(refused.error), false);
  assert.equal(refused.outcome, 'kept');
  assert.equal(lost.toJson(), purse.toJson());
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
      assert.equal(isWorglError(error), false);
      return true;
    });
    await assert.rejects(client.info('00'.repeat(32)), { code: 'unknown_mint' });
    await assert.rejects(new Client(`${server.url}/nowhere/`).info(server.mint), {
      code: 'http',
      status: 404,
    });
    assert.throws(() => new Client('ftp://example.com'), TypeError);

    const port = await keptPort();
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
    await assert.rejects(bob.accept([forged]), {
      name: 'WorglError',
      code: 'bad_signature',
      details: {},
      message: 'issuer signature does not verify',
    });
    assert.equal((await bob.balance(server.mint)).frozen, 0n);

    assert.equal(await bob.accept(submitted.notes), 6n);
    assert.equal(await bob.accept(submitted.notes), 0n);
    // What Alice hands over is Bob's notes only, none of her change.
    assert.equal(submitted.theirs.length, request.notes.length);
  });

  test('a balance adds up through a payment, a refresh and a cancel', async () => {
    // Its own server, since it rotates the mint twice.
    const own = await TestServer.start();
    try {
      const open = () => Purse.open(own.client(), quick);
      const [alice, bob, carol] = [await open(), await open(), await open()];
      // The parts that never overlap, once the sums are checked against them.
      const parts = async (purse: Purse): Promise<bigint[]> => {
        const b = await purse.balance(own.mint);
        assert.equal(b.held, b.spendable + b.frozen + b.unknown);
        assert.equal(b.total, b.held + b.change + b.refreshing);
        assert.equal(b.outgoing, b.sending + b.change + b.refreshing);
        assert.equal(b.expected, b.requested + b.change + b.refreshing);
        return [b.held, b.sending, b.change, b.refreshing, b.requested, b.unconfirmed];
      };
      await own.fund(alice, 12n, 8n);
      assert.deepEqual(await parts(alice), [12n, 0n, 0n, 0n, 0n, 0n]);

      // Bob asks for 3; Alice pays from her 4 and submits it herself.
      const request = await bob.request(own.mint, 3n);
      assert.deepEqual(await parts(bob), [0n, 0n, 0n, 0n, 3n, 0n]);
      const payment = await alice.pay(request, NO_SPACING);
      assert.deepEqual(await parts(alice), [8n, 3n, 1n, 0n, 0n, 0n]);
      assert.equal((await alice.balance(own.mint)).total, 9n);
      const submitted = await alice.submit(payment.transfers);
      allAnswered(submitted, 1);
      assert.deepEqual(await parts(alice), [9n, 0n, 0n, 0n, 0n, 0n]);
      assert.equal(await bob.accept(submitted.theirs), 3n);
      assert.deepEqual(await parts(bob), [3n, 0n, 0n, 0n, 0n, 0n]);

      // The 8 is about to die; refreshed, it is on its way back.
      await own.rotate();
      await own.rotate();
      const plan = await alice.planRefresh(own.mint);
      assert.equal(plan.length, 1);
      assert.deepEqual(await parts(alice), [1n, 0n, 0n, 8n, 0n, 0n]);
      allAnswered(await alice.submit(now(plan)), 1);
      assert.deepEqual(await parts(alice), [9n, 0n, 0n, 0n, 0n, 0n]);

      // She pays Carol 2 from the 8, then cancels: the 8 is back, and its
      // change may still come.
      const asked = await carol.request(own.mint, 2n);
      const cancelled = await alice.pay(asked, NO_SPACING);
      assert.deepEqual(await parts(alice), [1n, 2n, 6n, 0n, 0n, 0n]);
      assert.equal((await alice.balance(own.mint)).total, 7n);
      assert.equal(await alice.cancel(cancelled.transfers[0]!.request), true);
      assert.deepEqual(await parts(alice), [9n, 0n, 0n, 0n, 0n, 6n]);
      assert.equal((await alice.balance(own.mint)).total, 9n);
    } finally {
      await own.stop();
    }
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
    await assert.rejects(alice.pay(request, NO_SPACING), {
      code: 'denomination',
      details: { amount: 3n },
      message: "the request asks for a note of 3, which is not one of the mint's denominations",
    });
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

  test("an answer that is not the mint's is not kept", async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 8n);
    // Genuine notes of the mint, for a key nobody here holds.
    const stranger = (await (await purse()).request(server.mint, 1n)).notes[0]!.holder;
    const strangers = new Map<bigint, string>();
    for (const value of [1n, 2n, 4n]) {
      strangers.set(value, await server.mintTo(value, stranger));
    }
    // Bob's 4 and 1, and Alice's change of 2 and 1.
    const request = await bob.request(server.mint, 5n);
    const payment = await alice.pay(request, NO_SPACING);

    // A character of the signature changed.
    const forged = (note: string) =>
      note.slice(0, 190) + (note[190] === 'A' ? 'B' : 'A') + note.slice(191);
    // A note's amount: its bytes 45 to 53, big-endian.
    const amount = (note: string) => Buffer.from(note, 'base64url').readBigUInt64BE(45);
    const tamperings = [
      {
        tamper: ([first, ...rest]: string[]) => [forged(first!), ...rest],
        why: /note 0 of the answer is not signed by the mint: issuer signature does not verify/,
        code: 'unsigned' as const,
      },
      // A genuine note of the mint, of the amount asked for, to another key.
      {
        tamper: ([first, ...rest]: string[]) => [strangers.get(amount(first!))!, ...rest],
        why: /note 0 of the answer is not the transfer's output 0/,
        code: 'unasked' as const,
      },
    ];
    for (const { tamper, why, code } of tamperings) {
      // Neither the payer resending nor the payee submitting keeps any of it:
      // the payer's transfer stays outgoing, for the next settle.
      const open = (from: Purse) =>
        Purse.open(tampering(server, tamper), { store: new MemoryStore(from.toJson()), ...quick });
      const payer = await open(alice);
      keptNothing(payer, alice, await payer.settle(NO_SPACING), why, code);
      const payee = await open(bob);
      keptNothing(payee, bob, await payee.receive(request, payment), why, code);
    }

    // Nor without the mint's keys to check an answer with.
    const keyless: typeof globalThis.fetch = (input, init) =>
      String(input).endsWith('/keys')
        ? Promise.resolve(new Response('not here', { status: 404 }))
        : globalThis.fetch(input, init);
    const payer = await Purse.open(new Client(server.url, { fetch: keyless }), {
      store: new MemoryStore(alice.toJson()),
      ...quick,
    });
    keptNothing(payer, alice, await payer.settle(NO_SPACING), /not here \(http, HTTP 404\)/);

    // The mint's own server answers them, and both keep their notes.
    const received = await bob.receive(request, payment);
    allAnswered(received, 1);
    assert.equal(received.value, 5n);
    const settled = await alice.settle(NO_SPACING);
    allAnswered(settled, 1);
    assert.equal(settled.value, 3n);
  });

  test('an answer is checked with the keys read after it', async () => {
    // Its own server, since it rotates the mint.
    const own = await TestServer.start();
    try {
      const open = (client: Client, store = new MemoryStore()) =>
        Purse.open(client, { store, ...quick });
      const [alice, bob, carol] = [
        await open(own.client()),
        await open(own.client()),
        await open(own.client()),
      ];
      await own.fund(alice, 8n, 8n);
      // Bob's 4 lands in the last layer, frozen until the next rotation.
      const request = await bob.request(own.mint, 4n);
      allAnswered(await bob.receive(request, await alice.pay(request, NO_SPACING)), 1);
      assert.equal((await bob.balance(own.mint)).frozen, 4n);

      // He pays Carol with it now, to be submitted once that rotation frees
      // it. His purse pays only with notes it can spend, so it plans by the
      // mint's info as it will read then.
      const ahead: typeof globalThis.fetch = async (input, init) => {
        const answer = await globalThis.fetch(input, init);
        if (!String(input).endsWith('/info')) {
          return answer;
        }
        const info = (await answer.text()).replace(
          /"generation":(\d+)/g,
          (_, generation: string) => `"generation":${BigInt(generation) + 1n}`,
        );
        return new Response(info, { headers: { 'content-type': 'application/json' } });
      };
      const payer = await open(new Client(own.url, { fetch: ahead }), new MemoryStore(bob.toJson()));
      const toCarol = await carol.request(own.mint, 4n);
      const payment = await payer.pay(toCarol, NO_SPACING);

      // The rotation comes while the transfer is on its way, so the answer's
      // notes sit in the generation it opens, which keys read before the
      // transfer do not know. Carol's purse checks them with keys read after.
      const rotating: typeof globalThis.fetch = async (input, init) => {
        if (String(input).endsWith('/v1/transfer')) {
          await own.rotate();
        }
        return globalThis.fetch(input, init);
      };
      const payee = await open(
        new Client(own.url, { fetch: rotating }),
        new MemoryStore(carol.toJson()),
      );
      const received = await payee.receive(toCarol, payment);
      allAnswered(received, 1);
      assert.equal(received.value, 4n);
      const info = await own.client().info(own.mint);
      for (const note of received.notes) {
        const { serial } = await own.client().verify(note);
        assert.equal(serial.generation, info.oldest + BigInt(info.layers) - 1n);
      }
    } finally {
      await own.stop();
    }
  });

  test('a refusal the mint would not give drops or cancels nothing', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 8n);
    const request = await bob.request(server.mint, 5n);
    await alice.pay(request, NO_SPACING);

    // The mint answers `spent` and `expired` with 409 and `layer_full` with
    // 503, and never answers `unauthorized` or `duplicate_input` to a
    // transfer a holder signed.
    const answers = [
      [401, 'spent'],
      [401, 'expired'],
      [502, 'layer_full'],
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

  test('a transfer into a full layer is sent once and cancelled', async () => {
    // Layers of 64 notes. Carol's 64 notes of 1, paid from Dave's four of 16,
    // fill the last layer, where a hop from the mint layer lands.
    const own = await TestServer.start({}, undefined, 64);
    try {
      let tries = 0;
      const fetch: typeof globalThis.fetch = (input, init) => {
        if (String(input).endsWith('/v1/transfer')) {
          tries += 1;
        }
        return globalThis.fetch(input, init);
      };
      const open = () => Purse.open(own.client(), quick);
      const [bob, carol, dave] = [await open(), await open(), await open()];
      const alice = await Purse.open(new Client(own.url, { fetch }), quick);
      await own.fund(dave, 64n, 16n);
      const asked = await carol.requestAtMost(own.mint, 64n, 1n);
      allAnswered(await carol.receive(asked, await dave.pay(asked, NO_SPACING)), 4);

      await own.fund(alice, 8n, 8n);
      const held = alice.notes();
      const request = await bob.request(own.mint, 5n);
      const payment = await alice.pay(request, NO_SPACING);
      const received = await bob.receive(request, payment);
      assert.equal(received.refused.length, 1);
      const error = received.refused[0]!.error;
      assert.ok(error instanceof MintError);
      assert.equal(error.code, 'layer_full');
      assert.equal(isTransient(error), false);
      assert.equal(received.refused[0]!.outcome, 'kept');

      // After a rotation the hop lands in the same full generation, one
      // position down. Alice's purse sends it once and cancels it: the mint
      // never applied it, so the note is hers again.
      await own.rotate();
      tries = 0;
      const settled = await alice.settle(NO_SPACING);
      assert.equal(tries, 1);
      assert.equal(settled.refused.length, 1);
      const refused = settled.refused[0]!;
      assert.ok(refused.error instanceof MintError);
      assert.equal(refused.error.code, 'layer_full');
      assert.equal(refused.outcome, 'cancelled');
      assert.deepEqual(alice.notes(), held);
      assert.equal((await own.client().verify(held[0]!)).live?.position, 1);
      const balance = await alice.balance(own.mint);
      assert.deepEqual([balance.spendable, balance.outgoing, balance.unconfirmed], [8n, 0n, 3n]);

      // Its refresh, once the note dies at the next rotation, goes the same
      // way.
      await own.rotate();
      const refreshed = await alice.submit(now(await alice.planRefresh(own.mint)));
      assert.equal(tries, 2);
      assert.equal(refreshed.refused.length, 1);
      assert.equal(refreshed.refused[0]!.outcome, 'cancelled');
      assert.deepEqual(alice.notes(), held);
      assert.deepEqual(alice.outgoing(), []);
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
    const [unpaid] = asked.notes;
    await assert.rejects(bob.receive(asked, payment), {
      code: 'unpaid',
      details: { amount: unpaid!.amount, holder: unpaid!.holder },
    });
    assert.equal((await alice.balance(server.mint)).outgoing, 8n);
  });

  test('a payment the purse cannot make says why in its code and details', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 2n);

    const tooMuch = await bob.request(server.mint, 16n);
    await assert.rejects(alice.pay(tooMuch, NO_SPACING), (error: unknown) => {
      assert.ok(isWorglError(error) && error.code === 'unaffordable');
      assert.deepEqual(error.details, { spendable: 8n, needed: 16n });
      assert.equal(error.message, '8 is spendable now and 16 is needed');
      assert.equal(isTransient(error), false);
      return true;
    });
    await bob.withdraw(tooMuch);

    // Enough, but in notes of 2 for the one note of 8 asked: the payee asks
    // again in notes no larger than the largest that fit.
    const whole = await bob.request(server.mint, 8n);
    await assert.rejects(alice.pay(whole, NO_SPACING), {
      name: 'WorglError',
      code: 'fragmented',
      details: { largest: 2n, atMost: 2n },
      message: 'the requested notes do not fit the spendable notes; the largest spendable note is 2',
    });
    const balance = await alice.balance(server.mint);
    assert.deepEqual([balance.spendable, balance.outgoing], [8n, 0n]);

    await bob.withdraw(whole);
    const smaller = await bob.requestAtMost(server.mint, 8n, 2n);
    const received = await bob.receive(smaller, await alice.pay(smaller, NO_SPACING));
    allAnswered(received, 4);
    assert.equal(received.value, 8n);
    assert.equal((await bob.balance(server.mint)).expected, 0n);

    // A request paid already, and requests that cannot be made.
    const [paid] = smaller.notes;
    await assert.rejects(alice.pay(smaller, NO_SPACING), {
      code: 'paid',
      details: { holder: paid!.holder },
    });
    await assert.rejects(bob.request(server.mint, 0n), { code: 'zero_amount', details: {} });
    await assert.rejects(bob.requestAtMost(server.mint, 257n, 1n), {
      code: 'too_many_notes',
      details: { max: 256 },
      message: 'the request would need more than 256 notes',
    });
  });

  test('a fragmented payment names the largest notes that fit', async () => {
    const [alice, bob] = [await purse(), await purse()];
    // Notes of 4, 1, 1, 1 and 1. A request for 8 in notes of 4 would need
    // the second 4 from the notes of 1, and in notes of 2 two of them: only
    // notes of 1 fit.
    await server.fund(alice, 4n, 4n);
    await server.fund(alice, 4n, 1n);

    const request = await bob.request(server.mint, 8n);
    try {
      await alice.pay(request, NO_SPACING);
      assert.fail('the notes of 4 and 1 paid a note of 8');
    } catch (error) {
      // As the README has it.
      if (
        !(isWorglError(error) && error.code === 'fragmented') ||
        error.details.atMost === undefined
      ) {
        throw error;
      }
      assert.deepEqual(error.details, { largest: 4n, atMost: 1n });
      await bob.withdraw(request);
      const smaller = await bob.requestAtMost(server.mint, request.amount, error.details.atMost);
      const received = await bob.receive(smaller, await alice.pay(smaller, NO_SPACING));
      allAnswered(received, 5);
      assert.equal(received.value, 8n);
    }
    assert.equal((await bob.balance(server.mint)).expected, 0n);

    // From notes of 64, 1 and 1, no request for 66 fits: in any size it
    // asks for a note of 2 or more beside those the 64 pays, or, in notes
    // of 1, for more than one transfer from the 64 may make.
    const carol = await purse();
    await server.fund(carol, 64n, 64n);
    await server.fund(carol, 2n, 1n);
    await assert.rejects(carol.pay(await bob.request(server.mint, 66n), NO_SPACING), {
      code: 'fragmented',
      details: { largest: 64n, atMost: undefined },
    });
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

describe('note states', () => {
  test("come from the mint's info alone", async () => {
    // Its own server, since it rotates the mint, on a schedule so that
    // notes have times.
    const own = await TestServer.start({}, DAY);
    try {
      const asked: string[] = [];
      const fetch: typeof globalThis.fetch = (input, init) => {
        asked.push(`${init?.method ?? 'GET'} ${new URL(String(input)).pathname}`);
        return globalThis.fetch(input, init);
      };
      const alice = await Purse.open(new Client(own.url, { fetch }), quick);
      const bob = await Purse.open(own.client(), quick);
      await own.fund(alice, 4n, 4n);
      const stale = await own.client().infoText(own.mint);
      await own.rotate();
      await own.rotate();
      await own.fund(alice, 2n, 2n);
      await own.fund(bob, 1n, 1n);
      const request = await alice.request(own.mint, 1n);
      allAnswered(await alice.receive(request, await bob.pay(request, NO_SPACING)), 1);

      asked.length = 0;
      const notes = await alice.notesAt(own.mint);
      const first = await alice.firstExpiry(own.mint);
      // The mint was asked nothing about any one note.
      const info = `GET /v1/mints/${own.mint}/info`;
      assert.deepEqual(asked, [info, info]);

      const { oldest, rotation } = await own.client().info(own.mint);
      const next = rotation?.nextAt ?? 0n;
      const day = BigInt(DAY);
      const where = (note: HeldNote | undefined) => {
        assert.ok(note !== undefined);
        const { serial, state, position, spendableAt, expiresAt } = note;
        return { generation: serial.generation, state, position, spendableAt, expiresAt };
      };
      assert.deepEqual(
        notes.map((note) => note.amount),
        [4n, 2n, 1n],
      );
      // The 4, minted second to last two rotations ago, dies at the next.
      assert.deepEqual(where(notes[0]), {
        generation: oldest,
        state: 'expiring',
        position: 0,
        spendableAt: undefined,
        expiresAt: next,
      });
      assert.deepEqual(where(notes[1]), {
        generation: oldest + 2n,
        state: 'spendable',
        position: 2,
        spendableAt: undefined,
        expiresAt: next + 2n * day,
      });
      // The 1 just arrived in the last layer: it rests until the next rotation.
      assert.deepEqual(where(notes[2]), {
        generation: oldest + 3n,
        state: 'frozen',
        position: 3,
        spendableAt: next,
        expiresAt: next + 3n * day,
      });
      assert.equal(first, next);

      // An info older than a note cannot place it.
      const stub: typeof globalThis.fetch = (input, init) => {
        if (!String(input).endsWith('/info')) {
          return globalThis.fetch(input, init);
        }
        const headers = { 'content-type': 'application/json' };
        return Promise.resolve(new Response(stale, { headers }));
      };
      const behindClient = new Client(own.url, { fetch: stub });
      const behind = await Purse.open(behindClient, { store: new MemoryStore(alice.toJson()) });
      const staleNext = (await behindClient.info(own.mint)).rotation?.nextAt ?? 0n;
      // It still has a schedule, yet tells no time for the notes it cannot
      // place.
      assert.deepEqual(
        (await behind.notesAt(own.mint)).map((note) => [
          note.state,
          note.position,
          note.spendableAt,
          note.expiresAt,
        ]),
        [
          ['spendable', 2, undefined, staleNext + 2n * day],
          ['unknown', undefined, undefined, undefined],
          ['unknown', undefined, undefined, undefined],
        ],
      );
      // Another mint's info places none of them.
      const elsewhere = await Purse.open(server.client(), {
        store: new MemoryStore(alice.toJson()),
      });
      assert.deepEqual(await elsewhere.notesAt(server.mint), []);

      // A paused mint skips its rotations, so no time can be told.
      await own.client().pause(own.mint, own.token);
      assert.deepEqual(
        (await alice.notesAt(own.mint)).map((note) => [
          note.position,
          note.spendableAt,
          note.expiresAt,
        ]),
        [
          [0, undefined, undefined],
          [2, undefined, undefined],
          [3, undefined, undefined],
        ],
      );
      assert.equal(await alice.firstExpiry(own.mint), undefined);
      await own.client().resume(own.mint, own.token);

      // Left alone past the rotation, the 4 is gone and the 1 can move.
      await own.rotate();
      const later = (await own.client().info(own.mint)).rotation?.nextAt ?? 0n;
      assert.deepEqual(
        (await alice.notesAt(own.mint)).map((note) => [note.state, note.position, note.expiresAt]),
        [
          ['expired', undefined, undefined],
          ['spendable', 1, later + day],
          ['spendable', 2, later + 2n * day],
        ],
      );
      assert.equal(await alice.firstExpiry(own.mint), later + day);
      assert.equal(await alice.prune(own.mint), 4n);

      // The 2, spent by a transfer not yet answered, still counts.
      await alice.pay(await bob.request(own.mint, 2n), NO_SPACING);
      assert.deepEqual(
        (await alice.notesAt(own.mint)).map((note) => note.amount),
        [1n],
      );
      assert.equal(await alice.firstExpiry(own.mint), later + day);
    } finally {
      await own.stop();
    }
  });

  test('have no times at a mint without a schedule', async () => {
    const alice = await purse();
    await server.fund(alice, 8n, 8n);
    assert.deepEqual(
      (await alice.notesAt(server.mint)).map((note) => [
        note.amount,
        note.state,
        note.position,
        note.spendableAt,
        note.expiresAt,
      ]),
      [[8n, 'spendable', 2, undefined, undefined]],
    );
    assert.equal(await alice.firstExpiry(server.mint), undefined);
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

  test('a faucet whose notes its mint cannot make whole is refused', async () => {
    // What a faucet of notes of 3 would say, which the mint makes as 2 and 1.
    const about = {
      mint: playground.mint,
      amount: 3,
      per_address_per_hour: 3,
      per_day: 10_000,
      left_today: 10_000,
    };
    const fetch: typeof globalThis.fetch = async () =>
      new Response(JSON.stringify(about), { headers: { 'content-type': 'application/json' } });
    const purse = await Purse.open(playground.client(), quick);
    await assert.rejects(purse.takeFromFaucet(new Client(playground.url, { fetch })), {
      name: 'WorglError',
      code: 'faucet_amount',
      details: { amount: 3n },
      message: 'the faucet hands out notes of 3, which its mint cannot make as one note',
    });
    assert.equal((await purse.balance(playground.mint)).expected, 0n);
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

/**
 * A store in memory that several purses share, as tabs share a browser's,
 * with a lock that lets one at a time change it. `held` says whether one
 * holds it now and `locks` how often it was taken; `load` fails while
 * `failing` is set.
 */
class SharedStore extends MemoryStore {
  held = false;
  locks = 0;
  failing = false;
  #queue: Promise<unknown> = Promise.resolve();

  override async load(): Promise<string | undefined> {
    if (this.failing) {
      throw new Error('cannot read the store');
    }
    return super.load();
  }

  lock<T>(locked: () => Promise<T>): Promise<T> {
    const run = async (): Promise<T> => {
      this.held = true;
      this.locks += 1;
      try {
        return await locked();
      } finally {
        this.held = false;
      }
    };
    const result = this.#queue.then(run, run);
    this.#queue = result.catch(() => {});
    return result;
  }
}

describe('a shared store', () => {
  test('keeps the changes of purses racing on it', async () => {
    const alice = await purse();
    await server.fund(alice, 16n, 4n);
    const shared = new SharedStore();
    // Two tabs open Bob's purse, and each asks twice at once.
    const [bob, alsoBob] = [await purse(shared), await purse(shared)];
    const asking = [bob, bob, alsoBob, alsoBob];
    const requests = await Promise.all(asking.map((who) => who.request(server.mint, 4n)));
    const payments = await Promise.all(requests.map((request) => alice.pay(request, NO_SPACING)));
    const received = await Promise.all(
      asking.map((who, i) => who.receive(requests[i]!, payments[i]!)),
    );
    for (const submitted of received) {
      allAnswered(submitted, 1);
    }

    const reopened = await purse(new MemoryStore(shared.saved));
    const balance = await reopened.balance(server.mint);
    assert.deepEqual([balance.frozen, balance.requested], [16n, 0n]);
  });

  test('is locked only while a change is made, never across a request', async () => {
    const shared = new SharedStore();
    const lockedFor: string[] = [];
    const fetch: typeof globalThis.fetch = (input, init) => {
      if (shared.held) {
        lockedFor.push(String(input));
      }
      return globalThis.fetch(input, init);
    };
    const bob = await Purse.open(new Client(server.url, { fetch }), { store: shared, ...quick });
    await server.fund(bob, 8n, 8n);
    const request = await (await purse()).request(server.mint, 5n);
    const payment = await bob.pay(request, NO_SPACING);
    allAnswered(await bob.submit(payment.transfers), 1);
    allAnswered(await bob.settle(NO_SPACING), 0);
    await bob.planRefresh(server.mint);
    await bob.prune(server.mint);

    assert.deepEqual(lockedFor, []);
    assert.equal(shared.locks, 7);
  });

  test('a change it cannot read for rejects and keeps nothing', async () => {
    const shared = new SharedStore();
    const bob = await purse(shared);
    await server.fund(bob, 8n, 8n);
    const request = await (await purse()).request(server.mint, 5n);
    const payment = await bob.pay(request, NO_SPACING);

    shared.failing = true;
    await assert.rejects(bob.submit(payment.transfers), /cannot read the store/);
    await assert.rejects(bob.request(server.mint, 2n), /cannot read the store/);
    shared.failing = false;
    assert.equal(bob.outgoing().length, 1);

    // The mint answers the transfer again, and the purse keeps its change.
    const settled = await bob.settle(NO_SPACING);
    allAnswered(settled, 1);
    assert.equal(settled.value, 3n);
    const balance = await (await purse(new MemoryStore(shared.saved))).balance(server.mint);
    assert.deepEqual([balance.frozen, balance.outgoing, balance.requested], [3n, 0n, 0n]);
  });
});

describe('browser store', () => {
  test('needs IndexedDB and Web Locks', () => {
    // Node has neither, and a page outside a secure context no Web Locks.
    assert.throws(() => new IndexedDbStore('https://mint.example.com'), {
      name: 'NotSupportedError',
      message: /secure context/,
    });
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
    assert.throws(() => Holder.fromJson('{"format":9,"notes":[],"expected":[],"outgoing":[]}'), {
      name: 'WorglError',
      code: 'unknown_format',
      details: { format: 9 },
      message: 'saved in format 9, which this SDK does not read',
    });
    assert.throws(() => PaymentRequest.from('{"mint":'), { name: 'WorglError', code: 'malformed' });
  });
});
