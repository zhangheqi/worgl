// The requests this purse made, as Receive lists them: pure arithmetic on
// the purse's AskRecords (records.ts), so it can be tested.

import type { AskRecord } from './entries.ts';

/** How long an open request is the one Receive opens on: an ask in progress survives a reload, an old one never blocks a new ask. */
export const RECENT_ASK_MS = 30 * 60_000;

/** A request after a take of `value` for it: what came in so far, and taken once that is all it asked. */
export function askAfterTake(record: AskRecord, value: number, now: number): AskRecord {
  const got = record.got + value;
  if (record.state === 'open' && got >= record.amount) {
    return { ...record, got, state: 'taken', endedAt: now };
  }
  return { ...record, got };
}

/** The open requests for `mint`, newest first. */
export function openAsks(asks: readonly AskRecord[], mint: string): AskRecord[] {
  return asks.filter((ask) => ask.state === 'open' && ask.mint === mint).sort((a, b) => b.t - a.t);
}

/** The newest open request for `mint`, if it was made within `within` ms of `now`. */
export function newestOpen(
  asks: readonly AskRecord[],
  mint: string,
  now: number,
  within = RECENT_ASK_MS,
): AskRecord | undefined {
  const newest = openAsks(asks, mint)[0];
  return newest !== undefined && now - newest.t < within ? newest : undefined;
}

/** The ids of ended requests past the newest `keep` of them; open requests are never among them. */
export function endedToDrop(asks: readonly AskRecord[], keep: number): string[] {
  return asks
    .filter((ask) => ask.state !== 'open')
    .sort((a, b) => (b.endedAt ?? b.t) - (a.endedAt ?? a.t))
    .slice(keep)
    .map((ask) => ask.id);
}
