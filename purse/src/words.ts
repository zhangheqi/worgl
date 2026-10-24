// What Holdings says: the one sentence under the total, the date to come
// back by, the lane and note tips and the field's labels for assistive
// tech, and the first run's legend. Pure: each takes the holdings and the
// time now.

import { cap, hm, listWords, notesWords, toks, when, word } from './format.ts';
import {
  type Holdings,
  type LaneKey,
  type Note,
  comeBackAt,
  laneOf,
  laneTotal,
  notesIn,
  rotation,
  sum,
  values,
} from './holdings.ts';

/**
 * Whether the purse keeps tokens alive while it is open. Not yet: until
 * keep-alive comes (the SDK's refresh, one layer for what fades at the next
 * rotation), every line that would promise it, or that a note fades only
 * if the purse stays closed, says what happens without it.
 */
export const KEEPS_ALIVE = false;

/** A piece of the sentence: words, a number that opens its lane's tip, or words in italics. */
export type Segment = string | { n: number; lane: LaneKey } | { em: string };

/** The lane's name in the field. */
export const LANE_NAME: Record<LaneKey, string> = { fade: 'Fading', ready: 'Ready', rest: 'Resting' };

/** The state's word for one note: fading, ready, resting. */
const STATE: Record<LaneKey, string> = { fade: 'fading', ready: 'ready', rest: 'resting' };

/** When the `k`th rotation from now comes, in words; a mint with no schedule has no time to name. */
function rotationWords(holdings: Holdings, k: number, now: number): string {
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

/**
 * The sentence under the total: `69 are ready; 123 rest until 16:00.
 * Nothing fades before Thu 08:00.`
 */
export function sentence(holdings: Holdings, now: number, keepsAlive = KEEPS_ALIVE): Segment[] {
  if (holdings.notes.length === 0) {
    return ['This purse is empty.'];
  }
  const ready = laneTotal(holdings, 'ready');
  const resting = laneTotal(holdings, 'rest');
  const fading = laneTotal(holdings, 'fade');
  const next = rotationWords(holdings, 0, now);
  const out: Segment[] = [];
  if (ready > 0) {
    out.push({ n: ready, lane: 'ready' }, ready === 1 ? ' is ready' : ' are ready');
  } else {
    out.push('Nothing is ready');
  }
  if (resting > 0) {
    out.push('; ', { n: resting, lane: 'rest' }, `${resting === 1 ? ' rests' : ' rest'} until ${next}`);
  }
  if (fading > 0) {
    const at = resting > 0 ? 'then' : `at ${next}`;
    if (keepsAlive) {
      out.push(
        '; ',
        { n: fading, lane: 'fade' },
        ` would fade ${at}, so this purse keeps ${fading === 1 ? 'it' : 'them'} `,
        { em: 'alive' },
        ' while it is open.',
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
 * there is none. Until keep-alive comes, opening the purse again keeps
 * nothing, so the row says when the first tokens fade instead.
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
    return `The purse keeps tokens alive only while it is open. Left closed, your ${amount} ${state} ${goes} at ${at}.`;
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
    more = ` ${they} ${notes.length === 1 ? 'fades' : 'fade'} at ${rotationWords(holdings, 0, now)} unless ${notes.length === 1 ? 'it moves' : 'they move'}${keepsAlive ? `; while this purse is open, it keeps ${notes.length === 1 ? 'it' : 'them'} alive` : ''}.`;
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
              'While this purse is open, it keeps notes alive: one transfer at a time, it moves them a layer further from the first, down the field.',
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
        `While this purse is open, it keeps ${they} alive: one transfer at a time, it moves ${they} a layer further from the first, down the field.`,
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
          ? `Ready at ${next}. If this purse stays closed, ${one ? 'it fades' : 'they fade'} ${fadesAt(holdings, notes[0] as Note, now)}.`
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
  let closed: string;
  if (one || rest.length === 0) {
    closed = `${one ? 'it fades' : 'they fade'} ${fadesAt(holdings, notes[0] as Note, now)}`;
  } else {
    const later = [...new Set(rest.map((note) => fadesAt(holdings, note, now)))];
    const restAt = later.length === 1 ? later[0] : `from ${fadesAt(holdings, rest.reduce((a, b) => (a.pos <= b.pos ? a : b)), now)}`;
    closed = `the ${listWords(values(upper))} ${upper.length === 1 ? 'fades' : 'fade'} ${fadesAt(holdings, upper[0] as Note, now)}, the rest ${restAt}`;
  }
  const split = !one && rest.length > 0;
  const lines = [
    `${cap(notesWords(vals))}. Sends use ${they}.`,
    keepsAlive
      ? `${They} can move now. If this purse stays closed, ${closed}.`
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
  if (note.source === 'faucet' && note.arrivedAt !== undefined) {
    lines.push(`From the ${mintName} faucet at ${when(note.arrivedAt, now)}.`);
  }
  if (lane === 'fade') {
    lines.push(
      `Fades at ${rotationWords(holdings, 0, now)} unless it moves.${keepsAlive ? ' While this purse is open, it keeps it alive.' : ''}`,
    );
  } else {
    const ready = note.readyAt === undefined ? rotationWords(holdings, 0, now) : when(note.readyAt, now);
    const unless = keepsAlive ? 'if left closed' : 'unless it moves';
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
        ? 'Sat still too long; kept alive while the purse is open.'
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
