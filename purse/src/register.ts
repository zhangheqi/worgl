// History's register: the purse's log of one mint as a register by day,
// each row with its time, what it was, what it changed and what the purse
// held after it, and what opening it says. Pure, so it is tested: the view
// (history.ts) builds it into elements.
//
// What is held after each row is worked back from what the purse holds
// now, the SDK's `Balance.total`, by what each later row changed, rather
// than kept with each entry: so the column always ends at the number on
// Holdings, and a send that ended hours after it began counts once, as it
// ended. A row whose change is not known (part of a send whose answer gave
// no value, or a send the mint turned part of away while the rest waits)
// leaves the rows before it with no number.

import { effectOf, gaveOf } from './effect.ts';
import { type AskRecord, LOG_KEPT, type LogEntry } from './entries.ts';
import { type RowOpens, WAITING_BODY, partialBody, recentRow } from './flow-words.ts';
import { cap, hm, notesWords, plusJoin, toks, when, word } from './format.ts';

const DAYS_LONG = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
const MONTHS_LONG = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];

/** A send whose answer came this long after it began says when it came. */
const LATE_MS = 60_000;

/** What an entry changed: effect.ts's, which the Held column steps by. */
export { effectOf };

/** A row of the register: an entry, and what the purse held after it, if that is known. */
export interface Row {
  entry: LogEntry;
  held: number | undefined;
}

/** A day of the register: its heading and its rows, newest first. */
export interface Day {
  /** The local date, `2026-10-21`: one heading each. */
  key: string;
  name: string;
  rows: Row[];
}

const pad = (n: number): string => String(n).padStart(2, '0');
const dayKey = (t: number): string => {
  const d = new Date(t);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
};

/** A day's heading: `Wednesday 21 October`, with the year when it is not this one. */
export function dayName(t: number, now: number): string {
  const d = new Date(t);
  const year = d.getFullYear() === new Date(now).getFullYear() ? '' : ` ${d.getFullYear()}`;
  return `${DAYS_LONG[d.getDay()]} ${d.getDate()} ${MONTHS_LONG[d.getMonth()]}${year}`;
}

/** Newest first, by when each happened: a fade is logged on the open after it, at the rotation's time. */
export function byTime(log: readonly LogEntry[]): LogEntry[] {
  return [...log].sort((a, b) => b.t - a.t || (b.id ?? 0) - (a.id ?? 0));
}

/**
 * The register of `mint`: its entries by day, newest first, each with what
 * was held after it, worked back from `total` (what the purse holds now),
 * or none before the first read.
 */
export function register(log: readonly LogEntry[], mint: string, total: number | undefined, now: number): Day[] {
  const days: Day[] = [];
  let held = total;
  for (const entry of byTime(log.filter((each) => each.mint === mint))) {
    const key = dayKey(entry.t);
    let day = days[days.length - 1];
    if (day === undefined || day.key !== key) {
      day = { key, name: dayName(entry.t, now), rows: [] };
      days.push(day);
    }
    day.rows.push({ entry, held });
    const effect = effectOf(entry);
    held = held === undefined || effect === undefined || held - effect < 0 ? undefined : held - effect;
  }
  return days;
}

/** What the register's foot sums: tokens faded and lost in the entries kept, and the oldest entry's time. */
export interface Foot {
  faded: number;
  lost: number;
  since: number | undefined;
}

export function footOf(log: readonly LogEntry[], mint: string): Foot {
  let faded = 0;
  let lost = 0;
  let since: number | undefined;
  for (const entry of log) {
    if (entry.mint !== mint) {
      continue;
    }
    since = since === undefined ? entry.t : Math.min(since, entry.t);
    if (entry.kind === 'faded') {
      faded += entry.amount;
    } else if (entry.kind === 'lost') {
      lost += entry.amount;
    } else if (entry.kind === 'sent' && ((entry.lost ?? 0) > 0 || entry.state === 'lost')) {
      // what the mint dropped of a send, whether or not the rest of it still waits
      lost += entry.lost ?? entry.asked ?? entry.amount;
    }
  }
  return { faded, lost, since };
}

/** The foot's words: what faded and was lost since the oldest entry kept. */
export function footWords(foot: Foot, now: number): { lab: string; body: string } {
  if (foot.since === undefined) {
    return { lab: 'Nothing yet', body: 'What comes in and goes out shows here, newest first.' };
  }
  const since = sinceWords(foot.since, now);
  const lab = foot.faded === 0 ? 'Nothing has faded' : `Faded · ${toks(foot.faded)}`;
  const faded =
    foot.faded === 0
      ? `Since ${since}, nothing here has faded.`
      : `Since ${since}, ${toks(foot.faded)} faded: ${foot.faded === 1 ? 'it' : 'they'} sat in the first layer when the mint rotated.`;
  const lost = foot.lost === 0 ? '' : ` ${cap(toks(foot.lost))} ${foot.lost === 1 ? 'was' : 'were'} lost on transfers whose answers the mint no longer kept.`;
  return { lab, body: `${faded}${lost}` };
}

/** Today's `14:02`, else `Mon 19 Oct`, as the foot names the oldest entry. */
function sinceWords(t: number, now: number): string {
  if (new Date(t).toDateString() === new Date(now).toDateString()) {
    return hm(t);
  }
  const d = new Date(t);
  return `${DAYS_LONG[d.getDay()]?.slice(0, 3)} ${d.getDate()} ${MONTHS_LONG[d.getMonth()]?.slice(0, 3)}`;
}

/** Where the register is kept, and how long. */
export const KEPT_WORDS = `The purse keeps the last ${LOG_KEPT} entries here, in this browser, with the tokens. Clearing the site’s data clears both.`;

/** What a row's button opens on Send, in words. */
const ACTIONS: Record<RowOpens, string> = {
  sending: 'Show the send',
  waiting: 'Show the send',
  handback: 'Show the hand-back again',
  lost: 'Show what happened',
};

/** A row as History draws it: Recent's title and change, a line under the title, and what opening it says. */
export interface RowWords {
  title: string;
  sub: string | undefined;
  delta: number | undefined;
  /** Its paragraphs, when it is open. */
  detail: string[];
  opens: RowOpens | undefined;
  /** The button that opens it on Send. */
  action: string | undefined;
}

/** What History knows beside an entry: whether a send's hand-back is kept, the request a take answered, the server's host. */
export interface RowContext {
  /** Whether the purse still keeps the send's hand-back: `undefined` until it has read what it keeps. */
  kept: boolean | undefined;
  ask: AskRecord | undefined;
  host: string;
  now: number;
}

const largestFirst = (values: readonly number[]): number[] => [...values].sort((a, b) => b - a);

/** The most notes a row's line spells out, so it stays one line on a phone. */
const AS_MAX = 4;

/** `As 4 + 1`, `One note`, `Eight notes`, or nothing when the values are not known. */
function asWords(values: readonly number[] | undefined): string | undefined {
  if (values === undefined || values.length === 0) {
    return undefined;
  }
  if (values.length === 1) {
    return 'One note';
  }
  return values.length > AS_MAX ? `${cap(word(values.length))} notes` : `As ${plusJoin(largestFirst(values))}`;
}

/** The notes, or the tokens when their values are not known: `Notes of 4 and 1`, `5 tokens`. */
const notesOr = (values: readonly number[] | undefined, amount: number): string =>
  values === undefined || values.length === 0 ? toks(amount) : notesWords(values);

/** The words of one row of the register. */
export function rowWords(entry: LogEntry, ctx: RowContext): RowWords {
  const row = recentRow(entry, ctx.kept === true, ctx.now);
  const opens = row.opens;
  // the change the Held column steps by, so the two always agree: a lost send's counts what went too
  const base = { title: row.title, delta: effectOf(entry), opens, action: opens === undefined ? undefined : ACTIONS[opens] };
  switch (entry.kind) {
    case 'faucet':
      return {
        ...base,
        sub: asWords(entry.values),
        detail: [`${cap(notesOr(entry.values, entry.amount))}, from the faucet at ${ctx.host}.`],
      };
    case 'received':
      return { ...base, sub: asWords(entry.values), detail: receivedDetail(entry, ctx) };
    case 'kept':
      return { ...base, sub: keptSub(entry), detail: keptDetail(entry) };
    case 'faded': {
      const as = asWords(entry.gone);
      return {
        ...base,
        sub: as === undefined ? 'In the first layer' : `${as} · in the first layer`,
        detail: [
          `${cap(notesOr(entry.gone, entry.amount))} sat in the first layer when the mint rotated${FADE_AT[entry.fadeAt ?? 'at'](hm(entry.t))}, so ${entry.amount === 1 || entry.gone?.length === 1 ? 'it' : 'they'} faded with it.`,
          'The purse moves tokens on only while it is on screen.',
        ],
      };
    }
    case 'lost':
      return {
        ...base,
        sub: 'A keep-alive transfer',
        detail: [
          'The purse sent this transfer again after its first answer did not reach it. The mint had applied it, but no longer kept the answer, so the note it brought is lost.',
        ],
      };
    case 'sent':
      return { ...base, ...sentWords(entry, ctx) };
  }
}

/** When a fade's time is the rotation's (`at`), a bound after it came, or a time the purse saw it gone by (entries.ts `fadeAt`). */
const FADE_AT: Record<NonNullable<LogEntry['fadeAt']> | 'at', (time: string) => string> = {
  at: (time) => ` at ${time}`,
  after: (time) => `, some time after ${time}`,
  by: (time) => `, at ${time} or before`,
};

/** How many notes an entry brought or moved, as its values or serials say. */
const noteCount = (entry: LogEntry): number | undefined =>
  entry.values !== undefined && entry.values.length > 0 ? entry.values.length : entry.serials.length > 0 ? entry.serials.length : undefined;

function receivedDetail(entry: LogEntry, ctx: RowContext): string[] {
  const ask = ctx.ask;
  const notes = cap(notesOr(entry.values, entry.amount));
  const one = noteCount(entry) === 1;
  const keys = (which: string): string =>
    one
      ? `The note went to a key this purse made for ${which} request, so only this purse could take it in.`
      : `Each note went to a key this purse made for ${which} request, so only this purse could take them in.`;
  if (ask === undefined) {
    return [`${notes}, taken in from their hand-back.`, keys('your')];
  }
  const of = `${notes}, taken in from their hand-back for your request for ${ask.amount}, made at ${when(ask.t, ctx.now)}.`;
  let state = '';
  if (ask.state === 'taken') {
    state = ' That request is taken in full.';
  } else if (ask.state === 'open' && ask.got < ask.amount) {
    state = ` ${ask.got} of its ${ask.amount} ${ask.got === 1 ? 'has' : 'have'} come so far.`;
  }
  return [`${of}${state}`, keys('that')];
}

/** How many notes a keep-alive entry moved: one transfer each. */
const keptCount = noteCount;

function keptSub(entry: LogEntry): string {
  const as = asWords(entry.values);
  const n = keptCount(entry);
  if (as !== undefined) {
    return `${as} · one layer on`;
  }
  return n === undefined ? 'One layer on' : `${cap(word(n))} ${n === 1 ? 'note' : 'notes'} · one layer on`;
}

function keptDetail(entry: LogEntry): string[] {
  const n = keptCount(entry);
  const how =
    n === undefined
      ? entry.amount === 1
        ? 'The purse moved it one layer on, in a transfer of its own, to a fresh key.'
        : 'The purse moved them one layer on, each note in a transfer of its own, to a fresh key.'
      : n === 1
        ? 'The purse moved the note one layer on, in a transfer of its own, to a fresh key.'
        : `The purse moved each of the ${word(n)} notes one layer on, in a transfer of its own, to a fresh key.`;
  return [
    `${cap(toks(entry.amount))} sat in the first layer and would have faded at the mint’s next rotation. ${how}`,
    'The mint can tell which notes one visit moved.',
  ];
}

/** A send's transfers, `k` of `n`, as its paragraphs name them: `it` for a send of one. */
const ofIts = (k: number, n: number): string => (n === 1 ? 'it' : `${word(k)} of its transfers`);

/** What the mint dropped of a send: the notes those transfers spent, and what of that was theirs. */
function droppedWords(entry: LogEntry, asked: number, n: number): string {
  const k = entry.dropped ?? 1;
  const lost = entry.lost ?? asked;
  const theirs = entry.lostTheirs ?? asked;
  const notes = k === 1 ? 'the note it spent is' : `the ${word(k)} notes they spent are`;
  const what = lost > theirs ? `the ${theirs} for them and your change, ${lost} in all` : `the ${theirs} for them`;
  return `The mint no longer keeps the answer${k === 1 ? '' : 's'} to ${ofIts(k, n)}, so ${notes} gone: ${what}.`;
}

/** What the mint turned away of a send: those notes came back. */
function turnedWords(entry: LogEntry, n: number): string {
  const k = entry.turnedAway ?? 1;
  return `The mint turned ${ofIts(k, n)} away, and ${k === 1 ? 'its note is' : 'their notes are'} back with you.`;
}

/** A send some of whose transfers ended where the purse did not see (entries.ts `blind`). */
const BLIND_WORDS = 'The purse did not see what the mint made of all of its transfers, so what it changed is not known.';

/** A send's hand-back: shown again while it is kept, gone once it is not, and nothing said while the purse has not read what it keeps. */
const handBackWords = (kept: boolean | undefined, line: string): string[] =>
  kept === true ? [line] : kept === false ? ['Their hand-back is no longer kept: its tokens have faded, taken in or not.'] : [];

/** A send's line, paragraphs and button. */
function sentWords(entry: LogEntry, ctx: RowContext): { sub: string | undefined; detail: string[] } {
  const asked = entry.asked ?? entry.amount;
  const n = entry.transfers ?? entry.spent?.length ?? 1;
  const spent = entry.spentValues !== undefined && entry.spentValues.length > 0 ? entry.spentValues : undefined;
  const yours = spent === undefined ? undefined : spent.length === 1 ? `your note of ${spent[0]}` : `your ${notesWords(spent)}`;
  const from = yours === undefined ? undefined : `From ${yours}`;
  // whether they went spaced out, or all at once, or later on a settle, the log does not say
  const signed = `${n === 1 ? 'One transfer' : `${cap(word(n))} transfers, one per note`}${yours === undefined ? '' : `, from ${yours}`}.`;
  // the change that came back, when the entry knows it
  const back = entry.back;
  const cameBack = back === undefined || back === 0 ? '' : ` and ${back} came back to you`;
  const late = entry.t2 !== undefined && entry.t2 - entry.t >= LATE_MS ? `, when the mint answered at ${when(entry.t2, ctx.now)}` : '';
  const blind = entry.blind === true ? [BLIND_WORDS] : [];
  const lost = entry.lost ?? 0;
  const turned = entry.turnedAway ?? 0;
  switch (entry.state) {
    case 'sending':
      return { sub: undefined, detail: [signed, 'The purse is sending it now. If the page closes first, the purse finishes it on the next open.'] };
    case 'waiting': {
      const told = [...(lost > 0 ? [droppedWords(entry, asked, n)] : []), ...(turned > 0 ? [turnedWords(entry, n)] : [])];
      return {
        sub: 'Waiting for the mint',
        detail: [
          signed,
          told.length === 0
            ? `The mint has not answered yet. ${WAITING_BODY}`
            : `${told.join(' ')} The rest is not lost: the mint has not answered it yet, and this purse sends it again when it can.`,
          ...blind,
        ],
      };
    }
    case 'turned':
      return {
        sub: n === 1 ? 'Nothing went · your note is back' : 'Nothing went · your notes are back',
        detail: [
          signed,
          entry.layerFull === true
            ? `The mint turned it away because its newest layer was full, so ${n === 1 ? 'your note is' : 'your notes are'} back with you and nothing went.`
            : `The mint’s rules refused it, so ${n === 1 ? 'your note is' : 'your notes are'} back with you and nothing went.`,
        ],
      };
    case 'lost': {
      const k = entry.dropped ?? 1;
      const gave = gaveOf(entry);
      return {
        sub:
          gave !== undefined && gave > 0
            ? `${entry.lost ?? asked} lost · the mint no longer kept ${k === 1 ? 'an answer' : 'some answers'}`
            : n === 1
              ? 'The mint no longer kept its answer'
              : `The mint no longer kept ${k === 1 ? 'an answer' : 'its answers'}`,
        detail: [
          signed,
          ...(gave !== undefined && gave > 0 ? [`${gave} of ${asked} went to them${cameBack}${late}.`] : []),
          droppedWords(entry, asked, n),
          ...(turned > 0 ? [turnedWords(entry, n)] : []),
          ...blind,
          ...(gave !== undefined && gave > 0 ? handBackWords(ctx.kept, partialBody(gave, asked, 'Their hand-back')) : []),
        ],
      };
    }
    case 'partial': {
      const went = entry.went;
      const part = went === undefined ? `Part of ${asked}` : `${went} of ${asked}`;
      const returned =
        entry.turnedAway === undefined ? 'those notes are' : entry.turnedAway === 1 ? 'its note is' : 'their notes are';
      return {
        sub: `${went === undefined ? 'Part' : `${went} of ${asked}`} went${entry.blind === true ? '' : ' · the rest turned away'}`,
        detail: [
          signed,
          `${part} went to them${cameBack}${late}.${entry.blind === true ? '' : ` The mint turned the rest away, and ${returned} back with you.`}`,
          ...blind,
          ...handBackWords(ctx.kept, partialBody(went, asked, 'Their hand-back')),
        ],
      };
    }
    default: {
      // went through: handback while its hand-back is kept, done once it is not
      const went = entry.went ?? (entry.blind === true ? undefined : asked);
      const tail = back === undefined ? undefined : back > 0 ? `${back} back` : 'nothing back';
      const how = from ?? (n === 1 ? 'One transfer' : `${cap(word(n))} transfers`);
      return {
        sub: tail === undefined ? (from === undefined ? undefined : how) : `${how} · ${tail}`,
        detail: [
          signed,
          ...(went === undefined
            ? []
            : [`${went} went to them${back === 0 ? ', and nothing came back' : cameBack}${late}. Whether they have taken it in, this purse cannot see.`]),
          ...blind,
          ...handBackWords(ctx.kept, 'You can show the hand-back again until their tokens fade.'),
        ],
      };
    }
  }
}
