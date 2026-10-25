// What Receive and Send say, and what Holdings says of them: every line of
// the two flows that depends on a number or a state, as pure functions of
// it, so each can be tested. Words with a part in italics or bold come as
// segments, which the views build into elements. The one-word labels sit
// beside their views; the errors are in errors.ts.

import type { AmountProblem } from './amount.ts';
import { cap, hm, listWords, notesShort, notesWords, plusJoin, shortDay, when, word } from './format.ts';
import type { LinkKind, LinkProblem } from './links.ts';
import type { ChangeLands, ShortCase } from './plan.ts';
import type { AskRecord, LogEntry } from './entries.ts';
import { recentChange, recentTitle } from './words.ts';

/** A piece of a line: words, words in italics, or words in bold. */
export type Rich = string | { em: string } | { b: string };

/** A line as plain text, as a screen reader or a test reads it. */
export const plainText = (parts: readonly Rich[]): string =>
  parts.map((part) => (typeof part === 'string' ? part : 'em' in part ? part.em : part.b)).join('');

const are = (n: number): string => (n === 1 ? 'is' : 'are');
const rests = (n: number): string => (n === 1 ? 'rests' : 'rest');

// ---------- Holdings: Recent ----------

/** What a row of Recent opens on Send, when it opens anything. */
export type RowOpens = 'sending' | 'waiting' | 'handback' | 'lost';

/** A row of Recent: its title, a small tail, the change it made, and what it opens. */
export interface RecentRow {
  title: string;
  small: string | undefined;
  /** The change, or `undefined` for none (drawn as —). */
  delta: number | undefined;
  opens: RowOpens | undefined;
  /** For a row that opens something: what it is and what it opens, for assistive tech. */
  label: string | undefined;
}

/**
 * A log entry as a row of Recent. `kept` says whether the send's hand-back
 * is still kept, so that a row whose hand-back is gone opens nothing.
 */
export function recentRow(entry: LogEntry, kept: boolean, now: number): RecentRow {
  const plain = (title: string, delta: number | undefined): RecentRow => ({ title, small: undefined, delta, opens: undefined, label: undefined });
  if (entry.kind !== 'sent' && entry.kind !== 'received') {
    // a take from the faucet, a fade and keep-alive's entries: words.ts says them
    const own = { kind: entry.kind, amount: entry.amount };
    return plain(recentTitle(own), recentChange(own));
  }
  if (entry.kind === 'received') {
    return plain(`Received ${entry.amount}`, entry.amount);
  }
  const asked = entry.asked ?? entry.amount;
  const went = entry.went;
  const at = when(entry.t, now);
  switch (entry.state) {
    case 'sending':
      return { title: `Sending ${asked}`, small: undefined, delta: -asked, opens: 'sending', label: `Sending ${asked}. Show the send` };
    case 'waiting':
      return { title: `Sent ${asked}`, small: ' · waiting', delta: -asked, opens: 'waiting', label: `Sent ${asked} at ${at}. Waiting for the mint` };
    case 'turned':
      return plain(`Turned away: ${asked}`, undefined);
    case 'lost': {
      const lost = entry.lost ?? asked;
      return {
        title: `Lost ${lost} on a send of ${asked}`,
        small: undefined,
        delta: -lost,
        opens: 'lost',
        label: `Lost ${lost} on a send of ${asked}. Show what happened`,
      };
    }
    case 'partial': {
      const title = went === undefined ? `Sent part of ${asked}` : `Sent ${went} of ${asked}`;
      if (!kept) {
        // as any send that went, once its hand-back is gone; "Sent {asked}" would say more went than did
        return went === undefined ? plain(title, undefined) : plain(`Sent ${went}`, -went);
      }
      return {
        title,
        small: ' · hand-back',
        delta: went === undefined ? undefined : -went,
        opens: 'handback',
        label: `${title} at ${at}. Show the hand-back again`,
      };
    }
    case 'handback': {
      const sent = went ?? asked;
      if (!kept) {
        return plain(`Sent ${sent}`, -sent);
      }
      return {
        title: `Sent ${sent}`,
        small: ' · hand-back',
        delta: -sent,
        opens: 'handback',
        label: `Sent ${sent} at ${at}. Show the hand-back again`,
      };
    }
    default:
      return plain(`Sent ${went ?? asked}`, -(went ?? asked));
  }
}

// ---------- Receive: ask ----------

/** The amount's unit beside the field: `token` for 1. */
export const unitWords = (amount: number | undefined): string => (amount === 1 ? 'token' : 'tokens');

/** What is wrong with a typed amount. */
export const amountWords = (problem: AmountProblem): string =>
  problem === 'too_big' ? 'Ask for 4096 or less at a time.' : 'Type a whole number of tokens, 1 or more.';

/** The hint under the amount: how it comes, as notes. */
export const askHint = (amount: number, notes: readonly number[]): string =>
  `${amount} comes as ${notesWords(notes)}, ${notes.length === 1 ? 'to a fresh key' : 'each to a fresh key'}.`;

/** "Ask in smaller notes": the toggle, the field's label and its hint. */
export const SMALLER = {
  toggle: 'Ask in smaller notes',
  label: 'In notes of at most',
  hint: 'For a sender whose ready notes are small. More notes make a longer request.',
} as const;

/** The caption under Ask's slice: where what is taken in lands. */
export function recvCap(amount: number | undefined, notes: readonly number[], at: string): Rich[] {
  if (amount === undefined || notes.length === 0) {
    return ['What you take in usually rests in the last layer until the next rotation.'];
  }
  const as =
    notes.length > 4 ? ` as ${notes.length} notes, the first four shown,` : notes.length > 1 ? ` as ${plusJoin(notes)}` : '';
  return [`Taken in, the ${amount} lands here${as} and usually rests until `, { b: at }, '.'];
}

/** How a request goes, in three steps, verbatim from the prototype. */
export const HOW_IT_GOES: readonly { n: string; b: string; rest: string }[] = [
  { n: 'I', b: 'You ask.', rest: 'This purse makes a request with a fresh key for each note.' },
  { n: 'II', b: 'They send.', rest: 'They scan it, or open its link, in their purse.' },
  { n: 'III', b: 'You take.', rest: 'They give you a hand-back. Open it here and this purse takes the tokens in.' },
];

/** An open request in the list on Ask: its words, the cap's tail, and what the row says to assistive tech. */
export function askRowWords(ask: AskRecord, now: number): { title: string; small: string | undefined; label: string } {
  return {
    title: `Ask for ${ask.amount}`,
    small: ask.atMost === undefined ? undefined : ` · in notes of at most ${ask.atMost}`,
    label: `Your request for ${ask.amount}, made ${when(ask.t, now)}. Show it`,
  };
}

// ---------- Receive: request ----------

/** When and how a request was made. */
export const madeWords = (t: number, notes: readonly number[]): string =>
  `Made at ${hm(t)} as ${notes.length === 1 ? 'one note, to a fresh key' : `${notesShort(notes)}, each to its own key`}. Once they send, they give you a hand-back.`;

/** The request's stamp label. */
export const askStampLabel = (amount: number, mint: string): string => `Ask · ${amount} tokens · ${mint}`;

/** The request's code, for assistive tech. */
export const askQrLabel = (amount: number): string => `QR code: a request for ${amount} tokens`;

/** The copy and share buttons' words. */
export const COPY = {
  ask: 'Copy the request link',
  shareAsk: 'Share the request link',
  copied: 'Copied',
  selected: 'Selected',
  selectedLabel: 'Selected · copy it',
} as const;

/** In the stamp's place when the request's link could not be written here: it stays open and listed. */
export const ASK_UNWRITTEN = 'This purse could not write this request’s link here. Reload the page, then show it again from Your open requests.';

/** The share sheet's title for a request. */
export const askShareTitle = (amount: number): string => `A request for ${amount} tokens`;

/** The question before a request is cancelled. */
export const cancelQuestion = (amount: number): string =>
  `Cancel the request for ${amount}? If they send to it after this, those tokens are lost: no purse can take them in.`;

/** What the live region says once a request is cancelled. */
export const cancelledWords = (amount: number): string => `The request for ${amount} is cancelled.`;

// ---------- Receive: take ----------

/** The take step's label. */
export const handBackLabel = (amount: number, mint: string): string => `Hand-back · ${amount} tokens · ${mint}`;

/** The take step's heading while it takes. */
export const takingHeading = (amount: number): Rich[] => [`Taking ${amount} `, { em: 'in' }, '.'];

/** The take step's heading while its link is read, before it says what it carries. */
export const TAKE_READING: Rich[] = ['Reading the ', { em: 'hand-back' }, '.'];

/** The take step's words while it takes. */
export const TAKING_BODY = 'This purse checks each note against the mint’s keys, then keeps them.';

/** How a take ended, when it took nothing. */
export type TakeEnd = 'failed' | 'twice' | 'not-ours' | 'cancelled' | 'unheld';

/** The take step's heading and words once it ended with nothing taken. */
export function takeEndWords(
  end: TakeEnd,
  o: { amount: number; at?: number | undefined; now: number; host: string; words?: string | undefined },
): { h1: Rich[]; body: string } {
  const nothing: Rich[] = ['Nothing was ', { em: 'taken in' }, '.'];
  switch (end) {
    case 'twice':
      return {
        h1: ['Taken in ', { em: 'already' }, '.'],
        body: o.at === undefined ? 'This purse took these tokens in already.' : `This purse took these ${o.amount} in at ${when(o.at, o.now)}.`,
      };
    case 'not-ours':
      return {
        h1: ['Not for this ', { em: 'purse' }, '.'],
        body: 'This hand-back carries tokens for the keys of someone else’s request, so this purse cannot take them in. Open it in the purse that asked.',
      };
    case 'cancelled':
      return {
        h1: nothing,
        body: `You cancelled this request${o.at === undefined ? '' : ` at ${when(o.at, o.now)}`}, so this purse no longer holds its keys and cannot take these ${o.amount} in.`,
      };
    case 'unheld':
      return {
        h1: ['Not for this ', { em: 'purse' }, '.'],
        body: `This hand-back is for a mint at ${o.host} that this purse does not hold. Open it in the purse that asked.`,
      };
    default:
      return { h1: nothing, body: o.words ?? '' };
  }
}

/** What the live region says once a hand-back is taken in: where it landed. */
export function receivedWords(amount: number, ready: number, resting: number, notes: number, at: string): string {
  if (ready > 0 && resting > 0) {
    return `Received ${amount}: ${ready} ready now, ${resting} resting until ${at}.`;
  }
  const one = notes === 1;
  if (ready > 0) {
    return `Received ${amount}. ${one ? 'It is' : 'They are'} ready to send now.`;
  }
  return `Received ${amount}. ${one ? 'It rests' : 'They rest'} until ${at}, then ${one ? 'it is' : 'they are'} ready to send.`;
}

/** What the live region adds when a hand-back answered another open request than the one shown. */
export const otherAskWords = (amount: number): string => `This hand-back was for your request for ${amount}. It is taken in.`;

// ---------- Send: scan ----------

/** The scan step's heading. */
export const SCAN_HEADING: Rich[] = ['Scan their ', { em: 'request' }, '.'];

/** The scan step's words. */
export const SCAN_BODY = 'Ask them to open Receive and show you their code.';

/** The caption under the scan step's slice: what can be sent now. */
export function scanCap(ready: number, resting: number, at: string, faucet: number | undefined): Rich[] {
  if (ready === 0 && resting === 0) {
    return [
      faucet === undefined
        ? 'This purse is empty. Ask someone to send you some first.'
        : `This purse is empty. Take ${faucet} from the faucet on Holdings first.`,
    ];
  }
  if (ready === 0) {
    return [`Nothing is ready to send until ${at}, when `, { b: String(resting) }, ` ${are(resting)}.`];
  }
  return [{ b: String(ready) }, resting > 0 ? ` ready to send now · ${resting} more at ${at}` : ' ready to send now'];
}

/** A kept hand-back in the list on Send: its tail and what it is, for assistive tech. */
export const keptWords = (amount: number, t: number | undefined, now: number): { lab: string; label: string } => ({
  lab: t === undefined ? 'tokens' : `tokens · ${shortDay(t, now)}`,
  label: t === undefined ? `The hand-back for ${amount} tokens` : `The hand-back for ${amount} tokens, sent ${when(t, now)}`,
});

/** The reading step. */
export const READING_HEADING: Rich[] = ['Checking what can ', { em: 'go' }, '.'];
export const READING_BODY = 'This purse reads the mint’s info to see which notes can go.';

/** The ticket stub: how the request came, and when; its amount; its code's label. */
export const ticketWords = (via: 'camera' | 'link', at: number, amount: number): { lab: string; b: string; qr: string } => ({
  lab: `${via === 'camera' ? 'Scanned' : 'Opened'} request · ${hm(at)}`,
  b: `for ${amount} tokens`,
  qr: via === 'camera' ? 'The scanned request' : 'The opened request',
});

// ---------- Send: unknown, sent already, own ----------

export const UNKNOWN_HEADING: Rich[] = ['A mint this purse does not ', { em: 'hold' }, '.'];
export const unknownBody = (amount: number, host: string): string =>
  `This request asks for ${amount} tokens of a mint at ${host}, which this purse does not hold. Add that mint, then send.`;

/** When a request names another server for a mint this purse holds. */
export const serverNote = (named: string, held: string): string =>
  `This request names ${named} for this mint. This purse reaches it at ${held}, and sends there.`;

export const ALREADY_HEADING: Rich[] = ['You sent to this request ', { em: 'already' }, '.'];
export const TURNED_REQUEST_HEADING: Rich[] = ['This request was turned ', { em: 'away' }, '.'];

/** Why a request was sent to already: kept with its hand-back, still on its way, turned away, or no longer kept. */
export function alreadyBody(kind: 'kept' | 'outgoing' | 'turned' | 'gone', amount: number, t: number | undefined, now: number): string {
  const at = t === undefined ? undefined : when(t, now);
  if (kind === 'turned') {
    return `${at === undefined ? 'Before' : `At ${at}`}, this purse sent to it and the mint turned the send away: nothing went, and your note is back with you. A request takes one send, so ask them for a new one.`;
  }
  if (kind === 'kept') {
    return `${at === undefined ? 'Before' : `At ${at}`}, ${amount} went to it. If they did not get the hand-back, show it again.`;
  }
  if (kind === 'outgoing') {
    return `${at === undefined ? 'Before' : `At ${at}`}, this purse sent ${amount} to it, and the mint has not answered yet. The hand-back comes once it does.`;
  }
  return 'This purse sent to it before, and sends to a request once. If they did not get their tokens, ask them for a new request.';
}

export const OWN_HEADING: Rich[] = ['This is your own ', { em: 'request' }, '.'];
export const OWN_BODY = 'This purse made it. Show it to the person who will send to you.';

// ---------- Send: not enough ready ----------

/** One way out of "not enough ready": its line and its small print; `faucet` draws the faucet's button under it. */
export interface WayOut {
  text: string;
  small: string;
  faucet?: boolean;
}

/** Everything a "not enough ready" view says. */
export interface ShortWords {
  h1: Rich[];
  body: Rich[];
  options: WayOut[];
  /** The message to copy, without its quotes. */
  message: string;
  /** The slice's tag, and the tokens it is short (0 for the cases about note sizes). */
  tag: string;
  need: number;
}

/** What the short words need besides the case. */
export interface ShortInput {
  /** The request's amount and its notes. */
  amount: number;
  outputs: readonly number[];
  /** Ready now (fading included) and resting now. */
  ready: number;
  resting: number;
  /** When the next rotation comes, in words. */
  at: string;
  /** The faucet at the mint's server: its tap, the mint's name, and whether its notes land ready. */
  faucet: { amount: number; mint: string; ready: boolean } | undefined;
}

/** The faucet's way out, or receiving more where the mint has none. */
function faucetWay(input: ShortInput): WayOut {
  const f = input.faucet;
  if (f === undefined) {
    return { text: 'Receive more first.', small: 'Ask someone to send you some, then send this.' };
  }
  return {
    text: `Take ${f.amount} from the ${f.mint} faucet, or receive more first.`,
    small: f.ready ? 'Faucet notes are ready at once.' : `Faucet notes rest until ${input.at}${input.resting > 0 ? ' too' : ''}.`,
    faucet: true,
  };
}

/** What a "not enough ready" view says for its case. */
export function shortWords(c: ShortCase, input: ShortInput): ShortWords {
  const { amount: A, ready, resting, at } = input;
  const maxOut = Math.max(0, ...input.outputs);
  const wait = (whenReady: string): WayOut => ({ text: `Wait until ${at}, when ${whenReady}.`, small: 'Then this request can go as it is.' });
  if (c.kind === 'over' && c.has === 0) {
    return {
      h1: ['This purse is ', { em: 'empty' }, '.'],
      body: ['The request is for ', { b: String(A) }, '.'],
      options: [faucetWay(input)],
      message: `My purse is empty right now. I’ll send to your request for ${A} once it holds enough.`,
      tag: `${A} short`,
      need: A,
    };
  }
  if (c.kind === 'over') {
    const has = c.has;
    const tail = resting > 0 ? (ready > 0 ? ` ${ready} ${are(ready)} ready now and ${resting} ${rests(resting)} until ${at}.` : ` All of it rests until ${at}.`) : '';
    const small = ready >= has ? 'This purse can send that now.' : ready > 0 ? `It can go after ${at}, when all ${has} ${are(has)} ready.` : `It can go after ${at}.`;
    const why = ready >= has ? '' : ready > 0 ? `, and only ${ready} ${are(ready)} ready before ${at}` : `, and it is ready after ${at}`;
    return {
      h1: ['More than this purse ', { em: 'holds' }, '.'],
      body: ['This purse holds ', { b: String(has) }, '; the request is for ', { b: String(A) }, `.${tail}`],
      options: [{ text: `Ask them for ${has} instead.`, small }, faucetWay(input)],
      message: `I have ${has}, not ${A}${why}. Could you make a request for ${has} instead?`,
      tag: `${A - has} short`,
      need: A - has,
    };
  }
  if (c.kind === 'none') {
    const options = [wait(`${resting} ${are(resting)} ready`)];
    if (input.faucet?.ready === true) {
      options.push(faucetWay(input));
    }
    return {
      h1: ['Nothing is ', { em: 'ready' }, ` until ${at}.`],
      body: ['This purse holds ', { b: String(c.has) }, `, all resting until ${at}. Then this request can go.`],
      options,
      message: `I can send it after ${at}. I’ll send to this same request then.`,
      tag: `${A - ready} short now`,
      need: A - ready,
    };
  }
  if (c.kind === 'later') {
    const rest = A - ready;
    const options: WayOut[] = [
      { text: `Ask them for ${ready} now and ${rest} after ${at}.`, small: 'Two requests, each sent when it can go.' },
      wait(`${ready + resting} ${are(ready + resting)} ready`),
    ];
    if (input.faucet?.ready === true) {
      options.push(faucetWay(input));
    }
    return {
      h1: ['More than is ', { em: 'ready' }, ' now.'],
      body: [{ b: String(ready) }, ` ${are(ready)} ready now and `, { b: String(resting) }, ` more at ${at}.`],
      options,
      message: `I can send ${ready} now and ${rest} after ${at}. Could you make two requests, one for ${ready} and one for ${rest}?`,
      tag: `${rest} short now`,
      need: rest,
    };
  }
  if (c.kind === 'notes') {
    const m = c.atMost;
    const options: WayOut[] = [
      m === undefined
        ? { text: 'Ask them for less, in two requests.', small: `The notes here cannot make ${A} in one request.` }
        : { text: `Ask them to ask again in notes of at most ${m}.`, small: 'On Receive, under Ask in smaller notes.' },
    ];
    if (c.waitHelps) {
      options.push(wait('a larger note is ready'));
    }
    return {
      h1: ['No ready note is big ', { em: 'enough' }, '.'],
      body: [`The request asks for ${notesShort(input.outputs)}; the largest note ready here is ${c.largest}.`],
      options,
      message:
        m === undefined
          ? `I can’t send ${A} in one go from the notes I have. Could you make two smaller requests?`
          : `I can’t send a note of ${c.maxOut} right now. Could you ask again for ${A}, in notes of at most ${m}?`,
      tag: `Needs a note of ${c.maxOut}`,
      need: 0,
    };
  }
  return {
    h1: ['Too many small notes for one ', { em: 'transfer' }, '.'],
    body: [`One transfer here makes at most ${c.maxOutputs} notes, and this request needs more, with the change.`],
    options: [{ text: 'Ask them to ask again in larger notes.', small: 'A new request, without Ask in smaller notes.' }],
    message: `I can’t send to a request in so many small notes. Could you ask again for ${A} in larger notes?`,
    tag: `Needs a note of ${maxOut}`,
    need: 0,
  };
}

/** The faucet's button on "not enough ready". */
export const faucetButton = (amount: number, taking: boolean): string =>
  taking ? `Taking ${amount}…` : `Take ${amount} from the faucet`;

// ---------- Send: confirm ----------

/** Confirm's heading. */
export const confirmHeading = (amount: number): Rich[] => [`Send ${amount} to this `, { em: 'request' }, '.'];

/** How long a send of `n` notes takes, before `pay` says: the mean gap of the spacing, plus about a second for the answers. */
export const estimateSecs = (n: number): number => Math.round(1 + (n - 1) * 4);

/** The split's labels. */
export const notesLabel = (n: number): string => (n === 1 ? 'One note' : `${cap(word(n))} notes`);

/** What comes back, and where it lands. */
export function explainWords(n: number, back: readonly number[], lands: ChangeLands | undefined, at: string): string {
  const total = back.reduce((a, b) => a + b, 0);
  if (n === 1) {
    if (total === 0) {
      return 'Nothing comes back: the note goes whole.';
    }
    return `The ${total} comes back as ${notesWords(back)}, ${lands === 'ready' ? 'ready to send' : `resting until ${at}`}.`;
  }
  const lead = `${cap(word(n))} transfers, one per note, a few seconds apart.`;
  if (total === 0) {
    return `${lead} Nothing comes back.`;
  }
  const where =
    lands === 'ready' ? 'ready to send' : lands === 'mixed' ? `part ready and part resting until ${at}` : `resting until ${at}`;
  return `${lead} ${total} comes back, ${where}.`;
}

/** What stays ready after the send. */
export function afterWords(readyAfter: number, laterAfter: number, at: string): string {
  if (readyAfter > 0 && laterAfter > 0) {
    return `After this, ${readyAfter} ${are(readyAfter)} ready now and ${laterAfter} more at ${at}.`;
  }
  if (readyAfter > 0) {
    return `After this, ${readyAfter} ${are(readyAfter)} ready.`;
  }
  if (laterAfter > 0) {
    return `After this, nothing is ready until ${at}, when ${laterAfter} ${are(laterAfter)}.`;
  }
  return 'After this, the purse is empty.';
}

/** The line under Confirm's buttons when there is no room for the large time. */
export const durationLine = (n: number): string =>
  n === 1 ? 'One transfer, at once. Keep this open while it goes.' : `Keep this open about ${estimateSecs(n)} s while it goes.`;

export const CHANGED = 'What is ready changed since you opened this. Check it again.';

// ---------- Send: sending ----------

export const SENDING_LAB = 'Sending · keep this open';
export const sendingHeading = (amount: number): Rich[] => [`Sending ${amount} to the `, { em: 'mint' }, '.'];

/** The first stage: the notes chosen. */
export const chosenWords = (values: readonly number[]): string =>
  values.length === 1 ? `One note of ${values[0]} chosen` : `${cap(word(values.length))} notes chosen: ${listWords(values)}`;

/** The second stage: at the mint, `k` of `n`, and its small line. */
export function atMintWords(
  n: number,
  k: number,
  state: { done: boolean; slow: boolean },
  amount: number,
  back: number,
): { line: string; small: string } {
  const line = n > 1 ? `At the mint · ${Math.max(1, k)} of ${n}` : 'At the mint';
  let small: string;
  if (state.done) {
    small = back > 0 ? `Answered: ${amount} for them, ${back} back to you.` : `Answered: ${amount} for them.`;
  } else if (state.slow) {
    small = 'Waiting for the mint’s answer.';
  } else {
    small = n > 1 ? 'One transfer per note, a few seconds apart.' : 'One transfer.';
  }
  return { line, small };
}

export const STAGE_CODE = 'A code to give the receiver';

/** "Send the rest now", and what it costs. */
export const SEND_NOW = { text: 'Send the rest now', small: 'Quicker, but the mint may then tell these notes were one person’s.' } as const;

/** The slice's tag over what went. */
export const outTag = (out: number, amount: number): string => (out < amount ? `${out} of ${amount} to them` : `${amount} to them`);
export const backTag = (back: number): string => `${back} back`;

/** What the live region says as each transfer of several is answered. */
export const progressWords = (k: number, n: number): string => `${k} of ${n} at the mint.`;

/** What the live region says when a send ends: here, or while another screen showed. */
export const sentWords = (amount: number, away: boolean): string =>
  away ? `Sent ${amount}. The hand-back is waiting on Send.` : 'Sent. Give the hand-back to the receiver.';

/** What the live region says when a send kept for later went through on a later open. */
export const settledWords = (amount: number): string => `The send of ${amount} went through. Its hand-back is on Send.`;

// ---------- Send: hand-back ----------

export const sentLine = (amount: number, t: number, now: number): string => `Sent ${amount} · ${when(t, now)}`;

/** The label over a send that was turned away or lost, where "Sent" would say more went than did. */
export const sendOfLine = (amount: number, t: number, now: number): string => `Send of ${amount} · ${when(t, now)}`;
export const HANDBACK_HEADING: Rich[] = ['Give this to the ', { em: 'receiver' }, '.'];
export const carriesWords = (amount: number): string => `It carries their ${amount}. When they open it, their purse takes them in.`;

/** The change line: what came back and where it is now, when that is known. */
export function backLine(back: number, lands: ChangeLands | undefined, readyAt: number | undefined, now: number): { b: string; muted: string } {
  let tail: string;
  if (lands === 'ready') {
    tail = ' · ready to send';
  } else if (lands === undefined || readyAt === undefined) {
    // where it landed, or when it can move, is not known: no claim
    tail = '';
  } else {
    tail = ` · ${now < readyAt ? 'resting until' : 'rested until'} ${hm(readyAt)}`;
  }
  return { b: `${back} came back to you`, muted: tail };
}

/** The change line when nothing came back. */
export const nothingBack = (notes: number): string =>
  notes === 1 ? 'Nothing came back: the note went whole.' : 'Nothing came back: the notes went whole.';

/** The line of a send part of which the mint turned away. */
export function partialLine(went: number | undefined, amount: number, turned: number): { b: string; muted: string } {
  return {
    b: went === undefined ? `Part of ${amount} went.` : `${went} of ${amount} went.`,
    muted:
      turned === 1
        ? ' The mint turned one transfer away, and its note is back with you.'
        : ` The mint turned ${turned} transfers away, and their notes are back with you.`,
  };
}

/** The body of a partial hand-back. */
export const partialBody = (went: number | undefined, amount: number): string =>
  went === undefined
    ? `It carries part of their ${amount}. Their purse says how much when they take it in.`
    : `It carries their ${went}. Ask them to make a new request for the other ${amount - went}.`;

export const handBackQrLabel = (amount: number): string => `QR code: a hand-back carrying ${amount} tokens`;
export const handBackStamp = (amount: number, mint: string): string => `${amount} tokens · ${mint} · for one receiver`;
export const HANDBACK = {
  copy: 'Copy link',
  share: 'Share the hand-back link',
  again: 'You can show it again from Send until their tokens fade.',
  unwritten: 'This purse keeps their notes, but could not write the hand-back here. Show it again from Send, or reload the page first.',
} as const;
export const handBackShareTitle = (amount: number): string => `${amount} tokens for you`;

// ---------- Send: waiting, turned away, lost ----------

export const WAITING_HEADING: Rich[] = ['The mint has not ', { em: 'answered' }, ' yet.'];
export const WAITING_BODY = 'Nothing is lost. This purse sends it again when it can, and the hand-back comes then.';
export const TURNED_HEADING: Rich[] = ['The mint turned this send ', { em: 'away' }, '.'];
export const turnedBody = (layerFull: boolean, at: string): string =>
  layerFull
    ? `Its newest layer is full, so your note is back with you and nothing went. After its next rotation, at ${at}, ask them for a new request.`
    : 'Its rules refuse this send now, so your note is back with you and nothing went.';
export const LOST_HEADING: Rich[] = ['This send could not be ', { em: 'finished' }, '.'];
export const lostBody = (amount: number, lost: number): string =>
  `The mint says the note this send spent is gone, and it no longer keeps the answer. The ${amount} for them and your change are lost: ${lost} in all.`;

// ---------- links ----------

/**
 * What is wrong with a link pasted, opened or scanned where `want` was
 * looked for; `kind` is what the text named, if anything.
 */
export function linkProblemWords(want: LinkKind, problem: LinkProblem | 'request' | 'kind', kind?: LinkKind): string {
  if (problem === 'too_long') {
    return 'This link is longer than any request or hand-back. Check that you copied only the link.';
  }
  if (problem === 'newer') {
    return 'This link comes from a newer purse. Reload this page to update it, then open the link again.';
  }
  const other = problem === 'kind' || (kind !== undefined && kind !== want);
  if (want === 'take') {
    if (other) {
      return 'That is a request, not a hand-back. Their hand-back has #take= in it.';
    }
    switch (problem) {
      case 'empty':
        return 'Paste their hand-back link first.';
      case 'mint_link':
        return 'That is a mint’s link, not a hand-back. Their hand-back has #take= in it.';
      case 'server':
      case 'request':
        return 'This hand-back cannot be read. Ask them to show it again.';
      default:
        return 'This does not read as a hand-back. Check that you copied the whole link.';
    }
  }
  if (other) {
    return 'That is a hand-back, for taking tokens in. To send, open their request: a link with #ask= in it.';
  }
  switch (problem) {
    case 'empty':
      return 'Paste their request link first.';
    case 'mint_link':
      return 'That is a mint’s link, not a request. To send, open their request: a link with #ask= in it.';
    case 'server':
      return 'This request names a server this purse cannot reach: it reaches mints only over https.';
    case 'request':
      return 'This request cannot be read. Ask them to show it again.';
    default:
      return 'This does not read as a request. Check that you copied the whole link.';
  }
}

// ---------- QR codes ----------

export const QR = {
  denseAsk: 'A large request makes a dense code. If it does not scan, send the link.',
  denseTake: 'A large hand-back makes a dense code. If it does not scan, send the link.',
  none: 'Too much for one code. Send the link instead.',
  failed: 'The code could not be drawn here. The link below carries the same thing.',
} as const;

// ---------- camera ----------

/** Where the camera is. */
export type Camera =
  | { kind: 'idle' }
  | { kind: 'starting' }
  | { kind: 'running' }
  | { kind: 'refused'; name: string }
  | { kind: 'unsupported' }
  | { kind: 'no-reader' };

/** What the scanner says for the camera's state: nothing before it starts. */
export function cameraWords(state: Camera): string {
  switch (state.kind) {
    case 'running':
      return 'Point the camera at their code.';
    case 'refused':
      switch (state.name) {
        case 'NotAllowedError':
          return 'The camera is off for this purse. Allow it in the browser’s site settings, or paste their link instead.';
        case 'NotFoundError':
        case 'OverconstrainedError':
          return 'This device has no camera this purse can use. Paste their link instead.';
        case 'NotReadableError':
        case 'AbortError':
          return 'Another app is using the camera. Close it and try again, or paste their link.';
        default:
          return 'This browser offers no camera here. Paste their link instead.';
      }
    case 'unsupported':
      return 'This browser offers no camera here. Paste their link instead.';
    case 'no-reader':
      return 'The code reader did not load. Paste their link instead.';
    default:
      return '';
  }
}

/** The camera's buttons. */
export const CAMERA = {
  on: 'Turn on the camera',
  again: 'Try the camera again',
  off: 'Turn off the camera',
  orBelow: 'Or paste their link below.',
  or: 'Or paste their link.',
} as const;

/** What the copy button says once it copied, or selected the text for the person to copy. */
export const copiedWords = (copied: boolean): string => (copied ? 'Copied' : 'Selected · copy it');
