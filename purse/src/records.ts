// The purse's own records, apart from the tokens: the mints the person
// added, the requests it made, and a small log of what happened, such as
// "Took 64 from the faucet" or "Sent 5". They live in an IndexedDB database of the purse's own, `purse`,
// which can never be taken for an SDK store (those are `worgl:<server>`).
// The tokens themselves live only in the SDK's IndexedDbStore, one per
// server. Every write is strict, so it resolves only once it is on disk.

import { endedToDrop } from './asks.ts';
import type { AskRecord, LogEntry, MintRecord } from './entries.ts';

export type { AskRecord, LogEntry, MintRecord, SavedInfo } from './entries.ts';

/** The database, and the version of its layout. */
const DATABASE = 'purse';
const VERSION = 2;

/** How many ended requests (taken in full, or cancelled) the purse keeps; open ones it always keeps. */
export const ASKS_ENDED_KEPT = 50;

/** How many log entries the purse keeps: Holdings shows the last few, History (later) more. */
const LOG_KEPT = 200;

/** The purse's records in this browser. */
export class Records {
  readonly #database: IDBDatabase;

  private constructor(database: IDBDatabase) {
    this.#database = database;
  }

  /** Opens the records, making the database on first use. */
  static open(): Promise<Records> {
    return new Promise((resolve, reject) => {
      const indexedDB = globalThis.indexedDB as IDBFactory | undefined;
      if (indexedDB === undefined) {
        reject(new DOMException('this browser offers no IndexedDB', 'NotSupportedError'));
        return;
      }
      const request = indexedDB.open(DATABASE, VERSION);
      request.onupgradeneeded = () => {
        const database = request.result;
        if (!database.objectStoreNames.contains('mints')) {
          database.createObjectStore('mints', { keyPath: 'mint' });
        }
        if (!database.objectStoreNames.contains('log')) {
          database.createObjectStore('log', { keyPath: 'id', autoIncrement: true });
        }
        // version 2: the requests made; each store is made only if missing, so the upgrade works from any earlier version
        if (!database.objectStoreNames.contains('asks')) {
          database.createObjectStore('asks', { keyPath: 'id' });
        }
      };
      request.onsuccess = () => {
        const database = request.result;
        // a newer purse in another tab wants a newer layout: let it have the database
        database.onversionchange = () => database.close();
        resolve(new Records(database));
      };
      request.onerror = () => reject(request.error ?? new DOMException('the records did not open', 'UnknownError'));
      request.onblocked = () => reject(new DOMException('an older purse in another tab holds the records', 'InvalidStateError'));
    });
  }

  /** The mints, in the order they were added. */
  async mints(): Promise<MintRecord[]> {
    let all: MintRecord[] = [];
    await this.#run('mints', 'readonly', (store) => {
      const request = store.getAll();
      request.onsuccess = () => {
        all = request.result as MintRecord[];
      };
    });
    return all.sort((a, b) => a.added - b.added);
  }

  /** Adds a mint, or saves what changed of one. */
  async putMint(record: MintRecord): Promise<void> {
    await this.#run('mints', 'readwrite', (store) => {
      store.put(record);
    });
  }

  /** Every request this purse made and keeps, newest first. */
  async asks(): Promise<AskRecord[]> {
    let all: AskRecord[] = [];
    await this.#run('asks', 'readonly', (store) => {
      const request = store.getAll();
      request.onsuccess = () => {
        all = request.result as AskRecord[];
      };
    });
    return all.sort((a, b) => b.t - a.t);
  }

  /**
   * Keeps a request, or saves what changed of one, and drops the ended
   * requests past the newest {@link ASKS_ENDED_KEPT}, in the same
   * transaction. Open requests are never dropped: their keys stay in the
   * SDK until they are cancelled.
   */
  async putAsk(record: AskRecord): Promise<void> {
    await this.#run('asks', 'readwrite', (store) => {
      store.put(record);
      const all = store.getAll();
      all.onsuccess = () => {
        for (const id of endedToDrop(all.result as AskRecord[], ASKS_ENDED_KEPT)) {
          store.delete(id);
        }
      };
    });
  }

  /** Saves what changed of a log entry that has an id already. */
  async updateLog(entry: LogEntry): Promise<void> {
    if (entry.id === undefined) {
      throw new RangeError('a log entry to update has an id');
    }
    await this.#run('log', 'readwrite', (store) => {
      store.put(entry);
    });
  }

  /** The last `limit` log entries, newest first. */
  async log(limit = LOG_KEPT): Promise<LogEntry[]> {
    const entries: LogEntry[] = [];
    await this.#run('log', 'readonly', (store) => {
      const request = store.openCursor(null, 'prev');
      request.onsuccess = () => {
        const cursor = request.result;
        if (cursor !== null && entries.length < limit) {
          entries.push(cursor.value as LogEntry);
          cursor.continue();
        }
      };
    });
    return entries;
  }

  /** Adds an entry to the log, and drops the oldest past {@link LOG_KEPT}. Resolves to the entry's id. */
  async addLog(entry: Omit<LogEntry, 'id'>): Promise<number> {
    let id = 0;
    await this.#run('log', 'readwrite', (store) => {
      const added = store.add(entry);
      added.onsuccess = () => {
        id = added.result as number;
      };
      const count = store.count();
      count.onsuccess = () => {
        let extra = count.result - LOG_KEPT;
        if (extra <= 0) {
          return;
        }
        const oldest = store.openCursor();
        oldest.onsuccess = () => {
          const cursor = oldest.result;
          if (cursor !== null && extra > 0) {
            cursor.delete();
            extra--;
            cursor.continue();
          }
        };
      };
    });
    return id;
  }

  /**
   * Runs `work` in one transaction on `table`, resolving once it completes:
   * for a write, once it is on disk, since every write is strict.
   */
  #run(table: string, mode: IDBTransactionMode, work: (store: IDBObjectStore) => void): Promise<void> {
    return new Promise((resolve, reject) => {
      const transaction =
        mode === 'readwrite'
          ? this.#database.transaction(table, mode, { durability: 'strict' })
          : this.#database.transaction(table, mode);
      work(transaction.objectStore(table));
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(transaction.error ?? new DOMException('the transaction was aborted', 'AbortError'));
    });
  }
}
