// What Holdings shows of one mint, in plain numbers and in this device's
// clock: the notes as the field draws them, the rotation the ruler spans,
// and the date to come back by. Made from what the SDK reports (see
// purses.ts), so the words and the field can be worked out and tested
// without it.

import type { HeldNote, MintInfo } from 'worgl';

/** A lane of the field: what the notes in it can do now. */
export type LaneKey = 'fade' | 'ready' | 'rest';

/** One note, as the field draws it and its tip names it. */
export interface Note {
  /** Its serial, `generation:index`: stable while the note sits still. */
  id: string;
  /** Its value. */
  v: number;
  /** Its layer, 0 for the first. */
  pos: number;
  /** When it fades unless it moves, by this device's clock; unknown without a schedule. */
  expiresAt: number | undefined;
  /** For a resting note, when it can move: the next rotation. */
  readyAt: number | undefined;
  /** When it came into the purse, from the purse's own log, if the log has it. */
  arrivedAt: number | undefined;
  /** Where it came from, if the log knows: a faucet, a hand-back taken in, or the change of a send. */
  source: Source | undefined;
}

/** The mint's rotation, by this device's clock. */
export interface Schedule {
  /** When the current rotation began: the next, less one period. */
  startMs: number;
  /** When the next comes. */
  nextMs: number;
  /** Milliseconds between rotations. */
  periodMs: number;
}

/** One mint's holdings, ready to draw and to put in words. */
export interface Holdings {
  /** The mint's number of layers: one lane each. */
  layers: number;
  /** The notes that count and can be placed: not expired, not unknown. */
  notes: Note[];
  /** The large number: the SDK's `Balance.total`. */
  total: number;
  /** When it rotates on its own; `undefined` for a mint that rotates only by hand, or a paused one. */
  schedule: Schedule | undefined;
  /** Whether the mint is paused. */
  paused: boolean;
  /** When the mint first takes a note of this purse's unless it moves (`Purse.firstExpiry`). */
  firstExpiry: number | undefined;
}

/** The lane a layer is in: the first fades, the last rests, every one between is ready. */
export function laneOf(pos: number, layers: number): LaneKey {
  if (pos <= 0) {
    return 'fade';
  }
  return pos >= layers - 1 ? 'rest' : 'ready';
}

/** The layers a lane spans. */
export function lanePositions(lane: LaneKey, layers: number): number[] {
  const all = Array.from({ length: layers }, (_, i) => i);
  return all.filter((pos) => laneOf(pos, layers) === lane);
}

/** The notes in a lane. */
export function notesIn(holdings: Holdings, lane: LaneKey): Note[] {
  return holdings.notes.filter((note) => laneOf(note.pos, holdings.layers) === lane);
}

/** The tokens in a lane. */
export function laneTotal(holdings: Holdings, lane: LaneKey): number {
  return sum(notesIn(holdings, lane));
}

/** The value of some notes. */
export const sum = (notes: readonly Note[]): number => notes.reduce((total, note) => total + note.v, 0);

/** The values of some notes, largest first. */
export const values = (notes: readonly Note[]): number[] => notes.map((note) => note.v).sort((a, b) => b - a);

/** The time of the `k`th rotation from now (0: the next), if the mint has a schedule. */
export function rotation(holdings: Holdings, k: number): number | undefined {
  const schedule = holdings.schedule;
  return schedule === undefined ? undefined : schedule.nextMs + k * schedule.periodMs;
}

/**
 * When the purse should be opened again: the first expiry, less a margin of
 * an hour, or a quarter period on a mint that rotates more often than every
 * four hours. Once `now` is within that margin, the first expiry itself,
 * so the date is never one that has gone; once that has gone too (until the
 * read after the rotation), none.
 */
export function comeBackAt(holdings: Holdings, now: number): number | undefined {
  const first = holdings.firstExpiry;
  if (holdings.notes.length === 0 || holdings.schedule === undefined || first === undefined || first <= now) {
    return undefined;
  }
  const at = first - Math.min(3_600_000, holdings.schedule.periodMs / 4);
  return at > now ? at : first;
}

/** The serial of a held note as the field names it. */
export const noteId = (serial: { generation: bigint; index: number }): string =>
  `${serial.generation}:${serial.index}`;

/** Where a note came from, as the purse's log knows it. */
export type Source = 'faucet' | 'received' | 'change';

/** What the purse's own log knows of a note: when it came, and from where. */
export interface Arrival {
  at: number;
  source: Source;
}

/**
 * Holdings from what the SDK reports, with its times (Unix seconds by the
 * mint's clock) turned into this device's milliseconds: `skew` is the
 * mint's clock less this device's, in seconds.
 */
export function holdingsFrom(
  info: MintInfo,
  held: readonly HeldNote[],
  total: bigint,
  firstExpiry: bigint | undefined,
  skew: number,
  arrivals: ReadonlyMap<string, Arrival>,
): Holdings {
  const local = (t: bigint | undefined): number | undefined =>
    t === undefined ? undefined : (Number(t) - skew) * 1000;
  const notes: Note[] = [];
  for (const note of held) {
    if (note.position === undefined || note.state === 'expired' || note.state === 'unknown') {
      continue;
    }
    const id = noteId(note.serial);
    const arrival = arrivals.get(id);
    notes.push({
      id,
      v: Number(note.amount),
      pos: note.position,
      expiresAt: local(note.expiresAt),
      readyAt: local(note.spendableAt),
      arrivedAt: arrival?.at,
      source: arrival?.source,
    });
  }
  const rotation = info.paused ? undefined : info.rotation;
  const nextMs = local(rotation?.nextAt);
  return {
    layers: info.layers,
    notes,
    total: Number(total),
    schedule:
      rotation === undefined || nextMs === undefined
        ? undefined
        : {
            startMs: nextMs - Number(rotation.periodSecs) * 1000,
            nextMs,
            periodMs: Number(rotation.periodSecs) * 1000,
          },
    paused: info.paused,
    firstExpiry: local(firstExpiry),
  };
}

/**
 * When the rotation came that dropped the layer of `generation`, by this
 * device's clock: the first layer is generation `oldest` since the last
 * rotation, so that layer went `oldest - generation - 1` rotations before
 * it. Without a schedule, or for a time still to come, `now`. A mint
 * rotated by hand keeps its schedule, so this is then only a guess.
 */
export function fadedAt(generation: bigint, info: MintInfo, skew: number, now: number): number {
  const rotation = info.paused ? undefined : info.rotation;
  if (rotation === undefined || generation >= info.oldest) {
    return now;
  }
  const back = info.oldest - generation;
  const t = (Number(rotation.nextAt - rotation.periodSecs * back) - skew) * 1000;
  return Math.min(now, t);
}
