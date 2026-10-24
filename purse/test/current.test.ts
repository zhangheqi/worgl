import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { Current } from '../src/current.ts';

/** A stand-in for opening an SDK purse: each open is a new purse, numbered, read from the store as it is then. */
function opener(): { open: () => Promise<{ n: number; saw: number }>; opens: () => number; store: { notes: number } } {
  const store = { notes: 0 };
  let count = 0;
  return {
    store,
    opens: () => count,
    open: async () => ({ n: ++count, saw: store.notes }),
  };
}

/** A promise and the function that resolves it. */
function later(): { promise: Promise<void>; resolve: () => void } {
  let resolve = (): void => {};
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe('the current purse', () => {
  test('is opened once, and again only once marked stale', async () => {
    const { open, opens } = opener();
    const current = new Current(open);
    const a = await current.get();
    assert.equal(await current.get(), a);
    assert.equal(opens(), 1);
    current.stale();
    const b = await current.get();
    assert.notEqual(b, a);
    assert.equal(opens(), 2);
    assert.equal(await current.get(), b);
  });

  test('is not opened again while a change of it runs, but once it settles', async () => {
    // as when the tab comes back into view while a take from the faucet runs
    const { open, opens, store } = opener();
    const current = new Current(open);
    const gate = later();
    const taking = current.change(async (purse) => {
      await gate.promise;
      store.notes++;
      return purse;
    });
    const before = await current.get();
    current.stale();
    // a read meanwhile gets the purse that is taking, not one opened before the take saved
    assert.equal(await current.get(), before);
    assert.equal(opens(), 1);
    gate.resolve();
    assert.equal(await taking, before);
    const after = await current.get();
    assert.equal(opens(), 2);
    assert.equal(after.saw, 1, 'the new purse reads the store with the note in it');
  });

  test('opens a stale purse again before a change runs on it', async () => {
    const { open } = opener();
    const current = new Current(open);
    const first = await current.get();
    current.stale();
    const used = await current.change(async (purse) => purse);
    assert.notEqual(used, first);
    assert.equal(await current.get(), used);
  });

  test('stays stale when marked so during a change', async () => {
    const { open, opens } = opener();
    const current = new Current(open);
    await current.get();
    const gate = later();
    const changing = current.change(() => gate.promise);
    current.stale();
    gate.resolve();
    await changing;
    await current.get();
    assert.equal(opens(), 2);
  });

  test('does not keep a purse that failed to open', async () => {
    let fail = true;
    const current = new Current(async () => {
      if (fail) {
        throw new DOMException('no IndexedDB', 'NotSupportedError');
      }
      return 'purse';
    });
    await assert.rejects(current.get(), { name: 'NotSupportedError' });
    await assert.rejects(current.change(async () => 'never'), { name: 'NotSupportedError' });
    fail = false;
    assert.equal(await current.get(), 'purse');
  });
});
