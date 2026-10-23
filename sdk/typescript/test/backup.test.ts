// Backups of a purse: sealed, opened, read by the `worgl` command line and
// merged back in. Build the command line first with `cargo build -p
// worgl-cli`, or point WORGL_CLI_BINARY at it.

import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, describe, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import {
  Backup,
  Client,
  NO_SPACING,
  Purse,
  type WorglErrorCode,
  isWorglError,
} from '../src/index.ts';
import { TestServer } from './server.ts';

const cli =
  process.env.WORGL_CLI_BINARY ??
  fileURLToPath(new URL('../../../target/debug/worgl', import.meta.url));

const PASSPHRASE = 'correct horse battery staple';

/**
 * A purse file `worgl` 0.1 wrote under {@link PASSPHRASE}: notes of 8 and 4,
 * and a request for 5 expected, all of a mint no test serves.
 */
const writtenBy01 = readFileSync(
  new URL('../../../crates/worgl-sdk/tests/fixtures/purse-v1.worgl', import.meta.url),
  'utf8',
);
const itsMint = 'eefdebef624c7eca2dc7010746dfcb64a7e85459dfe31dd0068da35d208d3104';

/** Fast retries, so a refused transfer does not hold a test up. */
const quick = { retry: { attempts: 2, firstDelayMs: 10 } };

let server: TestServer;
let dir: string;
before(async () => {
  server = await TestServer.start();
  dir = mkdtempSync(join(tmpdir(), 'worgl-backup-'));
});
after(async () => {
  await server.stop();
  rmSync(dir, { recursive: true, force: true });
});

function purse(client: Client = server.client()): Promise<Purse> {
  return Purse.open(client, quick);
}

/** Opens what `purse` backs up now. */
async function backupOf(purse: Purse): Promise<Backup> {
  return Backup.open(await purse.backup(PASSPHRASE), PASSPHRASE);
}

/** Runs `worgl` on the purse file `name` in the test directory, and returns what it printed. */
async function worgl(name: string, ...args: string[]): Promise<string> {
  const env = {
    ...process.env,
    WORGL_SERVER: server.url,
    WORGL_PURSE: name,
    WORGL_PASSPHRASE: PASSPHRASE,
  };
  const { stdout } = await promisify(execFile)(cli, args, { cwd: dir, env });
  return stdout;
}

async function held(purse: Purse): Promise<bigint> {
  return (await purse.balance(server.mint)).held;
}

/** Checks that opening `sealed` is refused with `code` and `details`. */
async function refused(
  sealed: string,
  code: WorglErrorCode,
  details: Record<string, unknown> = {},
): Promise<void> {
  await assert.rejects(Backup.open(sealed, PASSPHRASE), (error: unknown) => {
    assert.ok(isWorglError(error), String(error));
    assert.equal(error.code, code, error.message);
    assert.deepEqual(error.details, details);
    return true;
  });
}

describe('backup', () => {
  test('restores into a new purse, and adds nothing twice', async () => {
    const alice = await purse();
    await server.fund(alice, 12n, 8n);
    await alice.request(server.mint, 3n);
    const sealed = await alice.backup(PASSPHRASE);
    const file = JSON.parse(sealed) as Record<string, unknown>;
    assert.equal(file.format, 'worgl-purse');
    assert.equal(file.version, 1);
    assert.ok(!sealed.includes('"notes"'));

    const backup = await Backup.open(sealed, PASSPHRASE);
    assert.deepEqual(backup.mints, [{ mint: server.mint, server: server.url, extra: {} }]);
    const copy = await purse();
    const restored = await copy.restore(backup);
    assert.deepEqual(restored.notes.sort(), alice.notes().sort());
    assert.deepEqual(
      [restored.spent, restored.outgoing, restored.ended, restored.conflicts, restored.others],
      [[], [], [], [], []],
    );
    // All of it is unchecked: another copy may have spent it since.
    assert.deepEqual(copy.unchecked().sort(), alice.notes().sort());
    const balance = await copy.balance(server.mint);
    assert.equal(balance.unchecked, 12n);
    assert.deepEqual({ ...balance, unchecked: 0n }, await alice.balance(server.mint));

    // Once in, the same backup adds nothing, nor does a purse's own.
    for (const [into, again] of [
      [copy, backup],
      [alice, backup],
      [alice, await backupOf(copy)],
    ] as const) {
      const before = await into.balance(server.mint);
      const nothing = await into.restore(again);
      assert.deepEqual(nothing.notes, []);
      assert.deepEqual(await into.balance(server.mint), before);
      assert.equal(before.spendable, 12n);
      assert.equal(before.requested, 3n);
    }
  });

  test('that does not open is refused with its code', async () => {
    const sealed = await (await purse()).backup(PASSPHRASE);
    await assert.rejects(Backup.open(sealed, 'correct horse battery stapler'), {
      name: 'WorglError',
      code: 'wrong_passphrase',
    });
    // Weakening the key derivation is caught, as is any change to the
    // header or the ciphertext.
    const weakened = sealed.replace('"iterations": 3', '"iterations": 1');
    assert.notEqual(weakened, sealed);
    await refused(weakened, 'wrong_passphrase');
    const ciphertext = /"ciphertext": "([^"]+)"/.exec(sealed)![1]!;
    const middle = Math.floor(ciphertext.length / 2);
    const flipped = ciphertext[middle] === 'A' ? 'B' : 'A';
    const altered = ciphertext.slice(0, middle) + flipped + ciphertext.slice(middle + 1);
    await refused(sealed.replace(ciphertext, altered), 'wrong_passphrase');

    await refused('{}', 'not_sealed');
    await refused('a purse', 'not_sealed');
    await refused(sealed.replace('"version": 1', '"version": 2'), 'unknown_version', {
      version: 2,
    });
    await refused(sealed.replace(ciphertext, '!'), 'damaged');
    await refused(sealed.replace('"memory_kib": 65536', '"memory_kib": 1073741824'), 'damaged');
  });

  test('is a purse file worgl opens, and its purse files restore', async () => {
    const alice = await purse();
    await server.fund(alice, 8n, 8n);
    writeFileSync(join(dir, 'alice.worgl'), await alice.backup(PASSPHRASE));
    assert.match(await worgl('alice.worgl', 'balance', server.mint), /^spendable {2}8$/m);
    // A change by `worgl` keeps the server the backup names.
    await worgl('alice.worgl', 'request', server.mint, '3', '-o', 'request.json');
    const changed = await Backup.open(readFileSync(join(dir, 'alice.worgl'), 'utf8'), PASSPHRASE);
    assert.deepEqual(changed.mints, [{ mint: server.mint, server: server.url, extra: {} }]);
    const copy = await purse();
    await copy.restore(changed);
    const balance = await copy.balance(server.mint);
    assert.equal(balance.spendable, 8n);
    assert.equal(balance.requested, 3n);

    // A purse `worgl` made names no server.
    await worgl('bob.worgl', 'init');
    await worgl('bob.worgl', 'request', server.mint, '2', '-o', 'bob.json');
    const bob = await Backup.open(readFileSync(join(dir, 'bob.worgl'), 'utf8'), PASSPHRASE);
    assert.deepEqual(bob.mints, [{ mint: server.mint, server: undefined, extra: {} }]);
    const bobs = await purse();
    await bobs.restore(bob);
    assert.equal((await bobs.balance(server.mint)).requested, 2n);
  });

  test('restores a purse file worgl 0.1 wrote', async () => {
    const backup = await Backup.open(writtenBy01, PASSPHRASE);
    assert.deepEqual(backup.mints, [{ mint: itsMint, server: undefined, extra: {} }]);
    const restored = await (await purse()).restore(backup);
    assert.equal(restored.notes.length, 2);
    assert.deepEqual(restored.others, []);
    // Only the mints asked for.
    const none = await (await purse()).restore(backup, []);
    assert.deepEqual(none.notes, []);
    assert.deepEqual(none.others, backup.mints);
  });

  test('leaves the mints of another server for its purse', async () => {
    const alice = await purse();
    await server.fund(alice, 8n, 8n);
    const backup = await backupOf(alice);
    // The mint is not asked anything, so no server needs to answer.
    const elsewhere = await purse(new Client('http://127.0.0.1:9'));
    const restored = await elsewhere.restore(backup);
    assert.deepEqual(restored.notes, []);
    assert.deepEqual(restored.others, [{ mint: server.mint, server: server.url, extra: {} }]);
    assert.deepEqual((await elsewhere.restore(backup, [server.mint])).notes, alice.notes());
  });

  test('holds the purses of several servers, and the names of their mints', async () => {
    const other = await TestServer.start();
    try {
      const [here, there] = [await purse(), await purse(other.client())];
      await server.fund(here, 8n, 8n);
      await other.fund(there, 4n, 4n);
      const named = { name: 'Playground', unit: 'tokens' };
      const elsewhere = 'https://mint.example.com';
      const sealed = await here.backup(PASSPHRASE, {
        purses: [there],
        mints: [
          { mint: server.mint, server: 'http://ignored.example', extra: named },
          { mint: itsMint, server: elsewhere, extra: { name: 'Old' } },
        ],
      });
      const listed = [
        { mint: server.mint, server: server.url, extra: named },
        { mint: other.mint, server: other.url, extra: {} },
        { mint: itsMint, server: elsewhere, extra: { name: 'Old' } },
      ].sort((a, b) => (a.mint < b.mint ? -1 : 1));
      const backup = await Backup.open(sealed, PASSPHRASE);
      assert.deepEqual(backup.mints, listed);

      // Each server's purse takes its part back.
      for (const [from, into] of [
        [here, await purse()],
        [there, await purse(other.client())],
      ] as const) {
        const restored = await into.restore(backup);
        assert.deepEqual(restored.notes, from.notes());
        const kept = from.client.baseUrl;
        assert.deepEqual(
          restored.others,
          listed.filter((m) => m.server !== kept),
        );
      }

      // `worgl` keeps the list as it is when it changes the file.
      writeFileSync(join(dir, 'both.worgl'), sealed);
      await worgl('both.worgl', 'request', server.mint, '3', '-o', 'both.json');
      const changed = readFileSync(join(dir, 'both.worgl'), 'utf8');
      assert.deepEqual((await Backup.open(changed, PASSPHRASE)).mints, listed);

      // A mint in two of the purses is refused.
      const twice = await purse();
      await server.fund(twice, 1n, 1n);
      await assert.rejects(here.backup(PASSPHRASE, { purses: [twice] }), TypeError);
    } finally {
      await other.stop();
    }
  });

  test('is refused under an empty passphrase', async () => {
    await assert.rejects((await purse()).backup(''), {
      name: 'WorglError',
      code: 'empty_passphrase',
    });
  });
});

describe('restoring a backup', () => {
  test('takes in notes spent since unchecked, until the mint says', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 8n);
    const old = await backupOf(alice);
    const request = await bob.request(server.mint, 5n);
    await bob.receive(request, await alice.pay(request, NO_SPACING));
    await alice.settle(NO_SPACING);
    assert.equal(await held(alice), 3n);
    // Alice remembers spending the note, so her old backup gives nothing back.
    assert.deepEqual((await alice.restore(old)).notes, []);
    assert.equal(await held(alice), 3n);

    // A new purse knows nothing, and takes it in unchecked.
    const copy = await purse();
    const restored = await copy.restore(old);
    assert.equal(restored.notes.length, 1);
    assert.deepEqual(copy.unchecked(), restored.notes);
    assert.equal((await copy.balance(server.mint)).unchecked, 8n);
    const checked = await copy.checkNotes(undefined, NO_SPACING);
    assert.deepEqual(checked, {
      value: 8n,
      discarded: restored.notes,
      kept: [],
      unanswered: [],
    });
    assert.equal(await held(copy), 0n);
    // A live note is kept, and checked.
    const fresh = await purse();
    await fresh.restore(await backupOf(alice));
    const live = await fresh.checkNotes(undefined, NO_SPACING);
    assert.deepEqual(live, { value: 0n, discarded: [], kept: alice.notes(), unanswered: [] });
    assert.deepEqual(fresh.unchecked(), []);
    assert.deepEqual(await fresh.balance(server.mint), await alice.balance(server.mint));
  });

  test('ends a transfer the backup shows answered', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 8n);
    const request = await bob.request(server.mint, 5n);
    const payment = await alice.pay(request, NO_SPACING);
    const paying = await backupOf(alice);
    await bob.receive(request, payment);
    await alice.settle(NO_SPACING);

    const copy = await purse();
    const { outgoing } = await copy.restore(paying);
    assert.equal(outgoing.length, 1);
    assert.equal((await copy.balance(server.mint)).outgoing, 8n);
    const restored = await copy.restore(await backupOf(alice));
    assert.deepEqual(restored.ended, outgoing);
    // The change is Alice's too, who may spend it.
    const balance = await copy.balance(server.mint);
    assert.equal(balance.unchecked, 3n);
    assert.deepEqual({ ...balance, unchecked: 0n }, await alice.balance(server.mint));
  });

  test('does not bring back a transfer settled since, nor its change', async () => {
    // Its own server, since the change moves only after a rotation.
    const own = await TestServer.start();
    try {
      const open = () => purse(own.client());
      const total = async (purse: Purse) => (await purse.balance(own.mint)).total;
      const [alice, bob, carol] = [await open(), await open(), await open()];
      await own.fund(alice, 8n, 8n);
      const toBob = await bob.request(own.mint, 5n);
      const payment = await alice.pay(toBob, NO_SPACING);
      const paying = await backupOf(alice);
      await bob.receive(toBob, payment);
      await alice.settle(NO_SPACING);
      await own.rotate();
      const toCarol = await carol.request(own.mint, 3n);
      await carol.receive(toCarol, await alice.pay(toCarol, NO_SPACING));
      await alice.settle(NO_SPACING);
      assert.equal(await total(alice), 0n);

      const restored = await alice.restore(paying);
      assert.deepEqual([restored.notes, restored.outgoing], [[], []]);
      await alice.settle(NO_SPACING);
      assert.equal(await total(alice), 0n);

      // A new purse brings the transfer back, and its change counts
      // unchecked until the mint says Alice spent it.
      const copy = await open();
      assert.equal((await copy.restore(paying)).outgoing.length, 1);
      assert.equal((await copy.settle(NO_SPACING)).answered, 1);
      const balance = await copy.balance(own.mint);
      assert.deepEqual([balance.held, balance.unchecked], [3n, 3n]);
      assert.equal((await copy.checkNotes(undefined, NO_SPACING)).value, 3n);
      assert.equal(await total(copy), 0n);
    } finally {
      await own.stop();
    }
  });

  test('keeps a transfer cancelled since cancelled', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 4n, 4n);
    const request = await bob.request(server.mint, 4n);
    const payment = await alice.pay(request, NO_SPACING);
    const paying = await backupOf(alice);
    const [transfer] = payment.transfers;
    assert.ok(transfer !== undefined && (await alice.cancel(transfer.request)));

    const restored = await alice.restore(paying);
    assert.deepEqual(restored.outgoing, []);
    const balance = await alice.balance(server.mint);
    assert.deepEqual([balance.held, balance.sending, balance.outgoing], [4n, 0n, 0n]);
  });

  test('expects the keys of requests from either copy', async () => {
    const alice = await purse();
    await alice.request(server.mint, 2n);
    const copy = await purse();
    await copy.restore(await backupOf(alice));
    await copy.request(server.mint, 4n);
    await alice.request(server.mint, 1n);
    await alice.restore(await backupOf(copy));
    assert.equal((await alice.balance(server.mint)).requested, 7n);
  });

  test('keeps both transfers of a note both copies spent, for the mint to settle', async () => {
    const [alice, bob, carol] = [await purse(), await purse(), await purse()];
    await server.fund(alice, 8n, 8n);
    const copy = await purse();
    await copy.restore(await backupOf(alice));
    const toBob = await bob.request(server.mint, 5n);
    const paid = await alice.pay(toBob, NO_SPACING);
    await copy.pay(await carol.request(server.mint, 6n), NO_SPACING);

    const restored = await alice.restore(await backupOf(copy));
    assert.equal(restored.outgoing.length, 1);
    assert.equal(restored.conflicts.length, 2);
    // Bob's payment reaches the mint first; the other is refused, and dropped.
    await bob.receive(toBob, paid);
    const settled = await alice.settle(NO_SPACING);
    assert.equal(settled.answered, 1);
    assert.deepEqual(
      settled.refused.map((r) => r.outcome),
      ['dropped'],
    );
    const balance = await alice.balance(server.mint);
    assert.equal(balance.held, 3n);
    assert.equal(balance.outgoing, 0n);
  });
});
