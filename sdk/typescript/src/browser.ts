// Browser-only parts of the SDK, kept apart as node.ts is, so that the main
// entry point needs nothing that only a browser offers.

import { worglError } from './error.ts';
import type { Store } from './store.ts';

/** The version of the database's layout below. */
const VERSION = 1;
/** The object store that holds the purse. */
const TABLE = 'purse';
/** The key of the holder's JSON in it. */
const HOLDER = 'holder';
/** The key of its revision: the number of saves that made it. */
const REVISION = 'revision';

/** A store's hold on its lock, while a callback it runs under the lock lasts. */
interface Grant {
  /** False once the callback is done, or once another took the lock with `steal`. */
  held: boolean;
  /** Whether the store was read under it yet. */
  read: boolean;
}

/**
 * A store in the browser's IndexedDB, for a purse in a web page or a
 * worker: the database `worgl:<name>`, written by one purse at a time
 * under the Web Lock (`navigator.locks`) of the same name.
 *
 * Each save is one transaction with strict durability, so it resolves only
 * once the browser has flushed it to disk, not once it has only handed it
 * to the operating system, which a power cut can undo and which Chrome
 * does by default. A browser that predates the option ignores it.
 *
 * Tabs and workers of one origin can share the store, each with a purse
 * of its own, and purses can share one `IndexedDbStore`: the purse takes
 * the lock for every change, reads what is saved, makes the change and
 * saves before letting go. The store saves only while a callback it runs
 * under the lock ({@link lock}) holds it; a save outside one throws a
 * `DOMException` named `InvalidStateError`. A change that loses the lock
 * meanwhile, as when another tab takes it with `steal`, saves nothing:
 * its save refuses with the `WorglError` code `store_changed`. So does a
 * save that finds, in its transaction, something saved since the change
 * read the store. A purse then undoes the change and rejects, but makes
 * one that keeps what the mint answered, as a submit's, again on what is
 * saved now. An application that holds a lock of its own, such as one
 * tab's claim to keep notes alive, must name it otherwise, or every change
 * waits for it.
 *
 * Name each store after its purse's server, such as by the server's URL,
 * and the purses of two servers live side by side.
 *
 * A save the browser refuses rejects with its `DOMException`, and the
 * purse undoes the change: a `QuotaExceededError` when the origin is out
 * of space, or an `AbortError` or `UnknownError` when the transaction is
 * aborted. A browser that refuses the page storage at all rejects the
 * first load with its error. In a private window the store lasts until
 * the window closes. Unless the page holds persistent storage
 * (`navigator.storage.persist()`), the browser may also clear it to free
 * space.
 */
export class IndexedDbStore implements Store {
  /** The store's name. */
  readonly name: string;
  /** The name of its database and of its lock. */
  readonly #databaseName: string;
  readonly #indexedDB: IDBFactory;
  readonly #locks: LockManager;
  /** The open database, opened on first use and again after the browser closes it. */
  #connection: Promise<IDBDatabase> | undefined;
  /** The revision a save is to find: as a change read it under the lock, or as saved since. */
  #revision = 0;
  /** The hold of the callback running under the lock, if one is. */
  #grant: Grant | undefined;
  /** The last callback asked to run under the lock: the next waits for it. */
  #callbacks: Promise<unknown> = Promise.resolve();

  /**
   * A store named `name`. Nothing is opened yet. Throws a `DOMException`
   * named `NotSupportedError` where IndexedDB or Web Locks are missing, as
   * they are outside a secure context: a browser offers Web Locks only to a
   * page served over https or from localhost.
   */
  constructor(name: string) {
    const indexedDB = globalThis.indexedDB as IDBFactory | undefined;
    const locks = globalThis.navigator?.locks as LockManager | undefined;
    if (indexedDB === undefined || locks === undefined) {
      throw new DOMException(
        'IndexedDbStore needs IndexedDB and Web Locks, and a browser offers Web Locks ' +
          'only to a page in a secure context, served over https or from localhost',
        'NotSupportedError',
      );
    }
    this.name = name;
    this.#databaseName = `worgl:${name}`;
    this.#indexedDB = indexedDB;
    this.#locks = locks;
  }

  async load(): Promise<string | undefined> {
    const grant = this.#grant;
    const database = await this.#open();
    const [json, revision] = await new Promise<unknown[]>((resolve, reject) => {
      const transaction = database.transaction(TABLE, 'readonly');
      const table = transaction.objectStore(TABLE);
      const reads = [table.get(HOLDER), table.get(REVISION)];
      transaction.oncomplete = () => resolve(reads.map((read) => read.result));
      transaction.onabort = () => reject(aborted(transaction));
    });
    if (json === undefined && revision === undefined) {
      this.#expect(grant, 0);
      return undefined;
    }
    if (typeof json !== 'string' || typeof revision !== 'number') {
      throw worglError('malformed', {}, `the database ${this.#databaseName} holds no saved purse`);
    }
    this.#expect(grant, revision);
    return json;
  }

  async save(json: string): Promise<void> {
    const grant = this.#grant;
    if (grant === undefined) {
      throw new DOMException(
        'IndexedDbStore saves only in a callback it runs under its lock, as a purse saves it',
        'InvalidStateError',
      );
    }
    const database = await this.#open();
    await new Promise<void>((resolve, reject) => {
      const transaction = database.transaction(TABLE, 'readwrite', { durability: 'strict' });
      const table = transaction.objectStore(TABLE);
      // What this change read under the lock, or saved since, which is
      // still saved unless another writer saved meanwhile.
      const revision = this.#revision;
      let refused: string | undefined;
      const read = table.get(REVISION);
      read.onsuccess = () => {
        // Checked here, as late as it can be before the write.
        if (!grant.held) {
          refused =
            `this store lost the lock on ${this.#databaseName}, ` +
            'as to a tab that takes it with steal';
        } else if ((read.result ?? 0) !== revision) {
          refused = `another writer saved ${this.#databaseName} since this change read it`;
        }
        if (refused !== undefined) {
          transaction.abort();
          return;
        }
        table.put(json, HOLDER);
        table.put(revision + 1, REVISION);
      };
      transaction.oncomplete = () => {
        this.#revision = revision + 1;
        resolve();
      };
      transaction.onabort = () =>
        reject(
          refused !== undefined ? worglError('store_changed', {}, refused) : aborted(transaction),
        );
    });
  }

  lock<T>(locked: () => Promise<T>): Promise<T> {
    // One callback at a time, even after another tab takes the lock with
    // `steal` from one that runs on, so that the grant a load or a save
    // finds is always that of the callback it is part of.
    const run = (): Promise<T> => this.#locked(locked);
    const result = this.#callbacks.then(run, run);
    this.#callbacks = result.catch(() => {});
    return result;
  }

  /**
   * Runs `locked` under the lock, as {@link lock} does, once no other
   * callback of this store's runs.
   */
  #locked<T>(locked: () => Promise<T>): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const grant: Grant = { held: false, read: false };
      let granted = false;
      this.#locks
        .request(this.#databaseName, async () => {
          granted = true;
          grant.held = true;
          this.#grant = grant;
          try {
            resolve(await locked());
          } catch (error) {
            reject(error);
          } finally {
            grant.held = false;
            this.#grant = undefined;
          }
        })
        .catch((error: unknown) => {
          if (!granted) {
            reject(error);
            return;
          }
          // Another took the lock with `steal`. The request rejects at once,
          // but `locked` runs on, and its outcome is the call's: its save
          // refuses.
          grant.held = false;
        });
    });
  }

  /**
   * Takes `revision` as what the next save is to find, if it is the first
   * read under a grant that still holds the lock: a later one, such as a
   * read outside the callback meanwhile, does not move it.
   */
  #expect(grant: Grant | undefined, revision: number): void {
    if (grant?.held === true && !grant.read) {
      grant.read = true;
      this.#revision = revision;
    }
  }

  /** The database, opened now unless it is open already. */
  #open(): Promise<IDBDatabase> {
    if (this.#connection !== undefined) {
      return this.#connection;
    }
    const forget = (): void => {
      if (this.#connection === opening) {
        this.#connection = undefined;
      }
    };
    const opening = new Promise<IDBDatabase>((resolve, reject) => {
      const request = this.#indexedDB.open(this.#databaseName, VERSION);
      // Set once the open is refused, after which it may still succeed.
      let refused = false;
      request.onupgradeneeded = () => {
        request.result.createObjectStore(TABLE);
      };
      request.onblocked = () => {
        // Another connection holds the database in an older version and
        // did not close when asked to. Waiting could take forever.
        refused = true;
        reject(
          worglError(
            'store_blocked',
            {},
            `another tab holds ${this.#databaseName} open in an older version`,
          ),
        );
      };
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        const database = request.result;
        if (refused) {
          database.close();
          return;
        }
        // Let another tab upgrade the database to a newer version, or delete
        // it, rather than block it: the next call opens it again.
        database.onversionchange = () => {
          database.close();
          forget();
        };
        // Closed by the browser, as when the site's data is cleared.
        database.onclose = forget;
        resolve(database);
      };
    });
    // A failed open is tried again on the next call.
    opening.catch(forget);
    this.#connection = opening;
    return opening;
  }
}

/** Why the browser aborted `transaction`. */
function aborted(transaction: IDBTransaction): DOMException {
  return transaction.error ?? new DOMException('the browser aborted the transaction', 'AbortError');
}
