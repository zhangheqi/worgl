// What Holdings says: the one sentence under the total, the date to come
// back by, the lane and note tips and the field's labels for assistive
// tech, the first run's legend, keep-alive's progress, results and
// failures, and Recent's entries. Pure: each takes the holdings and the
// time now.

import { cap, hm, listWords, notesWords, toks, when, word } from './format.ts';
import {
  type Holdings,
  type LaneKey,
  type LastFade,
  type Note,
  comeBackAt,
  firstFade,
  laneOf,
  laneTotal,
  notesIn,
  rotation,
  sum,
  values,
} from './holdings.ts';
import { type Progress, stepOf } from './keep.ts';

/**
 * Whether the purse keeps tokens alive while it is on screen: the SDK's
 * refresh, one layer for what fades at the next rotation. A page that is
 * hidden (another tab in front, the phone's screen off) keeps nothing alive,
 * so the words say "on screen", not "open", and "closed or hidden", not
 * "closed". Each function's `keepsAlive` says whether it can keep these
 * tokens alive now; the app passes `false` while it cannot (the mint
 * unreachable, a full next layer, a store problem, a paused mint), and
 * every line then says what happens without it.
 */
export const KEEPS_ALIVE = true;

/** A piece of the sentence: words, a number that opens its lane's tip, or words in italics. */
export type Segment = string | { n: number; lane: LaneKey } | { em: string };

/** The lane's name in the field. */
export const LANE_NAME: Record<LaneKey, string> = { fade: 'Fading', ready: 'Ready', rest: 'Resting' };

/** The state's word for one note: fading, ready, resting. */
const STATE: Record<LaneKey, string> = { fade: 'fading', ready: 'ready', rest: 'resting' };

/** When the `k`th rotation from now comes, in words; a mint with no schedule has no time to name. */
export function rotationWords(holdings: Holdings, k: number, now: number): string {
  const t = rotation(holdings, k);
  if (t !== undefined) {
    return when(t, now);
  }
  return k === 0 ? 'the next rotation' : 'a later rotation';
}

/** When a note fades unless it moves. */
function fadesAt(holdings: Holdings, note: Note, now: number): string {
  return note.expiresAt === undefined ? rotationWords(holdings, note.pos, now) : when(note.expiresAt, now);
}

/** The lowest layer any note sits in. */
const lowest = (notes: readonly Note[]): number => Math.min(...notes.map((note) => note.pos));

/** Tokens just taken in from a hand-back, and the lane they landed in, while Holdings says so. */
export interface Received {
  amount: number;
  lane: LaneKey;
}

/** The end of a lane's clause while it holds tokens just taken in. */
export const justTook = (amount: number): string => `, including the ${amount} you just took`;

/**
 * The sentence under the total: `69 are ready; 123 rest until 16:00.
 * Nothing fades before Thu 08:00.` While tokens just taken in show, the
 * clause of the lane they landed in ends `, including the 5 you just took`.
 */
export function sentence(
  holdings: Holdings,
  now: number,
  keepsAlive = KEEPS_ALIVE,
  received?: Received,
): Segment[] {
  if (holdings.notes.length === 0) {
    // a send's change counts in the total from the moment it is signed, before the mint has made its notes
    return holdings.total > 0 ? [`Nothing is held this moment: ${holdings.total} is on its way back from the mint.`] : ['This purse is empty.'];
  }
  const ready = laneTotal(holdings, 'ready');
  const resting = laneTotal(holdings, 'rest');
  const fading = laneTotal(holdings, 'fade');
  const next = rotationWords(holdings, 0, now);
  const out: Segment[] = [];
  const took = (lane: LaneKey): string => (received !== undefined && received.lane === lane ? justTook(received.amount) : '');
  if (ready > 0) {
    out.push({ n: ready, lane: 'ready' }, `${ready === 1 ? ' is ready' : ' are ready'}${took('ready')}`);
  } else {
    out.push('Nothing is ready');
  }
  if (resting > 0) {
    out.push('; ', { n: resting, lane: 'rest' }, `${resting === 1 ? ' rests' : ' rest'} until ${next}${took('rest')}`);
  }
  if (fading > 0) {
    const at = resting > 0 ? 'then' : `at ${next}`;
    if (keepsAlive) {
      out.push(
        '; ',
        { n: fading, lane: 'fade' },
        ` would fade ${at}, so this purse keeps ${fading === 1 ? 'it' : 'them'} `,
        { em: 'alive' },
        ' while it is on screen.',
      );
    } else {
      out.push(
        '; ',
        { n: fading, lane: 'fade' },
        ` ${fading === 1 ? 'fades' : 'fade'} ${at} unless ${fading === 1 ? 'it moves' : 'they move'}.`,
      );
    }
  } else if (holdings.paused) {
    out.push('. The mint is paused, so nothing fades until it resumes.');
  } else if (holdings.schedule === undefined) {
    out.push('. Nothing fades until the mint rotates.');
  } else {
    const first = holdings.firstExpiry ?? Math.min(...holdings.notes.map((note) => fadesAtMs(holdings, note)));
    out.push(`. Nothing fades before ${when(first, now)}.`);
  }
  return out;
}

const fadesAtMs = (holdings: Holdings, note: Note): number =>
  note.expiresAt ?? rotation(holdings, note.pos) ?? Number.POSITIVE_INFINITY;

/** The sentence as plain text, as a screen reader hears it. */
export const plain = (segments: readonly Segment[]): string =>
  segments.map((part) => (typeof part === 'string' ? part : 'n' in part ? String(part.n) : part.em)).join('');

/** What the come-back row and the desktop's note say with no notes. */
const nothingHeld = (keepsAlive: boolean): string => (keepsAlive ? 'Nothing to keep alive yet.' : 'Nothing to fade yet.');

/** The come-back column's heading on desktop, which the phone's row starts with. */
export const comeBackHeading = (keepsAlive = KEEPS_ALIVE): string => (keepsAlive ? 'Open again before' : 'First to fade');

/** The notes in the lowest layer: the first to fade. */
const firstNotes = (holdings: Holdings): Note[] => {
  const pos = lowest(holdings.notes);
  return holdings.notes.filter((note) => note.pos === pos);
};

/**
 * The come-back row on the phone: `Open again before` and the date, or why
 * there is none. While the purse cannot keep these tokens alive (see
 * {@link KEEPS_ALIVE}), opening it again keeps nothing, so the row says when
 * the first tokens fade instead.
 */
export function comeBackRow(
  holdings: Holdings,
  now: number,
  keepsAlive = KEEPS_ALIVE,
): { lead: string; date: string } | { none: string } {
  if (holdings.notes.length === 0) {
    return { none: nothingHeld(keepsAlive) };
  }
  if (holdings.paused) {
    return { none: 'This mint is paused.' };
  }
  if (holdings.schedule === undefined || holdings.firstExpiry === undefined) {
    return { none: 'This mint rotates only by hand.' };
  }
  const at = comeBackAt(holdings, now);
  if (at === undefined) {
    // the first expiry has come: the read after the rotation shows what it left
    return { none: 'The mint is rotating now.' };
  }
  if (!keepsAlive) {
    return { lead: 'First tokens fade at ', date: fadesAt(holdings, firstNotes(holdings)[0] as Note, now) };
  }
  return { lead: 'Open again before ', date: when(at, now) };
}

/**
 * The line under the come-back date on desktop: which tokens go first, and
 * when, or that nothing is held.
 */
export function firstToGo(holdings: Holdings, now: number, keepsAlive = KEEPS_ALIVE): string {
  if (holdings.notes.length === 0) {
    return nothingHeld(keepsAlive);
  }
  const first = firstNotes(holdings);
  const amount = sum(first);
  const state = STATE[laneOf(lowest(first), holdings.layers)];
  const goes = amount === 1 ? 'token goes' : 'tokens go';
  if (keepsAlive) {
    const at = fadesAt(holdings, first[0] as Note, now);
    // three lines at most in the narrowest desktop column, with a date a week out (styles.css keeps three)
    return `Kept alive only while the purse is on screen. Left closed or hidden, your ${amount} ${state} ${goes} at ${at}.`;
  }
  // the date above it is when these go
  return `Your ${amount} ${state} ${goes} then unless ${amount === 1 ? 'it moves' : 'they move'}.`;
}

/** A lane's label button, for assistive tech: what is in it and when it moves. */
export function laneDescription(holdings: Holdings, lane: LaneKey, now: number, keepsAlive = KEEPS_ALIVE): string {
  const notes = notesIn(holdings, lane);
  const total = sum(notes);
  const which = notes.length === 0 ? 'no notes' : notesWords(values(notes));
  const they = notes.length === 1 ? 'It' : 'They';
  let more = '';
  if (total > 0 && lane === 'fade') {
    more = ` ${they} ${notes.length === 1 ? 'fades' : 'fade'} at ${rotationWords(holdings, 0, now)} unless ${notes.length === 1 ? 'it moves' : 'they move'}${keepsAlive ? `; while this purse is on screen, it keeps ${notes.length === 1 ? 'it' : 'them'} alive` : ''}.`;
  } else if (total > 0 && lane === 'rest') {
    more = ` Ready at ${rotationWords(holdings, 0, now)}.`;
  } else if (total > 0) {
    more = ` ${they} can move now.`;
  }
  const step = notes.length > 0 ? ' The left and right arrow keys step through its notes.' : '';
  return `${LANE_NAME[lane]}: ${toks(total)}, ${which}.${more} Show where in the field.${step}`;
}

/** A number in the sentence, for assistive tech. */
export const numberDescription = (n: number, lane: LaneKey): string =>
  `${n}, ${STATE[lane]}: show where in the field`;

/** A tip: its head, its first paragraph, then the paragraphs in the muted ink. */
export interface Tip {
  head: string;
  lines: string[];
}

/** What keep-alive shows the mint, said where the person reads about it: hops of one visit come from one address, close together. */
const VISIT_SEEN = 'The mint can tell which notes one visit moved.';

/** The tip a lane's label or number opens. */
export function laneTip(holdings: Holdings, lane: LaneKey, now: number, keepsAlive = KEEPS_ALIVE): Tip {
  const notes = notesIn(holdings, lane);
  const vals = values(notes);
  const total = sum(notes);
  const one = notes.length === 1;
  const they = one ? 'it' : 'them';
  const They = one ? 'It' : 'They';
  const next = rotationWords(holdings, 0, now);
  if (lane === 'fade') {
    const head = `Fading · ${total}`;
    if (total === 0) {
      return {
        head,
        lines: keepsAlive
          ? [
              'Nothing is in the first layer.',
              'While this purse is on screen, it keeps notes alive: one transfer at a time, it moves them a layer further from the first, down the field.',
              VISIT_SEEN,
            ]
          : ['Nothing is in the first layer.'],
      };
    }
    const lines = [
      `${cap(notesWords(vals))} in the first layer. ${They} ${one ? 'fades' : 'fade'} at ${next} unless ${one ? 'it moves' : 'they move'}.`,
    ];
    // without keep-alive, opening the purse again does not save them: the first line says all there is
    if (keepsAlive) {
      lines.push(
        `While this purse is on screen, it keeps ${they} alive: one transfer at a time, it moves ${they} a layer further from the first, down the field.`,
        VISIT_SEEN,
      );
    }
    return { head, lines };
  }
  if (lane === 'rest') {
    const head = `Resting · ${total}`;
    if (total === 0) {
      return {
        head,
        lines: [
          'Nothing rests in the last layer.',
          'What you take in, and change from a send, usually rests here until the next rotation.',
        ],
      };
    }
    return {
      head,
      lines: [
        `${cap(notesWords(vals))} in the last layer. Notes that just moved rest here.`,
        keepsAlive
          ? `Ready at ${next}. If this purse stays closed or hidden, ${one ? 'it fades' : 'they fade'} at ${fadesAt(holdings, notes[0] as Note, now)}.`
          : `Ready at ${next}. ${one ? 'It fades' : 'They fade'} ${fadesAt(holdings, notes[0] as Note, now)} unless ${one ? 'it moves' : 'they move'}.`,
      ],
    };
  }
  const head = `Ready · ${total}`;
  if (total === 0) {
    return { head, lines: ['Nothing is ready right now.', `Resting notes are ready at ${next}.`] };
  }
  const lo = lowest(notes);
  const upper = notes.filter((note) => note.pos === lo);
  const rest = notes.filter((note) => note.pos > lo);
  // with keep-alive each time takes an "at", which a time today needs ("they fade at 18:00")
  const at = keepsAlive ? 'at ' : '';
  let closed: string;
  if (one || rest.length === 0) {
    closed = `${one ? 'it fades' : 'they fade'} ${at}${fadesAt(holdings, notes[0] as Note, now)}`;
  } else {
    const later = [...new Set(rest.map((note) => fadesAt(holdings, note, now)))];
    const restAt = later.length === 1 ? `${at}${later[0]}` : `from ${fadesAt(holdings, rest.reduce((a, b) => (a.pos <= b.pos ? a : b)), now)}`;
    closed = `the ${listWords(values(upper))} ${upper.length === 1 ? 'fades' : 'fade'} ${at}${fadesAt(holdings, upper[0] as Note, now)}, the rest ${restAt}`;
  }
  const split = !one && rest.length > 0;
  const lines = [
    `${cap(notesWords(vals))}. Sends use ${they}.`,
    keepsAlive
      ? `${They} can move now. If this purse stays closed or hidden, ${closed}.`
      : `${They} can move now. ${cap(closed)}${split ? ',' : ''} unless ${one ? 'it moves' : 'they move'}.`,
  ];
  if (rest.length > 0) {
    const readyLayers = holdings.layers - 2;
    const which = readyLayers === 2 ? 'The upper of the two ready layers' : 'The upper of the ready layers';
    lines.push(
      `${which} is one closer to the first: the ${listWords(values(upper))} there ${upper.length === 1 ? 'starts' : 'start'} to fade at ${rotationWords(holdings, lo - 1, now)}.`,
    );
  }
  return { head, lines };
}

/** The tip a note opens, by hover or by tap: what it is, where it came from and when it moves. */
export function noteTip(
  holdings: Holdings,
  note: Note,
  mintName: string,
  now: number,
  keepsAlive = KEEPS_ALIVE,
): Tip {
  const lane = laneOf(note.pos, holdings.layers);
  const lines: string[] = [];
  if (note.arrivedAt !== undefined) {
    const at = when(note.arrivedAt, now);
    if (note.source === 'faucet') {
      lines.push(`From the ${mintName} faucet at ${at}.`);
    } else if (note.source === 'kept') {
      lines.push(`Kept alive at ${at}.`);
    } else if (note.source === 'received') {
      lines.push(`Received at ${at}.`);
    } else if (note.source === 'change') {
      lines.push(`Change from your send at ${at}.`);
    }
  }
  if (note.moving) {
    // on its way: the tip says it fades only when the purse cannot move it now (the mint did not answer, or is paused)
    lines.push(
      keepsAlive
        ? 'Moving a layer on now, to a fresh key.'
        : `Waiting to move a layer on. Fades at ${fadesAt(holdings, note, now)} unless it moves.`,
    );
  } else if (lane === 'fade') {
    lines.push(
      `Fades at ${rotationWords(holdings, 0, now)} unless it moves.${keepsAlive ? ' While this purse is on screen, it keeps it alive.' : ''}`,
    );
  } else {
    const ready = note.readyAt === undefined ? rotationWords(holdings, 0, now) : when(note.readyAt, now);
    const unless = keepsAlive ? 'if left closed or hidden' : 'unless it moves';
    lines.push(`${lane === 'rest' ? `Ready ${ready}` : 'Can move now'} · fades ${fadesAt(holdings, note, now)} ${unless}`);
  }
  return { head: `Note of ${note.v} · ${STATE[lane]}`, lines };
}

/** The field's name for assistive tech. */
export const fieldLabel = (layers: number): string =>
  `The field: your notes on the mint’s ${word(layers)} layers, one square per token`;

/** What a tip says, as one line to read out: each part ends as a sentence. */
export const tipWords = (tip: Tip): string =>
  [tip.head, ...tip.lines].map((part) => (/[.!?]$/.test(part) ? part : `${part}.`)).join(' ');

/** The first run's three states, as its legend names them. */
export function legend(keepsAlive = KEEPS_ALIVE): { kind: LaneKey; name: string; words: string }[] {
  return [
    { kind: 'ready', name: LANE_NAME.ready, words: 'Can move now.' },
    { kind: 'rest', name: LANE_NAME.rest, words: 'Just arrived; ready at the next rotation.' },
    {
      kind: 'fade',
      name: LANE_NAME.fade,
      words: keepsAlive
        ? 'Sat still too long; kept alive while the purse is on screen.'
        : 'Sat still too long; fades at the next rotation unless it moves.',
    },
  ];
}

/** The caption under the field on desktop. */
export const caption = (notes: number): string =>
  `One square is one token · ${notes} ${notes === 1 ? 'note' : 'notes'} · first layer at the top`;

/** What the live region says after a take from the faucet. */
export function tookWords(amount: number, lane: LaneKey | undefined, holdings: Holdings, now: number): string {
  const after =
    lane === 'rest' ? ` It rests until ${rotationWords(holdings, 0, now)}.` : lane === 'ready' ? ' It is ready now.' : '';
  return `Took ${amount} from the faucet.${after}`;
}

/** What the live region says when the mint rotates while the purse is open. */
export function rotatedWords(at: number, holdings: Holdings): string {
  const ready = laneTotal(holdings, 'ready');
  const now = ready === 0 ? 'Nothing is ready now.' : `${ready} ${ready === 1 ? 'is' : 'are'} ready now.`;
  return `The mint rotated at ${hm(at)}. ${now}`;
}

// ---------- keep-alive ----------

/** The back row's lead while keep-alive moves notes: `Keeping 128 alive`. */
export const keepingLead = (n: number): string => `Keeping ${n} alive`;

/** Which hop is under way: `1 of 2`. */
export const keepingCount = (p: Progress): string => `${stepOf(p)} of ${p.total}`;

/** Under the count, when another tab of this purse is the one moving them. */
export const KEEPING_ELSEWHERE = 'In another tab';

/** The desktop column's line while this tab moves them; `spaced` is false when a near rotation squeezed the gaps. */
export const keepingNote = (n: number, spaced: boolean): string =>
  `Moving ${n} on, one layer per transfer,${spaced ? ' a few seconds apart,' : ''} each to a fresh key.`;

/** The desktop column's line while another tab moves them. */
export const KEEPING_ELSEWHERE_NOTE =
  'Another tab of this purse is moving them on, one layer per transfer, a few seconds apart.';

/** What the live region says once the hops are planned and the first waits. */
export const keepingStart = (n: number, count: number, spaced: boolean): string =>
  `Keeping ${n} alive: ${word(count)} ${count === 1 ? 'transfer' : 'transfers'}${count > 1 && spaced ? ', a few seconds apart' : ''}.`;

/** What the live region says once they moved: the new date to come back by, if there is one. */
export const keptWords = (n: number, comeBack: number | undefined, now: number): string =>
  comeBack === undefined ? `Kept ${n} alive.` : `Kept ${n} alive. Open again before ${when(comeBack, now)}.`;

/** What the live region says of a hop the mint applied whose answer it no longer keeps. */
export const lostWords = (n: number, notes: number): string =>
  `Lost ${n}: the mint moved ${notes === 1 ? 'it' : 'them'} a layer on, but its reply never reached this purse, and the mint no longer keeps that reply.`;

/** What did not move, for the failure words: the notes' values, their layer, and the mint's holdings for when it rotates. */
export interface Stuck {
  /** The values of the notes that did not move. */
  values: readonly number[];
  /** Their layer: they fade at the rotation that drops it. */
  pos: number;
  holdings: Holdings;
  now: number;
}

/** `5 tokens`, `They fade at 16:00`: the amount of tokens that did not move, and when they go. */
function tokensFade({ values: vals, pos, holdings, now }: Stuck): { n: string; they: string; at: string } {
  const n = vals.reduce((t, v) => t + v, 0);
  const at = rotationWords(holdings, pos, now);
  return { n: toks(n), they: n === 1 ? `It fades at ${at}` : `They fade at ${at}`, at };
}

/** The notes that did not move, and when they go: `notes of 4 and 1`, `They fade at 16:00`. */
function notesFade({ values: vals, pos, holdings, now }: Stuck): { notes: string; they: string } {
  const sorted = [...vals].sort((a, b) => b - a);
  return { notes: notesWords(sorted), they: `${sorted.length === 1 ? 'it fades' : 'they fade'} at ${rotationWords(holdings, pos, now)}` };
}

/** How the failure words end: the purse tries again only while it can run. */
const AGAIN = 'The purse asks again each minute while it is on screen.';

/** The mint did not answer the hops: what fades, and when, unless the purse reaches it. */
export function unreachableWords(host: string, stuck: Stuck): string {
  const { n, they } = tokensFade(stuck);
  return `The mint at ${host} did not answer, so ${n} could not move. ${they} unless the purse reaches it before then. ${AGAIN}`;
}

/**
 * The mint did not answer, with nothing of the purse's on its way or
 * fading: what goes first, and when, as the purse last saw it.
 */
export function unreachableFirstWords(host: string, first: LastFade, now: number): string {
  const fade = first.amount === 1 ? 'fades' : 'fade';
  return `The mint at ${host} did not answer. As the purse last saw them, ${toks(first.amount)} ${fade} at ${when(first.at, now)} unless the purse reaches it before then. ${AGAIN}`;
}

/** The mint did not answer, and the purse has not read it since this version kept what fades first. */
export const unreachableUnknownWords = (host: string): string =>
  `The mint at ${host} did not answer, so the purse cannot tell yet when its tokens there fade. ${AGAIN}`;

/**
 * What the purse says when a mint does not answer, from the newest holdings
 * it has of it, or else what it last saved (`null`: nothing fades; missing:
 * never saved): what fades and when, unless the purse reaches it. Undefined
 * when there is nothing to fade, so the read's own words stand.
 */
export function unreachableFor(
  host: string,
  holdings: Holdings | undefined,
  last: LastFade | null | undefined,
  now: number,
): string | undefined {
  let first = last;
  if (holdings !== undefined) {
    const fading = holdings.notes.filter((note) => note.pos === 0);
    if (fading.length > 0) {
      return unreachableWords(host, { values: values(fading), pos: 0, holdings, now });
    }
    first = firstFade(holdings);
  }
  if (first === undefined) {
    return unreachableUnknownWords(host);
  }
  return first === null || first.at <= now ? undefined : unreachableFirstWords(host, first, now);
}

/** The mint's answer to a hop did not check out, so the purse kept none of it. */
export function uncheckedWords(host: string, stuck: Stuck): string {
  const { n, they } = tokensFade(stuck);
  return `The answer from ${host} did not check out, so ${n} could not move. ${they} unless a later answer checks out before then. ${AGAIN}`;
}

/** The mint was paused when the hops went: nothing fades while it is. */
export function pausedKeepWords(host: string, amount: number): string {
  return `The mint at ${host} is paused, so ${toks(amount)} could not move. Nothing fades while it is paused; once it resumes, the purse moves ${amount === 1 ? 'it' : 'them'} while it is on screen.`;
}

/** The mint's next layer is full: the hops were cancelled, and the notes go with their layer. */
export function layerFullWords(host: string, stuck: Stuck): string {
  const { notes, they } = notesFade(stuck);
  return `The mint at ${host} has no room in the next layer, so ${notes} could not move. ${cap(they)}.`;
}

/** The mint's rules, as it publishes them now, refuse the hops. */
export function policyWords(host: string, stuck: Stuck): string {
  const { notes, they } = notesFade(stuck);
  return `The rules of the mint at ${host} no longer let ${notes} move, so ${they}.`;
}

/** The browser refused to save a hop's plan or its answer. */
export const keepStoreWords = 'This browser refused to save, so the purse stopped moving tokens. It tries again each minute while it is on screen.';

/** Under the come-back row, when its date is for another mint's tokens than those shown. */
export const elsewhereRow = (name: string): string => `For your tokens at ${name}`;

/**
 * The desktop come-back column's line when its date is for another mint's
 * tokens than those shown: which, and what happens to them.
 */
export function elsewhereNote(name: string, holdings: Holdings, now: number, keepsAlive = KEEPS_ALIVE): string {
  if (holdings.notes.length === 0) {
    return `For your tokens at ${name}.`;
  }
  const first = firstNotes(holdings);
  const amount = sum(first);
  const one = amount === 1;
  if (keepsAlive) {
    return `For your tokens at ${name}. Left closed or hidden, ${amount} there ${one ? 'goes' : 'go'} at ${fadesAt(holdings, first[0] as Note, now)}.`;
  }
  // the date above it is when these go
  return `For your tokens at ${name}: ${amount} there ${one ? 'goes' : 'go'} then unless ${one ? 'it moves' : 'they move'}.`;
}

/**
 * What one visit keeps a token for on a mint that rotates every
 * `periodSecs`: `1 to 2 hours`, `30 to 60 minutes`, `2 to 4 minutes`,
 * `30 to 60 seconds`.
 */
export function visitSpan(periodSecs: number): string {
  let a: number;
  let unit: string;
  if (periodSecs >= 3600 && periodSecs % 3600 === 0) {
    a = periodSecs / 3600;
    unit = 'hour';
  } else if (periodSecs >= 60) {
    a = Math.round(periodSecs / 60);
    unit = 'minute';
  } else {
    a = periodSecs;
    unit = 'second';
  }
  return `${a} to ${2 * a} ${unit}s`;
}

/** The first run's line for a mint that rotates more often than every four hours. */
export const visitWords = (periodSecs: number): string =>
  `A visit keeps this mint’s tokens for ${visitSpan(periodSecs)}, so it suits services more than people.`;

/** Whether a mint rotates so often that a visit keeps its tokens only a few hours: under four, where `comeBackAt` treats it as short. */
export const rotatesQuickly = (periodSecs: number): boolean => periodSecs < 14_400;

// ---------- Recent ----------

/** What Recent says of an entry of the purse's log (records.ts `LogEntry`, which needs IndexedDB to load). */
export interface RecentEntry {
  kind: 'faucet' | 'faded' | 'kept' | 'lost';
  amount: number;
}

/** A log entry's title in Recent. */
export function recentTitle(entry: RecentEntry): string {
  switch (entry.kind) {
    case 'faucet':
      return `Took ${entry.amount} from the faucet`;
    case 'faded':
      return `Faded ${entry.amount}`;
    case 'kept':
      return `Kept ${entry.amount} alive`;
    case 'lost':
      return `Lost ${entry.amount}`;
  }
}
