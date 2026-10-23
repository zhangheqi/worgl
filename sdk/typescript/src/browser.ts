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
 * of its own: the purse takes the lock for every change, reads what is
 * saved, makes the change and saves before letting go. Each save also
 * checks, in its transaction, that nothing was saved since this store last
 * read or saved, and otherwise refuses with the `WorglError` code
 * `store_changed`, as when another tab took the lock with `steal`.
 * An application that holds a lock of its own, such as one tab's claim to
 * keep notes alive, must name it otherwise, or every change waits for it.
 *
 * Give each purse a name of its own, such as its server's URL, and two
 * purses, one per server, live side by side.
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
  /** The revision this store last read or saved, 0 for none. */
  #revision = 0;

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
    const database = await this.#open();
    const [json, revision] = await new Promise<unknown[]>((resolve, reject) => {
      const transaction = database.transaction(TABLE, 'readonly');
      const table = transaction.objectStore(TABLE);
      const reads = [table.get(HOLDER), table.get(REVISION)];
      transaction.oncomplete = () => resolve(reads.map((read) => read.result));
      transaction.onabort = () => reject(aborted(transaction));
    });
    if (json === undefined && revision === undefined) {
      this.#revision = 0;
      return undefined;
    }
    if (typeof json !== 'string' || typeof revision !== 'number') {
      throw worglError('malformed', {}, `the database ${this.#databaseName} holds no saved purse`);
    }
    this.#revision = revision;
    return json;
  }

  async save(json: string): Promise<void> {
    const database = await this.#open();
    await new Promise<void>((resolve, reject) => {
      const transaction = database.transaction(TABLE, 'readwrite', { durability: 'strict' });
      const table = transaction.objectStore(TABLE);
      // What this store last read or saved, which is still saved unless
      // another writer saved since.
      const revision = this.#revision;
      let changed = false;
      const read = table.get(REVISION);
      read.onsuccess = () => {
        if ((read.result ?? 0) !== revision) {
          changed = true;
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
          changed
            ? worglError(
                'store_changed',
                {},
                `another writer saved ${this.#databaseName} since this store last read it`,
              )
            : aborted(transaction),
        );
    });
  }

  lock<T>(locked: () => Promise<T>): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      let granted = false;
      this.#locks
        .request(this.#databaseName, () => {
          granted = true;
          // Settles even if `locked` throws rather than rejects.
          const outcome = new Promise<T>((settle) => settle(locked()));
          outcome.then(resolve, reject);
          return outcome;
        })
        .catch((error: unknown) => {
          // Once granted, the request also rejects as soon as another tab
          // takes the lock with `steal`, but `locked` runs on, and its save
          // refuses if the other saved first: its own outcome is the call's.
          if (!granted) {
            reject(error);
          }
        });
    });
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
