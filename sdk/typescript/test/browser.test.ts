// The browser store against a real server, on an IndexedDB in memory and
// Web Locks as a browser grants them. What only a browser shows, such as
// durability on disk, quota, private windows and secure contexts, is not
// checked here.

import assert from 'node:assert/strict';
import { after, afterEach, before, beforeEach, describe, test } from 'node:test';

import {
  IDBDatabase as MemoryDatabase,
  IDBFactory as MemoryIndexedDb,
  forceCloseDatabase,
} from 'fake-indexeddb';

import { Client, NO_SPACING, Purse, isWorglError } from '../src/index.ts';
import { IndexedDbStore } from '../src/browser.ts';
import { TestServer } from './server.ts';

/** Fast retries, so a refused transfer does not hold a test up. */
const quick = { retry: { attempts: 2, firstDelayMs: 10 } };

/** For a test that a store which waits on another tab would hang, not fail. */
const waits = { timeout: 5000 };

/** A callback that runs under a lock. */
type Locked<T> = (lock: { name: string }) => Promise<T>;

/**
 * Web Locks (`navigator.locks`) as a browser grants them: one holder per
 * name, in the order asked, each callback in a task of its own. A request
 * with `steal` takes the lock at once: the request of the one that held it
 * rejects with an `AbortError`, while its callback runs on.
 */
class Locks {
  /** For each name held, how to take it from its holder. */
  readonly #holders = new Map<string, () => void>();
  /** For each name, the grants waiting for it, first first. */
  readonly #waiting = new Map<string, (() => void)[]>();

  request<T>(name: string, ...args: [Locked<T>] | [{ steal?: boolean }, Locked<T>]): Promise<T> {
    const [options, callback] = args.length === 1 ? [{}, args[0]] : args;
    return new Promise<T>((resolve, reject) => {
      const grant = (): void => {
        let held = true;
        this.#holders.set(name, () => {
          held = false;
          reject(new DOMException('the lock was taken with steal', 'AbortError'));
        });
        const done = (settle: () => void): void => {
          if (held) {
            held = false;
            settle();
            this.#holders.delete(name);
            this.#waiting.get(name)?.shift()?.();
          }
        };
        setTimeout(() => {
          Promise.resolve()
            .then(() => callback({ name }))
            .then(
              (value) => done(() => resolve(value)),
              (error: unknown) => done(() => reject(error)),
            );
        });
      };
      if (options.steal === true) {
        this.#holders.get(name)?.();
        grant();
      } else if (this.#holders.has(name)) {
        this.#waiting.set(name, [...(this.#waiting.get(name) ?? []), grant]);
      } else {
        grant();
      }
    });
  }
}

/** The durability of each readwrite transaction asked for. */
let durabilities: IDBTransactionDurability[] = [];
/** Each request to open a database, first first. */
let opened: IDBOpenDBRequest[] = [];

const transaction = MemoryDatabase.prototype.transaction;
MemoryDatabase.prototype.transaction = function (
  this: IDBDatabase,
  names: string | string[],
  mode?: IDBTransactionMode,
  options?: IDBTransactionOptions,
): IDBTransaction {
  const made = transaction.call(this, names, mode, options);
  if (mode === 'readwrite') {
    durabilities.push(made.durability);
  }
  return made;
};

/**
 * Gives this process an IndexedDB and Web Locks of their own, as a browser
 * gives the pages of one origin, until {@link leave}.
 */
function enter(): IDBFactory {
  const indexedDB = new MemoryIndexedDb();
  const open = indexedDB.open.bind(indexedDB);
  indexedDB.open = (name, version) => {
    const request = open(name, version);
    opened.push(request);
    return request;
  };
  Object.defineProperty(globalThis, 'indexedDB', { value: indexedDB, configurable: true });
  Object.defineProperty(globalThis.navigator, 'locks', { value: new Locks(), configurable: true });
  durabilities = [];
  opened = [];
  return indexedDB;
}

function leave(): void {
  Reflect.deleteProperty(globalThis, 'indexedDB');
  Reflect.deleteProperty(globalThis.navigator, 'locks');
}

let server: TestServer;
before(async () => {
  server = await TestServer.start();
});
after(async () => {
  await server.stop();
});

/** A purse of the test server's on `store`, by default a new store named `name`. */
function open(name: string, store = new IndexedDbStore(name)): Promise<Purse> {
  return Purse.open(server.client(), { store, ...quick });
}

/** What a purse opened now on the store named `name` has asked the mint for. */
async function requested(name: string): Promise<bigint> {
  return (await (await open(name)).balance(server.mint)).requested;
}

/**
 * Makes the next load of `store` wait, once it has read, until `resume` is
 * called; `loaded` resolves once it has read.
 */
function pauseAfterLoad(store: IndexedDbStore): { loaded: Promise<void>; resume: () => void } {
  const load = store.load.bind(store);
  let resume = (): void => {};
  const resumed = new Promise<void>((resolve) => (resume = resolve));
  let read = (): void => {};
  const loaded = new Promise<void>((resolve) => (read = resolve));
  store.load = async () => {
    store.load = load;
    const saved = await load();
    read();
    await resumed;
    return saved;
  };
  return { loaded, resume };
}

function storeChanged(error: unknown): boolean {
  return isWorglError(error) && error.code === 'store_changed';
}

describe('browser store', () => {
  test('needs IndexedDB and Web Locks', () => {
    // Node has neither, and a page outside a secure context no Web Locks.
    assert.throws(() => new IndexedDbStore('https://mint.example.com'), {
      name: 'NotSupportedError',
      message: /secure context/,
    });
  });

  describe('in a browser', () => {
    let indexedDB: IDBFactory;
    beforeEach(() => {
      indexedDB = enter();
    });
    afterEach(leave);

    test('keeps the changes of purses racing on it, each saved strictly', async () => {
      // Two tabs open the purse, and each asks three times at once.
      const name = server.url;
      const tabs = [await open(name), await open(name)];
      await Promise.all(
        tabs.flatMap((tab) => [1n, 2n, 3n].map((amount) => tab.request(server.mint, amount))),
      );
      assert.equal(await requested(name), 12n);
      assert.deepEqual(durabilities, Array(6).fill('strict'));
    });

    test('refuses a change that lost the lock, though nothing else was saved', async () => {
      const name = server.url;
      const stale = new IndexedDbStore(name);
      const tab = await open(name, stale);
      await tab.request(server.mint, 1n);

      // The tab's next change waits after its read under the lock, while
      // another tab takes the lock with steal and holds it.
      const paused = pauseAfterLoad(stale);
      const change = tab.request(server.mint, 4n);
      await paused.loaded;
      let release = (): void => {};
      const released = new Promise<void>((resolve) => (release = resolve));
      const stealing = navigator.locks.request(`worgl:${name}`, { steal: true }, () => released);
      paused.resume();

      await assert.rejects(change, storeChanged);
      release();
      await stealing;
      assert.equal(await requested(name), 1n);
    });

    test('refuses a change that lost the lock, even after a read', async () => {
      const name = server.url;
      const stale = new IndexedDbStore(name);
      const tab = await open(name, stale);
      await tab.request(server.mint, 1n);

      // The tab's next change waits after its read under the lock, while
      // another tab takes the lock with steal and asks for 3 through a
      // purse of its own.
      const paused = pauseAfterLoad(stale);
      const change = tab.request(server.mint, 4n);
      await paused.loaded;
      await navigator.locks.request(`worgl:${name}`, { steal: true }, async () => {});
      await (await open(name)).request(server.mint, 3n);
      // Meanwhile a purse opened on the same store reads it and asks for 2,
      // with time to slip in a save if the store let it.
      const view = await open(name, stale);
      const asked = view.request(server.mint, 2n);
      await new Promise((resolve) => setTimeout(resolve, 50));
      paused.resume();

      await assert.rejects(change, storeChanged);
      await asked;
      assert.equal(await requested(name), 1n + 3n + 2n);
      // The tab reads the store again for its next change.
      await tab.request(server.mint, 5n);
      assert.equal(await requested(name), 1n + 3n + 2n + 5n);
    });

    test('refuses a save that finds another writer saved since it read', async () => {
      const name = server.url;
      const stale = new IndexedDbStore(name);
      const tab = await open(name, stale);
      await tab.request(server.mint, 1n);
      const saved = await stale.load();

      // A writer that skips the lock saves while the tab's change waits
      // after its read, and the tab reads the store again outside the lock.
      const paused = pauseAfterLoad(stale);
      const change = tab.request(server.mint, 4n);
      await paused.loaded;
      await write(indexedDB, `worgl:${name}`, saved!, 2);
      await stale.load();
      paused.resume();

      await assert.rejects(change, storeChanged);
      assert.equal(await requested(name), 1n);
    });

    test('keeps an answer in hand though its save loses the lock', waits, async () => {
      // The tab pays twice, in a transfer each that goes at once, and submits
      // both. The save that keeps the first answer waits after its read under
      // the lock, while another tab takes the lock with steal and asks for 1
      // through a purse of its own.
      const name = server.url;
      const store = new IndexedDbStore(name);
      const tab = await open(name, store);
      const payee = await Purse.open(server.client(), quick);
      await server.fund(tab, 16n, 8n);
      const transfers = [
        ...(await tab.pay(await payee.request(server.mint, 5n))).transfers,
        ...(await tab.pay(await payee.request(server.mint, 5n))).transfers,
      ];
      const paused = pauseAfterLoad(store);
      const steps: string[] = [];
      const submitting = tab.submit(transfers, {
        onProgress: (progress) => steps.push(`${progress.step} ${progress.index}`),
      });
      await paused.loaded;
      await navigator.locks.request(`worgl:${name}`, { steal: true }, async () => {});
      await (await open(name)).request(server.mint, 1n);
      paused.resume();

      // The answer is kept all the same, in the purse as the other tab saved
      // it, and the second transfer goes on.
      const submitted = await submitting;
      assert.deepEqual([submitted.answered, submitted.value, submitted.refused], [2, 6n, []]);
      assert.deepEqual(submitted.unsent, []);
      assert.deepEqual(steps, ['sending 0', 'answered 0', 'sending 1', 'answered 1']);
      assert.deepEqual(tab.outgoing(), []);
      const saved = await (await open(name)).balance(server.mint);
      assert.deepEqual([saved.outgoing, saved.held, saved.requested], [0n, 6n, 1n]);
      assert.equal(await payee.accept(submitted.theirs), 10n);
    });

    test('keeps an answer in hand though its database is deleted meanwhile', waits, async () => {
      // The save that keeps the answer waits after its read under the lock,
      // while another tab deletes the database.
      const name = server.url;
      const store = new IndexedDbStore(name);
      const tab = await open(name, store);
      const payee = await Purse.open(server.client(), quick);
      await server.fund(tab, 8n, 8n);
      const { transfers } = await tab.pay(await payee.request(server.mint, 5n));
      const paused = pauseAfterLoad(store);
      const submitting = tab.submit(transfers);
      await paused.loaded;
      await new Promise<void>((resolve, reject) => {
        const request = indexedDB.deleteDatabase(`worgl:${name}`);
        request.onsuccess = () => resolve();
        request.onblocked = () => reject(new Error('the store blocked the delete'));
      });
      paused.resume();

      // The purse saves itself again, answer and all, and counts the answer
      // once.
      const submitted = await submitting;
      assert.deepEqual([submitted.answered, submitted.value, submitted.refused], [1, 3n, []]);
      const saved = await (await open(name)).balance(server.mint);
      assert.deepEqual([saved.outgoing, saved.held], [0n, 3n]);
    });

    test('drops a transfer the mint refused though its save loses the lock', waits, async () => {
      // A purse elsewhere, on a store of its own that a copy of the tab's
      // purse is written into, spends first the note the tab pays from.
      const name = server.url;
      const store = new IndexedDbStore(name);
      const tab = await open(name, store);
      const payee = await Purse.open(server.client(), quick);
      await server.fund(tab, 8n, 8n);
      const elsewhere = await open('copy');
      await write(indexedDB, 'worgl:copy', tab.toJson(), 1);
      const first = await payee.request(server.mint, 5n);
      await payee.receive(first, await elsewhere.pay(first));
      const { transfers } = await tab.pay(await payee.request(server.mint, 5n));

      // The save that drops the tab's transfer, which the mint refuses as
      // spent, waits after its read under the lock, while another tab takes
      // the lock with steal and asks for 1 through a purse of its own.
      const paused = pauseAfterLoad(store);
      const submitting = tab.submit(transfers);
      await paused.loaded;
      await navigator.locks.request(`worgl:${name}`, { steal: true }, async () => {});
      await (await open(name)).request(server.mint, 1n);
      paused.resume();

      const submitted = await submitting;
      assert.deepEqual(
        submitted.refused.map(({ outcome, atStake }) => [outcome, atStake]),
        [['dropped', 3n]],
      );
      const saved = await (await open(name)).balance(server.mint);
      assert.deepEqual([saved.outgoing, saved.requested], [0n, 1n]);
    });

    test('saves only under its lock', async () => {
      await assert.rejects(new IndexedDbStore(server.url).save('{}'), {
        name: 'InvalidStateError',
      });
      assert.deepEqual(await indexedDB.databases(), []);
    });

    test('keeps the purses of two servers apart', async () => {
      await (await open('https://one.example')).request(server.mint, 1n);
      await (await open('https://two.example')).request(server.mint, 2n);
      assert.deepEqual(
        [await requested('https://one.example'), await requested('https://two.example')],
        [1n, 2n],
      );
      const names = (await indexedDB.databases()).map((database) => database.name).sort();
      assert.deepEqual(names, ['worgl:https://one.example', 'worgl:https://two.example']);
    });

    test('rejects an open another tab blocks, rather than waiting', waits, async () => {
      // Another tab holds the database open and does not close it when a
      // newer layout asks it to, as from a newer SDK.
      const database = `worgl:${server.url}`;
      const holding = await connect(indexedDB, database);
      const openAt = indexedDB.open;
      indexedDB.open = (name, version) => openAt(name, name === database ? 2 : version);
      try {
        await assert.rejects(
          new IndexedDbStore(server.url).load(),
          (error) => isWorglError(error) && error.code === 'store_blocked',
        );
      } finally {
        indexedDB.open = openAt;
        holding.close();
      }
    });

    test('lets another tab delete its database, and opens it again', waits, async () => {
      const name = server.url;
      const store = new IndexedDbStore(name);
      await (await open(name, store)).request(server.mint, 1n);

      // The store closes its connection when asked to, so the delete goes on.
      await new Promise<void>((resolve, reject) => {
        const request = indexedDB.deleteDatabase(`worgl:${name}`);
        request.onsuccess = () => resolve();
        request.onblocked = () => reject(new Error('the store blocked the delete'));
      });
      assert.equal(await store.load(), undefined);

      // The browser closes it too, as when the site's data is cleared.
      await (await open(name, store)).request(server.mint, 2n);
      // Its types take the class for an instance.
      forceCloseDatabase(opened.at(-1)!.result as unknown as typeof IDBDatabase);
      assert.notEqual(await store.load(), undefined);
      assert.equal(await requested(name), 2n);
    });

    test('settles what another tab paid, as saved now, and says so', async () => {
      // A tab opens the purse before another funds it and pays from it.
      const name = server.url;
      const tab = await open(name);
      const other = await open(name);
      await server.fund(other, 8n, 4n);
      const payee = await Purse.open(server.client(), quick);
      const request = await payee.requestAtMost(server.mint, 8n, 4n);
      const payment = await other.pay(request, NO_SPACING);
      const paid = payment.transfers.map((transfer) => transfer.request).sort();
      assert.deepEqual(tab.outgoing(), []);

      // Stopped before it starts, a settle sends none of them, and lists
      // them all as unsent.
      const stopped = await tab.settle(NO_SPACING, { signal: AbortSignal.abort() });
      assert.deepEqual([stopped.answered, stopped.refused], [0, []]);
      assert.deepEqual(stopped.unsent.map((transfer) => transfer.request).sort(), paid);

      const steps: string[] = [];
      const settled = await tab.settle(NO_SPACING, {
        onProgress: (progress) => steps.push(`${progress.step} ${progress.index}`),
      });
      assert.deepEqual([settled.answered, settled.refused, settled.unsent], [2, [], []]);
      assert.deepEqual(steps, ['sending 0', 'answered 0', 'sending 1', 'answered 1']);
      assert.equal(await payee.accept(settled.theirs), 8n);
      assert.deepEqual((await open(name)).outgoing(), []);
    });

    test('keeps an answer in hand when stopped while another tab holds the lock', async () => {
      const name = server.url;
      const store = new IndexedDbStore(name);
      const stop = new AbortController();
      let armed = false;
      let holding: Promise<void> | undefined;
      let release = (): void => {};
      let released = false;
      const fetch: typeof globalThis.fetch = (input, init) => {
        if (armed && holding === undefined && String(input).endsWith('/keys')) {
          // Another tab takes the lock while the purse reads the keys to
          // check the first answer.
          holding = navigator.locks.request(
            `worgl:${name}`,
            () => new Promise<void>((resolve) => (release = resolve)),
          );
        }
        return globalThis.fetch(input, init);
      };
      const lock = store.lock.bind(store);
      store.lock = <T>(locked: () => Promise<T>): Promise<T> => {
        if (holding !== undefined && !stop.signal.aborted) {
          // The person leaves while the purse waits for the lock to keep the
          // answer, and the other tab lets go a little later.
          stop.abort();
          setTimeout(() => {
            released = true;
            release();
          }, 50);
        }
        return lock(locked);
      };
      const tab = await Purse.open(new Client(server.url, { fetch }), { store, ...quick });
      const payee = await Purse.open(server.client(), quick);
      await server.fund(tab, 16n, 8n);
      const transfers = [
        ...(await tab.pay(await payee.request(server.mint, 5n), NO_SPACING)).transfers,
        ...(await tab.pay(await payee.request(server.mint, 5n), NO_SPACING)).transfers,
      ];

      const steps: string[] = [];
      armed = true;
      const stopped = await tab.submit(transfers, {
        signal: stop.signal,
        onProgress: (progress) => steps.push(`${progress.step} ${progress.index}`),
      });
      await holding;
      assert.ok(released);
      // The first answer is kept once the other tab lets go; the stop takes
      // effect from the second transfer.
      assert.deepEqual(steps, ['sending 0', 'answered 0', 'unsent 1']);
      assert.deepEqual([stopped.answered, stopped.value, stopped.refused], [1, 3n, []]);
      assert.deepEqual(stopped.unsent, transfers.slice(1));

      // A purse opened on the store has the rest outgoing, and settles it.
      const reopened = await open(name);
      assert.deepEqual(reopened.outgoing(), [transfers[1]!.request]);
      const settled = await reopened.settle(NO_SPACING);
      assert.deepEqual([settled.answered, settled.refused, settled.unsent], [1, [], []]);
      assert.equal(await payee.accept([...stopped.theirs, ...settled.theirs]), 10n);
    });
  });
});

/** A connection to the database `name`, which ignores requests to close it. */
function connect(indexedDB: IDBFactory, name: string): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(name, 1);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

/** Saves `json` at `revision` in the database `name`, as a writer that skips the lock would. */
async function write(
  indexedDB: IDBFactory,
  name: string,
  json: string,
  revision: number,
): Promise<void> {
  const database = await connect(indexedDB, name);
  try {
    await new Promise<void>((resolve, reject) => {
      const transaction = database.transaction('purse', 'readwrite');
      transaction.objectStore('purse').put(json, 'holder');
      transaction.objectStore('purse').put(revision, 'revision');
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(transaction.error);
    });
  } finally {
    database.close();
  }
}
