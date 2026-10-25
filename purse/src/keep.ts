// Keep-alive's pure logic: how a pass spreads its hops over the visit,
// which note a transfer spends, which notes are on their way, which new
// notes the answered hops brought, when a mint is due, and the counts the
// back row draws. No DOM and no SDK at run time, so it can be tested.
//
// A hop is one refresh transfer: it spends one note of the purse's at layer
// `p` and gets back one note of the same value at layer `p + 1`, to a fresh
// key. Keep-alive is the SDK's refresh and nothing more: only what fades at
// the next rotation moves, one layer.

import type { HeldNote, MintInfo, ScheduledTransfer, Spacing } from 'worgl';

/** Before the first hop: one to three seconds. */
export const KEEP_FIRST: Spacing = { minSecs: 1, maxSecs: 3 };
/** Between hops: two to five seconds. */
export const KEEP_GAP: Spacing = { minSecs: 2, maxSecs: 5 };
/** The SDK's own window before a rotation (`REFRESH_MARGIN_SECS`): no hop is planned past it. */
export const KEEP_MARGIN_SECS = 300;
/** The time a mint with no schedule gives a pass, as the SDK's `UNSCHEDULED_REFRESH_SECS`. */
export const UNSCHEDULED_SECS = 60;
/** The most a pass spreads its hops over: a page cannot count on running longer. */
export const RUN_MAX_SECS = 120;
/** The most a settle on open spreads its resends over, inside the mint's receipt of about 128 s. */
export const SETTLE_MAX_SECS = 60;

/** How near its rotation a mint must be before keep-alive moves its notes while Send shows a plan of them: time enough for a pass after. */
export const SEND_WAIT_SECS = 900;

/**
 * Whether a pass leaves a mint alone because Send shows a plan of its notes,
 * not signed yet: moving them, or landing its outgoing transfers, would make
 * the plan untrue. Not once the rotation is nearer than {@link SEND_WAIT_SECS},
 * nor for a mint with no schedule (`left` undefined), since keeping its
 * tokens alive comes first; Send then says what is ready changed.
 */
export const waitsForSend = (shows: boolean, left: number | undefined): boolean =>
  shows && left !== undefined && left > SEND_WAIT_SECS;

/** Seconds a pass may spread its hops over: the time left before the rotation, less the SDK's margin. */
export function budgetSecs(leftSecs: number | undefined): number {
  if (leftSecs === undefined) {
    return UNSCHEDULED_SECS;
  }
  return Math.max(0, Math.min(RUN_MAX_SECS, leftSecs - Math.min(KEEP_MARGIN_SECS, leftSecs / 2)));
}

/**
 * The plan's transfers in its own order, which the SDK made random, a few
 * seconds apart and ending within `budget`. `planRefresh` spreads them over
 * the whole time left before the rotation, which can be hours; the purse
 * spreads them over the visit only.
 */
export function respace(
  plan: readonly ScheduledTransfer[],
  budget: number,
  random: () => number = Math.random,
): ScheduledTransfer[] {
  if (plan.length === 0) {
    return [];
  }
  if (budget <= 0) {
    return plan.map(({ request }) => ({ request, afterSecs: 0 }));
  }
  const times: number[] = [];
  let t = KEEP_FIRST.minSecs + random() * (KEEP_FIRST.maxSecs - KEEP_FIRST.minSecs);
  times.push(t);
  for (let i = 1; i < plan.length; i++) {
    t += KEEP_GAP.minSecs + random() * (KEEP_GAP.maxSecs - KEEP_GAP.minSecs);
    times.push(t);
  }
  // a near rotation squeezes the gaps, never the order
  const k = t > budget ? budget / t : 1;
  return plan.map(({ request }, i) => ({ request, afterSecs: Math.round((times[i] as number) * k * 1000) / 1000 }));
}

/** Whether a respaced plan keeps its hops a few seconds apart, as the words say, or a near rotation squeezed them. */
export function spacedPlan(plan: readonly ScheduledTransfer[]): boolean {
  const times = plan.map((each) => each.afterSecs).sort((a, b) => a - b);
  return times.every((t, i) => i === 0 || t - (times[i - 1] as number) >= KEEP_GAP.minSecs - 0.01);
}

/** Settle's gaps for `count` transfers within `budget`: whole seconds, since settle hands them to `BigInt`. */
export function spacingFor(count: number, budget: number): Spacing {
  const maxSecs = Math.max(0, Math.min(KEEP_GAP.maxSecs, Math.floor(budget / Math.max(1, count))));
  return { minSecs: Math.min(KEEP_GAP.minSecs, maxSecs), maxSecs };
}

/** Whether settle's gaps are the purse's few seconds, as the words say, or squeezed by a near rotation or many transfers. */
export const spaced = (s: Spacing): boolean => s.minSecs >= KEEP_GAP.minSecs && s.maxSecs >= KEEP_GAP.maxSecs;

/** The seconds left before a mint's next rotation, by its clock (`skew`: the mint's clock less this device's). */
export function leftSecs(info: MintInfo, skew: number, now: number): number | undefined {
  const rotation = info.paused ? undefined : info.rotation;
  return rotation === undefined ? undefined : Number(rotation.nextAt) - (now / 1000 + skew);
}

// ---------- which note a transfer spends ----------

/** A note's fields, read from its 149 bytes (`note-format.md`). */
export interface NoteFields {
  /** Its mint's issuer key, in hex. */
  issuer: string;
  generation: bigint;
  index: number;
  amount: bigint;
  /** Its one-time holder key, in hex. */
  holder: string;
}

/** The bytes of unpadded base64url text, or undefined if it is not that. */
function base64url(text: string): Uint8Array | undefined {
  if (!/^[A-Za-z0-9_-]*$/.test(text)) {
    return undefined;
  }
  try {
    const plain = text.replace(/-/g, '+').replace(/_/g, '/');
    const binary = atob(plain.padEnd(Math.ceil(plain.length / 4) * 4, '='));
    return Uint8Array.from(binary, (c) => c.charCodeAt(0));
  } catch {
    return undefined;
  }
}

const hex = (bytes: Uint8Array): string => [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');

/** A big-endian unsigned integer. */
function uint(bytes: Uint8Array): bigint {
  let n = 0n;
  for (const b of bytes) {
    n = (n << 8n) | BigInt(b);
  }
  return n;
}

/**
 * A note's fields from its text (199 characters of unpadded base64url, 149
 * bytes): version 1, issuer, generation, index, amount (never 0), holder,
 * then a signature this does not check. Undefined if it is not one.
 */
export function decodeNote(text: string): NoteFields | undefined {
  const bytes = base64url(text);
  if (bytes === undefined || bytes.length !== 149 || bytes[0] !== 1) {
    return undefined;
  }
  const amount = uint(bytes.subarray(45, 53));
  if (amount === 0n) {
    return undefined;
  }
  return {
    issuer: hex(bytes.subarray(1, 33)),
    generation: uint(bytes.subarray(33, 41)),
    index: Number(uint(bytes.subarray(41, 45))),
    amount,
    holder: hex(bytes.subarray(53, 85)),
  };
}

/** A note a transfer spends: its mint, its serial as the field names it (`generation:index`), its value and generation. */
export interface Spent {
  mint: string;
  id: string;
  v: number;
  generation: bigint;
}

/** A note on its way in a refresh transfer, drawn in its lane until the mint answers. */
export interface Moving {
  id: string;
  v: number;
  generation: bigint;
}

/** What a transfer looks like on the wire (`http-api.md`). */
interface TransferBody {
  inputs?: { note?: unknown }[];
  outputs?: unknown[];
}

function body(request: string): TransferBody | undefined {
  try {
    const parsed = JSON.parse(request) as unknown;
    return typeof parsed === 'object' && parsed !== null ? (parsed as TransferBody) : undefined;
  } catch {
    return undefined;
  }
}

/**
 * The notes a transfer's inputs spend, from
 * `{"inputs":[{"note","signature"}],"outputs":[{"amount","holder"}]}`;
 * none if it does not read.
 */
export function spentBy(request: string): Spent[] {
  const inputs = body(request)?.inputs;
  if (!Array.isArray(inputs)) {
    return [];
  }
  const spent: Spent[] = [];
  for (const input of inputs) {
    const note = typeof input?.note === 'string' ? decodeNote(input.note) : undefined;
    if (note !== undefined) {
      spent.push({ mint: note.issuer, id: `${note.generation}:${note.index}`, v: Number(note.amount), generation: note.generation });
    }
  }
  return spent;
}

/** How many notes the mint's answer to a transfer holds: one per output. */
export function outputsOf(request: string): number {
  const outputs = body(request)?.outputs;
  return Array.isArray(outputs) ? outputs.length : 0;
}

/**
 * Each answer's notes, in the order of `answered`: the SDK's
 * `Submitted.notes` holds one note per output of each transfer answered, in
 * the order the answers came.
 */
export function answersOf(answered: readonly string[], notes: readonly string[]): string[][] {
  const each: string[][] = [];
  let at = 0;
  for (const request of answered) {
    const n = outputsOf(request);
    each.push(notes.slice(at, at + n));
    at += n;
  }
  return each;
}

/** A spent note's key across mints, as `sendInputs` holds them. */
export const spentKey = (spent: Spent): string => `${spent.mint}/${spent.id}`;

/** The one note a refresh hop spends, or undefined for a transfer that is a send's (it spends a note one of the purse's sends spends) or does not read. */
export function hopOf(request: string, sendInputs: ReadonlySet<string>): Spent | undefined {
  const spent = spentBy(request);
  if (spent.length !== 1 || spent.some((each) => sendInputs.has(spentKey(each)))) {
    return undefined;
  }
  return spent[0];
}

/** This mint's notes on their way in refresh transfers: outgoing ones that spend no note of a send's. */
export function movingNotes(outgoing: readonly string[], sendInputs: ReadonlySet<string>, mint: string): Moving[] {
  const moving: Moving[] = [];
  for (const request of outgoing) {
    const spent = spentBy(request);
    if (spent.some((each) => sendInputs.has(spentKey(each)))) {
      continue;
    }
    for (const each of spent) {
      if (each.mint === mint) {
        moving.push({ id: each.id, v: each.v, generation: each.generation });
      }
    }
  }
  return moving;
}

/**
 * The serial of the note each answered hop brought, in the hops' order:
 * the first answer note not yet matched of the same mint and value, one
 * generation on, since a hop's output lands one layer past its input;
 * undefined for a hop whose note the answers do not hold.
 */
export function broughtBy(hops: readonly Spent[], answerNotes: readonly string[]): (string | undefined)[] {
  const notes = answerNotes.map(decodeNote);
  const used = new Set<number>();
  return hops.map((hop) => {
    const at = notes.findIndex(
      (note, i) =>
        note !== undefined &&
        !used.has(i) &&
        note.issuer === hop.mint &&
        Number(note.amount) === hop.v &&
        note.generation === hop.generation + 1n,
    );
    const note = notes[at];
    if (at < 0 || note === undefined) {
      return undefined;
    }
    used.add(at);
    return `${note.generation}:${note.index}`;
  });
}

/** The serials of the notes the answered hops brought (see {@link broughtBy}). */
export const keptNotes = (hops: readonly Spent[], answerNotes: readonly string[]): string[] =>
  broughtBy(hops, answerNotes).filter((id): id is string => id !== undefined);

/** A note a hop spent that was the purse's before the run: its serial and value. */
export interface Moved {
  id: string;
  v: number;
}

/**
 * What a run's answered hops kept, in the order they were answered: the
 * value of the tokens they moved, each counted once though a note may
 * move twice (a hop resent after a rotation lands in the first layer, and
 * moves on again); the notes they left; and the notes they spent that the
 * purse held before the run.
 */
export function keptOf(hops: readonly { hop: Spent; brought: string | undefined }[]): {
  amount: number;
  serials: string[];
  values: number[];
  spent: Moved[];
} {
  const serials: string[] = [];
  // a refresh keeps a note's value: the note a hop brought is worth what it spent
  const values: number[] = [];
  const spent: Moved[] = [];
  let amount = 0;
  for (const { hop, brought } of hops) {
    const again = serials.indexOf(hop.id);
    if (again >= 0) {
      serials.splice(again, 1);
      values.splice(again, 1);
    } else {
      amount += hop.v;
      spent.push({ id: hop.id, v: hop.v });
    }
    if (brought !== undefined) {
      serials.push(brought);
      values.push(hop.v);
    }
  }
  return { amount, serials, values, spent };
}

/**
 * Keep-alive's log entry for a rotation that has one already, from an
 * earlier run of the same visit or another tab's: one entry with both
 * runs' tokens, a note `more` spent that `old` brought counted once, and
 * the notes the two left.
 */
export function mergeKept<E extends { amount: number; serials: string[]; values?: number[] }>(
  old: { amount: number; serials: readonly string[]; values?: readonly number[] | undefined },
  more: E,
  spent: readonly Moved[],
): E {
  const again = spent.filter((each) => old.serials.includes(each.id));
  const gone = new Set(again.map((each) => each.id));
  const merged: E = {
    ...more,
    amount: old.amount + more.amount - again.reduce((total, each) => total + each.v, 0),
    serials: [...old.serials.filter((id) => !gone.has(id)), ...more.serials],
  };
  // the values only while both runs have them, each in its serials' order
  const oldValues = old.values;
  if (oldValues === undefined || oldValues.length !== old.serials.length || more.values === undefined) {
    delete merged.values;
  } else {
    merged.values = [...oldValues.filter((_, i) => !gone.has(old.serials[i] ?? '')), ...more.values];
  }
  return merged;
}

// ---------- when a mint is due ----------

/** What decides whether a pass plans a refresh for a mint. */
export interface DueInput {
  /** Notes at layer 0 that are not on their way. */
  fading: number;
  /** Notes on their way in refresh transfers. */
  moving: number;
  paused: boolean;
  /** A hop at the mint's current first layer was refused for good (`layer_full`, `policy`): planning it again would be refused again. */
  blocked: boolean;
}

/** Whether a pass plans a refresh for this mint now. Settling what is outgoing needs no such check. */
export const due = (m: DueInput): boolean => !m.paused && !m.blocked && m.fading > 0;

/** A mint's {@link DueInput} from what the SDK reports, and whether a refusal blocks its first layer now. */
export function dueInput(notes: readonly HeldNote[], moving: number, info: MintInfo, blockedAt: bigint | undefined): DueInput {
  return {
    fading: notes.filter((note) => note.state === 'expiring').length,
    moving,
    paused: info.paused,
    blocked: blockedAt !== undefined && blockedAt === info.oldest,
  };
}

// ---------- progress ----------

/** The shown mint's hops in a pass: their value, and how many are done (answered or refused) of how many. */
export interface Progress {
  amount: number;
  done: number;
  total: number;
}

/** "1 of 2": the transfer on its way or waiting, counted from 1. */
export const stepOf = (p: Progress): number => Math.min(p.done + 1, p.total);

/** The boxes: one per hop up to `max`, each a share of the hops above that; `true` for done. */
export function boxes(p: Progress, max: number): boolean[] {
  const n = Math.min(p.total, max);
  const on = p.total === 0 ? 0 : Math.floor((p.done * n) / p.total);
  return Array.from({ length: n }, (_, i) => i < on);
}
