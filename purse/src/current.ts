// Which SDK purse of a server is the current one. An SDK purse shows its
// holder as it last read or saved it, so after another tab changed the
// store the purse opens a new one on the same store. It never does while a
// change of the current one is on its way: that change reads the store
// under its lock and saves, so once it settles that purse is current
// again, and a purse opened before then would miss what it kept.
//
// No SDK here, so it can be tested with stand-ins.

/** The current purse of one server: opened on first use, and again once marked stale and no change runs. */
export class Current<T> {
  readonly #open: () => Promise<T>;
  #purse: Promise<T> | undefined;
  #stale = false;
  #changing = 0;

  /** A current purse that `open` opens, from the server's one store. */
  constructor(open: () => Promise<T>) {
    this.#open = open;
  }

  /**
   * The purse to read from or change: the one open, unless it is stale and
   * no change is on its way, when a new one is opened. One that failed to
   * open is not kept, so the next call tries again.
   */
  get(): Promise<T> {
    if (this.#purse === undefined || (this.#stale && this.#changing === 0)) {
      this.#stale = false;
      const opening = this.#open();
      this.#purse = opening;
      opening.catch(() => {
        if (this.#purse === opening) {
          this.#purse = undefined;
        }
      });
    }
    return this.#purse;
  }

  /** Marks the purse to be opened again from its store, as soon as no change runs. */
  stale(): void {
    this.#stale = true;
  }

  /**
   * Runs `work`, a change, on the current purse (opened again first if it
   * is stale). Until the change settles, every call gets that same purse.
   */
  async change<R>(work: (purse: T) => Promise<R>): Promise<R> {
    const purse = this.get();
    this.#changing++;
    try {
      return await work(await purse);
    } finally {
      this.#changing--;
    }
  }
}
