// The purse's own records, apart from the tokens: the mints the person
// added, the requests it made, and a small log of what happened, such as
// "Took 64 from the faucet", "Sent 5" or "Kept 128 alive". They live in an
// IndexedDB database of the purse's own, `purse`, which can never be taken
// for an SDK store (those are `worgl:<server>`).
// The tokens themselves live only in the SDK's IndexedDbStore, one per
// server. Every write is strict, so it resolves only once it is on disk.

import { endedToDrop } from './asks.ts';
import type { AskRecord, LogEntry, MintRecord } from './entries.ts';
import { type Moved, mergeKept } from './keep.ts';

export type { AskRecord, LogEntry, MintRecord, SavedInfo } from './entries.ts';

/**
 * The database, and the version of its layout. 1: the mints and the log.
 * 2: either of two builds made side by side, keep-alive's (the log's `kept`
 * and `lost` entries) or Receive and Send's (the store `asks`, and the log's
 * `sent` and `received` entries). 3: both. The upgrade makes each store
 * that is missing, so it works from any of these. Opening at a newer version
 * closes the records of an older tab still open, so that tab never reads an
 * entry it does not know.
 */
const DATABASE = 'purse';
const VERSION = 3;

/** How many ended requests (taken in full, or cancelled) the purse keeps; open ones it always keeps. */
export const ASKS_ENDED_KEPT = 50;

/** How many log entries the purse keeps: Holdings shows the last few, History (later) more. */
const LOG_KEPT = 200;
/** How far back a run's entry looks for its rotation's: a rotation adds a few entries at most. */
const KEPT_SCAN = 50;

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
        // the requests made, since Receive and Send's version 2; each store is made only if missing, so the upgrade works from any earlier version
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

  /**
   * Changes the fields `patch` names of a mint kept already, reading it in
   * the same transaction, so that two tabs that each change one field never
   * put back the other's from a copy read before. A mint not kept is left so.
   */
  async updateMint(mint: string, patch: Partial<Omit<MintRecord, 'mint'>>): Promise<void> {
    await this.#run('mints', 'readwrite', (store) => {
      const request = store.get(mint);
      request.onsuccess = () => {
        const record = request.result as MintRecord | undefined;
        if (record !== undefined) {
          store.put({ ...record, ...patch });
        }
      };
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

  /** The last `limit` log entries, newest first, and any older send still on its way (see `trim`). */
  async log(limit = LOG_KEPT): Promise<LogEntry[]> {
    const entries: LogEntry[] = [];
    let seen = 0;
    await this.#run('log', 'readonly', (store) => {
      const request = store.openCursor(null, 'prev');
      request.onsuccess = () => {
        const cursor = request.result;
        if (cursor === null) {
          return;
        }
        const entry = cursor.value as LogEntry;
        seen += 1;
        if (seen <= limit || stays(entry)) {
          entries.push(entry);
        }
        cursor.continue();
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
      trim(store);
    });
    return id;
  }

  /**
   * Adds keep-alive's entry for a rotation: when the log has one for the same
   * mint and rotation already (an earlier run of the visit, stopped when the
   * page was hidden, or another tab's), it takes that one's place with both
   * runs in it, as the newest entry. `spent` names the notes this run moved
   * that the purse held before it, so a note the earlier run brought and
   * this one moved on counts once.
   */
  async addKept(entry: Omit<LogEntry, 'id'>, spent: readonly Moved[]): Promise<void> {
    await this.#run('log', 'readwrite', (store) => {
      let seen = 0;
      const cursor = store.openCursor(null, 'prev');
      cursor.onsuccess = () => {
        const at = cursor.result;
        if (at === null || seen >= KEPT_SCAN) {
          store.add(entry);
          trim(store);
          return;
        }
        seen += 1;
        const old = at.value as LogEntry;
        if (old.kind !== 'kept' || old.mint !== entry.mint || old.round === undefined || old.round !== entry.round) {
          at.continue();
          return;
        }
        at.delete();
        store.add(mergeKept(old, entry, spent));
      };
    });
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

/** Whether an entry stays however old: a send still on its way, whose settle writes what it came to there. */
const stays = (entry: LogEntry): boolean => entry.kind === 'sent' && (entry.state === 'sending' || entry.state === 'waiting');

/** Drops the oldest entries past {@link LOG_KEPT}, but a send still on its way, in the transaction that added one. */
function trim(store: IDBObjectStore): void {
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
        if (!stays(cursor.value as LogEntry)) {
          cursor.delete();
          extra--;
        }
        cursor.continue();
      }
    };
  };
}
