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
    assert.deepEqual(backup.mints, [{ mint: server.mint, server: server.url }]);
    const copy = await purse();
    const restored = await copy.restore(backup);
    assert.deepEqual(restored.notes.sort(), alice.notes().sort());
    assert.deepEqual(
      [restored.outgoing, restored.settled, restored.ended, restored.conflicts, restored.others],
      [[], [], [], [], []],
    );
    assert.deepEqual(await copy.balance(server.mint), await alice.balance(server.mint));

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
    assert.deepEqual(changed.mints, [{ mint: server.mint, server: server.url }]);
    const copy = await purse();
    await copy.restore(changed);
    const balance = await copy.balance(server.mint);
    assert.equal(balance.spendable, 8n);
    assert.equal(balance.requested, 3n);

    // A purse `worgl` made names no server.
    await worgl('bob.worgl', 'init');
    await worgl('bob.worgl', 'request', server.mint, '2', '-o', 'bob.json');
    const bob = await Backup.open(readFileSync(join(dir, 'bob.worgl'), 'utf8'), PASSPHRASE);
    assert.deepEqual(bob.mints, [{ mint: server.mint, server: undefined }]);
    const bobs = await purse();
    await bobs.restore(bob);
    assert.equal((await bobs.balance(server.mint)).requested, 2n);
  });

  test('restores a purse file worgl 0.1 wrote', async () => {
    const backup = await Backup.open(writtenBy01, PASSPHRASE);
    assert.deepEqual(backup.mints, [{ mint: itsMint, server: undefined }]);
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
    assert.deepEqual(restored.others, [{ mint: server.mint, server: server.url }]);
    assert.deepEqual((await elsewhere.restore(backup, [server.mint])).notes, alice.notes());
  });
});

describe('restoring a backup', () => {
  test('takes back a note spent since, until the mint says so', async () => {
    const [alice, bob] = [await purse(), await purse()];
    await server.fund(alice, 8n, 8n);
    const old = await backupOf(alice);
    const request = await bob.request(server.mint, 5n);
    await bob.receive(request, await alice.pay(request, NO_SPACING));
    await alice.settle(NO_SPACING);
    assert.equal(await held(alice), 3n);

    const restored = await alice.restore(old);
    assert.equal(restored.notes.length, 1);
    assert.equal(await held(alice), 11n);
    const checked = await alice.checkNotes(restored.notes, NO_SPACING);
    assert.deepEqual(checked, { value: 8n, discarded: restored.notes, unanswered: [] });
    assert.equal(await held(alice), 3n);
    // A live note is kept.
    const live = await alice.checkNotes(alice.notes(), NO_SPACING);
    assert.deepEqual(live, { value: 0n, discarded: [], unanswered: [] });
  });

  test('settles a transfer the backup shows answered', async () => {
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
    assert.deepEqual(restored.settled, outgoing);
    assert.deepEqual(await copy.balance(server.mint), await alice.balance(server.mint));
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
