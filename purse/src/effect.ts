// What each entry of the purse's log changed the mint's total by: the
// number Recent and History show beside it, and the step History's Held
// column works back by. Pure, so it is tested.

import type { LogEntry } from './entries.ts';

/**
 * What an entry changed the mint's total by, the SDK's `Balance.total`,
 * which counts a send's change while it is on its way and leaves out what
 * goes to the receiver: so a send still on its way has taken what it asks
 * already. `undefined` when it is not known.
 */
export function effectOf(entry: LogEntry): number | undefined {
  switch (entry.kind) {
    case 'faucet':
    case 'received':
    case 'restored':
      return entry.amount;
    case 'faded':
    case 'lost':
    case 'checked':
      return less(entry.amount);
    case 'kept':
    case 'backup':
      return 0;
    case 'sent':
      break;
  }
  const asked = entry.asked ?? entry.amount;
  if (entry.state === 'sending') {
    return less(asked);
  }
  if (entry.blind === true) {
    // what some of its transfers came to, the purse never saw
    return undefined;
  }
  const lost = entry.lost ?? 0;
  // the change the transfers the mint dropped would have brought: the total counted it, but it went with their notes
  const change = lost - (entry.lostTheirs ?? 0);
  switch (entry.state) {
    case 'turned':
      // every transfer it had was turned away: their notes are back
      return less(change);
    case 'waiting':
      // what it asks is out of the total, but for what a transfer turned away gave back, which is not known
      return entry.turnedAway === undefined ? less(asked + change) : undefined;
    default:
      // what the answered transfers gave them, and every note the dropped ones spent
      if (entry.went !== undefined) {
        return less(entry.went + lost);
      }
      return entry.state === 'partial' || entry.turnedAway !== undefined ? undefined : less(asked + change);
  }
}

/**
 * What a lost send's answered transfers gave them: as the entry says, or,
 * with none turned away and every end seen, what the dropped ones did not
 * carry. `undefined` when that is not known.
 */
export function gaveOf(entry: LogEntry): number | undefined {
  if (entry.went !== undefined) {
    return entry.went;
  }
  const asked = entry.asked ?? entry.amount;
  const n = entry.transfers ?? entry.spent?.length ?? 1;
  const k = entry.dropped ?? 1;
  return entry.turnedAway === undefined && entry.blind !== true && k < n ? asked - (entry.lostTheirs ?? asked) : undefined;
}

/** Taking `n` away: 0 for nothing, never -0. */
const less = (n: number): number => (n === 0 ? 0 : -n);
