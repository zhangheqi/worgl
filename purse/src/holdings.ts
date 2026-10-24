// What Holdings shows of one mint, in plain numbers and in this device's
// clock: the notes as the field draws them, the rotation the ruler spans,
// and the date to come back by. Made from what the SDK reports (see
// purses.ts), so the words and the field can be worked out and tested
// without it.

import type { HeldNote, MintInfo } from 'worgl';

import type { Moving } from './keep.ts';

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
  /** Where it came from, if the log knows: a faucet, or keep-alive, which moved it a layer on. */
  source: 'faucet' | 'kept' | undefined;
  /** On its way a layer on in a refresh transfer the mint has not answered yet: drawn where it was. */
  moving: boolean;
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

/** What the purse's own log knows of a note: when it came, and from where. */
export interface Arrival {
  at: number;
  source: 'faucet' | 'kept';
}

/** An entry of the purse's log (records.ts `LogEntry`, which needs IndexedDB to load), as far as arrivals read it. */
export interface Logged {
  t: number;
  kind: 'faucet' | 'faded' | 'kept' | 'lost';
  mint: string;
  serials: readonly string[];
}

/**
 * What the purse's log knows of each note of `mint`: when it came, and from
 * where, a faucet or keep-alive. The log is newest first, so a note's latest
 * entry wins. A note keep-alive just moved (`hopped`, by when) counts from
 * its hop until its run's entry is written, so it keeps its place in the
 * field.
 */
export function arrivalsOf(
  log: readonly Logged[],
  mint: string,
  hopped: ReadonlyMap<string, number>,
): Map<string, Arrival> {
  const known = new Map<string, Arrival>();
  for (const entry of log) {
    if (entry.mint === mint && (entry.kind === 'faucet' || entry.kind === 'kept')) {
      for (const id of entry.serials) {
        if (!known.has(id)) {
          known.set(id, { at: entry.t, source: entry.kind });
        }
      }
    }
  }
  for (const [id, at] of hopped) {
    if (!known.has(id)) {
      known.set(id, { at, source: 'kept' });
    }
  }
  return known;
}

/**
 * Holdings from what the SDK reports, with its times (Unix seconds by the
 * mint's clock) turned into this device's milliseconds: `skew` is the
 * mint's clock less this device's, in seconds. The notes on their way in
 * refresh transfers (`moving`), which the SDK no longer lists but still
 * counts in the total, are drawn in the layer they leave until the mint
 * answers. One whose layer the mint dropped since is drawn in the first
 * layer, where its answer would land if the mint applied it before the
 * rotation; one dropped two layers back is gone either way, so it is not
 * drawn and not counted, as a note the purse prunes is not.
 */
export function holdingsFrom(
  info: MintInfo,
  held: readonly HeldNote[],
  total: bigint,
  firstExpiry: bigint | undefined,
  skew: number,
  arrivals: ReadonlyMap<string, Arrival>,
  moving: readonly Moving[] = [],
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
      moving: false,
    });
  }
  const rotation = info.paused ? undefined : info.rotation;
  let gone = 0;
  for (const note of moving) {
    const layer = Number(note.generation - info.oldest);
    if (layer < -1) {
      gone += note.v;
      continue;
    }
    const pos = Math.max(0, layer);
    if (pos >= info.layers) {
      continue;
    }
    const arrival = arrivals.get(note.id);
    notes.push({
      id: note.id,
      v: note.v,
      pos,
      expiresAt: rotation === undefined ? undefined : local(rotation.nextAt + BigInt(pos) * rotation.periodSecs),
      readyAt: undefined,
      arrivedAt: arrival?.at,
      source: arrival?.source,
      moving: true,
    });
  }
  const nextMs = local(rotation?.nextAt);
  return {
    layers: info.layers,
    notes,
    total: Number(total) - gone,
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

/** What fades first, and when, as the purse last saw it: kept with its records, for an open the mint does not answer. */
export interface LastFade {
  /** When, by this device's clock. */
  at: number;
  /** How many tokens. */
  amount: number;
}

/** What fades first in `holdings`, and when; `null` when nothing does (no notes, or no schedule). */
export function firstFade(holdings: Holdings): LastFade | null {
  if (holdings.notes.length === 0 || holdings.schedule === undefined || holdings.firstExpiry === undefined) {
    return null;
  }
  const low = Math.min(...holdings.notes.map((note) => note.pos));
  return { at: holdings.firstExpiry, amount: sum(holdings.notes.filter((note) => note.pos === low)) };
}

/** What the purse saved of a mint's info (records.ts `SavedInfo`), as far as {@link savedHoldings} reads it. */
export interface Saved {
  layers: number;
  periodSecs: number | undefined;
  paused: boolean;
  /** `null`: nothing fades; missing: never saved. */
  fades?: LastFade | null | undefined;
}

/**
 * A mint's holdings as the purse saved them with the mint (`SavedInfo`), for
 * one this page has not read, since it has not answered yet or does not:
 * only the tokens that fade first, as one note in the layer their time puts
 * them in, and the period, so that the date to come back by and its words
 * can be worked out without the mint. Never drawn. Undefined when, as the
 * purse last saw it, nothing fades or the mint was paused, or the time has
 * gone.
 */
export function savedHoldings(saved: Saved, now: number): Holdings | undefined {
  const fades = saved.fades;
  if (fades === undefined || fades === null || saved.paused || saved.periodSecs === undefined || fades.at <= now) {
    return undefined;
  }
  const periodMs = saved.periodSecs * 1000;
  // they fade at a rotation, a whole number of periods after the next one
  const pos = Math.min(saved.layers - 1, Math.max(0, Math.ceil((fades.at - now) / periodMs) - 1));
  const nextMs = fades.at - pos * periodMs;
  return {
    layers: saved.layers,
    notes: [
      { id: 'saved', v: fades.amount, pos, expiresAt: fades.at, readyAt: undefined, arrivedAt: undefined, source: undefined, moving: false },
    ],
    total: fades.amount,
    schedule: { startMs: nextMs - periodMs, nextMs, periodMs },
    paused: false,
    firstExpiry: fades.at,
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
