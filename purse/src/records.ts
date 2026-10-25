// The purse's own records, apart from the tokens: the mints the person
// added and a small log of what happened, such as "Took 64 from the
// faucet" or "Kept 128 alive". They live in an IndexedDB database of the
// purse's own, `purse`, which can never be taken for an SDK store (those
// are `worgl:<server>`).
// The tokens themselves live only in the SDK's IndexedDbStore, one per
// server. Every write is strict, so it resolves only once it is on disk.

import type { LastFade } from './holdings.ts';
import { type Moved, mergeKept } from './keep.ts';

/**
 * The database, and the version of its layout. 2: the log holds keep-alive's
 * entries (`kept`, `lost`), which a purse of version 1 reads as "Faded";
 * opening at 2 closes the records of a version-1 tab still open, so it never
 * shows them.
 */
const DATABASE = 'purse';
const VERSION = 2;

/** How many log entries the purse keeps: Holdings shows the last few, History (later) more. */
const LOG_KEPT = 200;
/** How far back a run's entry looks for its rotation's: a rotation adds a few entries at most. */
const KEPT_SCAN = 50;

/** What the purse last read of a mint's info. */
export interface SavedInfo {
  layers: number;
  /** Seconds between rotations, if it rotates on its own. */
  periodSecs: number | undefined;
  paused: boolean;
  /** When the purse read it, by this device's clock. */
  readAt: number;
  /**
   * What fades first, and when, as the last read saw it: said when the mint
   * does not answer the next open. `null` when nothing does (no notes, or no
   * schedule); missing when no read of this version saved it.
   */
  fades?: LastFade | null;
}

/** A mint the person added. */
export interface MintRecord {
  /** Its issuer key, in hex: the record's key. */
  mint: string;
  /** The server it was added at: one SDK purse and store per server. */
  server: string;
  /** What the purse calls it: the build's playground, or the server's host until the person names it. */
  name: string;
  /** What its tokens are called. */
  unit: string;
  /** When it was added. */
  added: number;
  /** The info last read. */
  info: SavedInfo | undefined;
}

/** One entry in the purse's log. */
export interface LogEntry {
  /** Its place in the log, given when it is added. */
  id?: number;
  /** When it happened. */
  t: number;
  /**
   * `faucet`: a note taken from the faucet; `faded`: notes a rotation
   * dropped; `kept`: notes moved a layer on by keep-alive (the amount moved;
   * the log shows no change); `lost`: a hop the mint applied whose answer
   * it no longer keeps.
   */
  kind: 'faucet' | 'faded' | 'kept' | 'lost';
  /** The mint. */
  mint: string;
  /** Tokens it brought (faucet), took away (faded, lost) or moved on (kept). */
  amount: number;
  /** The serials of the notes it brought (faucet, kept), `generation:index`, so a note can say where it came from. */
  serials: string[];
  /** For `kept`: the mint's first layer (its generation) when they moved, so one rotation's runs make one entry. */
  round?: string;
}

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

  /** Adds an entry to the log, and drops the oldest past {@link LOG_KEPT}. */
  async addLog(entry: Omit<LogEntry, 'id'>): Promise<void> {
    await this.#run('log', 'readwrite', (store) => {
      store.add(entry);
      trim(store);
    });
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

/** Drops the oldest entries past {@link LOG_KEPT}, in the transaction that added one. */
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
        cursor.delete();
        extra--;
        cursor.continue();
      }
    };
  };
}
