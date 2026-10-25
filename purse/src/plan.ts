// Which notes a send spends, worked out as the SDK will before it signs
// anything: `pay` signs and saves at once and has no dry run, so Confirm
// draws this plan, and the preview checks it against what `pay` did. It
// mirrors `Holder::plan`, `parts`, `at_most` and `split`
// (crates/worgl-sdk/src/holder.rs) and `Denominations::decompose`
// (crates/worgl/src/policy.rs), in plain numbers: past
// Number.MAX_SAFE_INTEGER it says nothing rather than guess.
//
// Also here: the policy as the mint's info carries it (the SDK's MintInfo
// leaves it out), the sizes "Ask in smaller notes" offers, and which "not
// enough ready" case a refused plan is.

/** The note values a mint makes. */
export type Denoms = { kind: 'powers' } | { kind: 'any' } | { kind: 'list'; values: number[] };

/** What the planner needs of a mint's policy. */
export interface Policy {
  denominations: Denoms;
  /** The most notes one transfer may make: what it pays and its change. */
  maxOutputs: number;
}

/** The most notes a request asks for: the SDK's MAX_REQUESTED_NOTES. */
export const MAX_REQUESTED_NOTES = 256;

/** The SDK's defaults, `Policy::cash_like`. */
const DEFAULT_POLICY: Policy = { denominations: { kind: 'powers' }, maxOutputs: 16 };

const record = (value: unknown): Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

const whole = (value: unknown): value is number => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;

/** The denominations, from `"powers_of_two"`, `"any"` or `{"list": [1, 5]}`; anything else is the default. */
function denomsFrom(value: unknown): Denoms {
  if (value === 'any') {
    return { kind: 'any' };
  }
  const values = record(value)['list'];
  if (
    Array.isArray(values) &&
    values.length > 0 &&
    values.every((v, i) => whole(v) && v > 0 && (i === 0 || v > (values[i - 1] as number)))
  ) {
    return { kind: 'list', values: values as number[] };
  }
  return { kind: 'powers' };
}

/**
 * The policy, the layer new notes are minted into and the number of layers,
 * from a mint's info as the server sends it (`GET …/info`).
 */
export function policyFrom(info: unknown): { policy: Policy; mintLayer: number | undefined; layers: number } {
  const fields = record(info);
  const policy = record(fields['policy']);
  const maxOutputs = policy['max_outputs'];
  const layers = fields['layers'];
  const mintLayer = fields['mint_layer'];
  return {
    policy: {
      denominations: 'denominations' in policy ? denomsFrom(policy['denominations']) : DEFAULT_POLICY.denominations,
      maxOutputs: whole(maxOutputs) ? maxOutputs : DEFAULT_POLICY.maxOutputs,
    },
    mintLayer: whole(mintLayer) ? mintLayer : undefined,
    layers: Array.isArray(layers) ? layers.length : 0,
  };
}

/** The largest power of two at or under `n` (n ≥ 1). */
function topPower(n: number): number {
  let p = 1;
  while (p * 2 <= n) {
    p *= 2;
  }
  return p;
}

/** `amount` in notes of `d`, largest first, as `[value, count]` pairs; `undefined` when it cannot be made. */
function decompose(d: Denoms, amount: number): [number, number][] | undefined {
  if (amount === 0) {
    return undefined;
  }
  if (d.kind === 'any') {
    return [[amount, 1]];
  }
  if (d.kind === 'powers') {
    const out: [number, number][] = [];
    let rest = amount;
    for (let p = topPower(amount); p >= 1; p /= 2) {
      if (rest >= p) {
        out.push([p, 1]);
        rest -= p;
      }
    }
    return out;
  }
  const out: [number, number][] = [];
  let rest = amount;
  for (const value of [...d.values].reverse()) {
    const count = Math.floor(rest / value);
    if (count > 0) {
      rest -= count * value;
      out.push([value, count]);
    }
  }
  return rest === 0 ? out : undefined;
}

/**
 * `amount` in notes of `d` no larger than `largest`, largest first, as at
 * most `limit` notes: `inexact` when it cannot be made, `too_many` past the
 * limit.
 */
export function split(d: Denoms, amount: number, largest: number | undefined, limit: number): number[] | 'inexact' | 'too_many' {
  if (amount === 0) {
    return [];
  }
  let pieces: [number, number][] | undefined;
  if (largest === 0) {
    return 'inexact';
  } else if (largest === undefined) {
    pieces = decompose(d, amount);
  } else if (d.kind === 'any') {
    pieces = [[largest, Math.floor(amount / largest)]];
    if (amount % largest !== 0) {
      pieces.push([amount % largest, 1]);
    }
  } else if (d.kind === 'powers') {
    const top = topPower(largest);
    pieces = [[top, Math.floor(amount / top)], ...(decompose(d, amount % top) ?? [])];
  } else {
    const allowed = d.values.filter((v) => v <= largest);
    pieces = allowed.length === 0 ? undefined : decompose({ kind: 'list', values: allowed }, amount);
  }
  if (pieces === undefined) {
    return 'inexact';
  }
  const count = pieces.reduce((n, [, k]) => n + k, 0);
  if (count > limit) {
    return 'too_many';
  }
  return pieces.filter(([, k]) => k > 0).flatMap(([value, k]) => Array.from({ length: k }, () => value));
}

/** A note that can be spent, in the order `notesAt` lists them. */
export interface Spendable {
  id: string;
  v: number;
  /** Its layer, 0 for the first. */
  pos: number;
}

/** One note's transfer: the note, the notes it sends them, its change, and the layer its notes land in. */
export interface Part {
  note: Spendable;
  theirs: number[];
  change: number[];
  lands: number;
}

/** What {@link plan} makes of a request: the parts, or why it cannot be sent now, as the SDK refuses it. */
export type Planned =
  | { ok: true; parts: Part[] }
  | { ok: false; why: 'unaffordable'; spendable: number; needed: number }
  | { ok: false; why: 'fragmented'; largest: number; atMost: number | undefined }
  | { ok: false; why: 'too_many_outputs'; maxOutputs: number };

const total = (values: readonly number[]): number => values.reduce((a, b) => a + b, 0);

/** The parts, or why none fit: `notes` too small, or `change` that would take too many outputs. */
function parts(
  d: Denoms,
  maxOutputs: number,
  notes: readonly Spendable[],
  outputs: readonly number[],
  layers: number,
): Part[] | 'notes' | 'change' {
  const max = Math.max(1, maxOutputs);
  const changeOf = (room: number): number[] | undefined => {
    const out = split(d, room, undefined, max);
    return Array.isArray(out) ? out : undefined;
  };
  const lands = (note: Spendable): number => Math.min(Math.max(0, layers - 1), note.pos + 1);
  const needed = total(outputs);
  // the smallest single note that covers everything, change included
  for (const note of notes) {
    if (note.v < needed) {
      continue;
    }
    const change = changeOf(note.v - needed);
    if (change !== undefined && outputs.length + change.length <= max) {
      return [{ note, theirs: [...outputs], change, lands: lands(note) }];
    }
  }
  // otherwise the largest notes first, each paying the largest outputs it can hold
  const byValue = (a: number, b: number): number => b - a;
  const wanted = [...outputs].sort(byValue);
  const left = [...notes];
  const plan: Part[] = [];
  let changeLimited = false;
  while (wanted.length > 0) {
    const note = left.pop();
    if (note === undefined) {
      break;
    }
    let room = note.v;
    const theirs: number[] = [];
    let k = 0;
    while (k < wanted.length && theirs.length < max) {
      const out = wanted[k] as number;
      if (out <= room) {
        room -= out;
        theirs.push(out);
        wanted.splice(k, 1);
      } else {
        k++;
      }
    }
    // give back the smallest outputs until the change fits
    let change: number[] | undefined;
    for (;;) {
      const fits = changeOf(room);
      if (fits !== undefined && theirs.length + fits.length <= max) {
        change = fits;
        break;
      }
      if (theirs.length === 0) {
        change = undefined;
        break;
      }
      changeLimited = true;
      const out = theirs.pop() as number;
      room += out;
      wanted.push(out);
    }
    wanted.sort(byValue);
    if (change !== undefined && theirs.length > 0) {
      plan.push({ note, theirs, change, lands: lands(note) });
    }
  }
  if (wanted.length > 0) {
    return changeLimited ? 'change' : 'notes';
  }
  return plan;
}

/**
 * The largest note a request for `needed`, split as `requestAtMost` splits
 * it, may ask for and still be paid from `notes` (smallest first), whose
 * largest is `largest`; `undefined` when none can.
 */
function atMost(d: Denoms, maxOutputs: number, notes: readonly Spendable[], needed: number, largest: number, layers: number): number | undefined {
  let sizes: number[];
  if (d.kind === 'any') {
    sizes = notes.map((note) => note.v);
  } else if (d.kind === 'powers') {
    sizes = [];
    for (let p = 1; p <= largest; p *= 2) {
      sizes.push(p);
    }
  } else {
    sizes = [...d.values];
  }
  const candidates = [...new Set(sizes.filter((size) => size <= largest))].sort((a, b) => b - a);
  return candidates.find((size) => {
    const amounts = split(d, needed, size, MAX_REQUESTED_NOTES);
    return Array.isArray(amounts) && Array.isArray(parts(d, maxOutputs, notes, amounts, layers));
  });
}

const safe = (n: number): boolean => Number.isSafeInteger(n) && n >= 0;

/**
 * Plans a send of `outputs` (the request's notes, in its order) from
 * `notes`, those that can be spent now (spendable or fading), in the order
 * `notesAt` lists them. `undefined` when an amount is past what a JS number
 * holds exactly.
 */
export function plan(notes: readonly Spendable[], outputs: readonly number[], policy: Policy, layers: number): Planned | undefined {
  const needed = total(outputs);
  const have = total(notes.map((note) => note.v));
  if (![needed, have, ...outputs, ...notes.map((note) => note.v)].every(safe)) {
    return undefined;
  }
  const sorted = [...notes].sort((a, b) => a.v - b.v);
  if (have < needed) {
    return { ok: false, why: 'unaffordable', spendable: have, needed };
  }
  const d = policy.denominations;
  const made = parts(d, policy.maxOutputs, sorted, outputs, layers);
  if (made === 'change') {
    return { ok: false, why: 'too_many_outputs', maxOutputs: policy.maxOutputs };
  }
  if (made === 'notes') {
    const largest = sorted[sorted.length - 1]?.v ?? 0;
    return { ok: false, why: 'fragmented', largest, atMost: atMost(d, policy.maxOutputs, sorted, needed, largest, layers) };
  }
  return { ok: true, parts: made };
}

/**
 * The sizes "Ask in smaller notes" offers for `amount`, largest first: the
 * allowed sizes under the largest note of the plain split that keep the
 * request at or under 256 notes, at most eight. None under `any`, whose
 * sizes are arbitrary.
 */
export function atMostOptions(d: Denoms, amount: number): number[] {
  const plain = split(d, amount, undefined, MAX_REQUESTED_NOTES);
  if (!Array.isArray(plain) || plain.length === 0 || d.kind === 'any') {
    return [];
  }
  const top = plain[0] as number;
  const fits = (size: number): boolean => Array.isArray(split(d, amount, size, MAX_REQUESTED_NOTES));
  const out: number[] = [];
  if (d.kind === 'powers') {
    for (let size = top / 2; size >= 1 && out.length < 8; size /= 2) {
      if (!fits(size)) {
        break;
      }
      out.push(size);
    }
    return out;
  }
  return [...d.values]
    .filter((v) => v < top && fits(v))
    .sort((a, b) => b - a)
    .slice(0, 8);
}

/** Where a send's change lands: all resting, all ready, or some of each. */
export type ChangeLands = 'rest' | 'ready' | 'mixed';

/** The numbers Confirm and Sending draw from a plan. */
export interface Shown {
  parts: Part[];
  /** The notes spent, in plan order. */
  chosen: Spendable[];
  /** All the change, in plan order. */
  back: number[];
  /** Ready now (fading included) and resting now. */
  readyNow: number;
  restingNow: number;
  /** Ready and resting once the send is answered. */
  readyAfter: number;
  laterAfter: number;
  /** Where the change lands, when there is any. */
  changeLands: ChangeLands | undefined;
}

/** What a plan of `parts` leaves, from `readyNow` and `restingNow`, at a mint of `layers` layers. */
export function shown(parts: Part[], readyNow: number, restingNow: number, layers: number): Shown {
  const last = layers - 1;
  const chosen = parts.map((part) => part.note);
  const back = parts.flatMap((part) => part.change);
  const changeIn = (rest: boolean): number =>
    total(parts.filter((part) => (part.lands >= last) === rest).flatMap((part) => part.change));
  const resting = changeIn(true);
  const ready = changeIn(false);
  return {
    parts,
    chosen,
    back,
    readyNow,
    restingNow,
    readyAfter: readyNow - total(chosen.map((note) => note.v)) + ready,
    laterAfter: restingNow + resting,
    changeLands: resting + ready === 0 ? undefined : ready === 0 ? 'rest' : resting === 0 ? 'ready' : 'mixed',
  };
}

/** Which "not enough ready" case a request is. */
export type ShortCase =
  /** More than this purse holds, ready and resting together (`has` 0: it is empty). */
  | { kind: 'over'; has: number }
  /** Nothing ready, and the resting notes would do. */
  | { kind: 'none'; has: number }
  /** Some ready, and ready and resting together would do. */
  | { kind: 'later'; has: number }
  /** No ready note is big enough. */
  | { kind: 'notes'; largest: number; atMost: number | undefined; maxOut: number; waitHelps: boolean }
  /** Too many small notes for one transfer. */
  | { kind: 'outputs'; maxOutputs: number };

/**
 * The case a refused plan is. `ready` is what can be spent now, `frozen`
 * what rests; "wait" is offered only when the plan from both would go, so
 * that "then this request can go as it is" is true.
 */
export function shortCase(
  refused: Exclude<Planned, { ok: true }>,
  ready: readonly Spendable[],
  frozen: readonly Spendable[],
  outputs: readonly number[],
  policy: Policy,
  layers: number,
): ShortCase {
  const readyTotal = total(ready.map((note) => note.v));
  const has = readyTotal + total(frozen.map((note) => note.v));
  const all = (): Planned | undefined => plan([...ready, ...frozen], outputs, policy, layers);
  const maxOut = Math.max(0, ...outputs);
  const fromRefusal = (planned: Planned | undefined, waitHelps: boolean): ShortCase | undefined => {
    if (planned?.ok === false && planned.why === 'fragmented') {
      return { kind: 'notes', largest: planned.largest, atMost: planned.atMost, maxOut, waitHelps };
    }
    if (planned?.ok === false && planned.why === 'too_many_outputs') {
      return { kind: 'outputs', maxOutputs: planned.maxOutputs };
    }
    return undefined;
  };
  if (refused.why === 'unaffordable') {
    if (has < total(outputs)) {
      return { kind: 'over', has };
    }
    const later = all();
    if (later?.ok === true) {
      return readyTotal === 0 ? { kind: 'none', has } : { kind: 'later', has };
    }
    return fromRefusal(later, false) ?? { kind: 'over', has };
  }
  return fromRefusal(refused, all()?.ok === true) ?? { kind: 'over', has };
}

/**
 * What transfers the mint dropped took with them, from each one's JSON and
 * its `atStake` (what this purse still expected of it: its change): `lost`,
 * the notes they spent, and `theirs`, the part of that for the receiver. A
 * transfer makes exactly what its note holds, so the note's value is the
 * sum of its outputs. A transfer that does not read counts its change alone.
 */
export function droppedValue(dropped: readonly { request: string; atStake: bigint | undefined }[]): { lost: number; theirs: number } {
  let lost = 0;
  let change = 0;
  for (const refusal of dropped) {
    const mine = Number(refusal.atStake ?? 0n);
    change += mine;
    lost += spentBy(refusal.request) ?? mine;
  }
  return { lost, theirs: lost - change };
}

/** The value a transfer spends: the sum of the notes it makes, or `undefined` when its JSON does not read. */
function spentBy(transfer: string): number | undefined {
  let outputs: unknown;
  try {
    outputs = record(JSON.parse(transfer))['outputs'];
  } catch {
    return undefined;
  }
  if (!Array.isArray(outputs)) {
    return undefined;
  }
  const amounts = outputs.map((output) => record(output)['amount']);
  return amounts.every(whole) ? total(amounts) : undefined;
}
