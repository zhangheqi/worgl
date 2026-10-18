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
