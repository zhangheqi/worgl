/**
 * Where a `Purse` keeps its holder between runs.
 *
 * The JSON holds the secret key of every note, and notes are bearer:
 * whoever reads it can spend them. Keep it private and encrypted at rest.
 */
export interface Store {
  /** The JSON saved last, or `undefined` if nothing was saved yet. */
  load(): Promise<string | undefined>;
  /**
   * Replaces what was saved with `json`. Resolves only once the data would
   * survive a crash: the purse hands out keys and transfers only after it
   * does.
   */
  save(json: string): Promise<void>;
  /**
   * Runs `locked` while holding the store's lock, and resolves or rejects
   * as it does, for a store more than one purse writes, such as one that two
   * tabs of a browser share. The lock keeps out every other purse that takes
   * it, in this program or another, until `locked` is done. A purse takes
   * it for every change, and to read the transfers a settle resends and the
   * holder a backup seals: under the lock it reads what is saved
   * ({@link load}), makes the change to that and saves it, so it never saves
   * over a change another purse made since it last looked. It never holds
   * the lock across a request to the mint, and never asks for it while it
   * holds it. A store with a lock may refuse a save made outside it, as
   * `IndexedDbStore` does.
   *
   * Leave it out for a store only one purse writes: the purse's own holder
   * is then always current, and it reads the store only when it opens.
   */
  lock?<T>(locked: () => Promise<T>): Promise<T>;
}

/**
 * A store in memory, lost when the process ends. For tests, and for
 * applications that persist the JSON themselves.
 */
export class MemoryStore implements Store {
  #saved: string | undefined;

  constructor(saved?: string) {
    this.#saved = saved;
  }

  /** The JSON saved last, if any. */
  get saved(): string | undefined {
    return this.#saved;
  }

  async load(): Promise<string | undefined> {
    return this.#saved;
  }

  async save(json: string): Promise<void> {
    this.#saved = json;
  }
}
