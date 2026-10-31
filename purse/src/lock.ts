// The purse's one lock for sending transfers to a mint: a send, a settle
// of what a send kept for later, a keep-alive pass and a restore each hold
// it, so two of them never submit the same transfers at once, in one tab
// or across the purse's tabs. It is never the SDK store's own lock
// (`worgl:<server URL>`), which each save takes, so no change of a store
// waits for it. And a second, for asking the mint about restored notes:
// one tab asks, so the mint is never asked about a note twice at once.

/** The lock's name, the same in every tab of the purse. */
export const TRANSFERS_LOCK = 'purse:transfers';

/** Web Locks, as far as the purse uses them: typed here, so the tests' Node build, which has no DOM, reads this file too. */
interface Locks {
  request<R>(name: string, options: { ifAvailable: boolean }, work: (lock: object | null) => Promise<R>): Promise<R>;
}

/**
 * Runs `work` holding the lock: waiting for it, or, with `ifAvailable`,
 * resolving to `undefined` without running it when it is held now. In a
 * browser without Web Locks `work` runs all the same: the SDK's store
 * cannot open there either, and the work's reads say so.
 */
export async function withTransfersLock<R>(work: () => Promise<R>, ifAvailable: boolean): Promise<R | undefined> {
  return withLock(TRANSFERS_LOCK, work, ifAvailable);
}

/** The lock for asking the mint about restored notes. */
export const CHECKS_LOCK = 'purse:checks';

/** Runs `work` holding the checks' lock, unless another tab holds it now: then `undefined`, and that tab asks. */
export async function withChecksLock<R>(work: () => Promise<R>): Promise<R | undefined> {
  return withLock(CHECKS_LOCK, work, true);
}

async function withLock<R>(name: string, work: () => Promise<R>, ifAvailable: boolean): Promise<R | undefined> {
  const locks = (globalThis as unknown as { navigator?: { locks?: Locks } }).navigator?.locks;
  if (locks === undefined) {
    return work();
  }
  return locks.request(name, { ifAvailable }, async (lock) => (lock === null ? undefined : work()));
}
