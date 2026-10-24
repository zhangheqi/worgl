// Every error the SDK, the mint, the faucet or the browser can give, in
// plain words. The purse never shows an error's own message: it is for
// developers, and it may carry whatever a server sent.
//
// Told apart as the SDK documents them, in this order: a WorglError by its
// code, a MintError by its status and code, the purse's own FaucetChanged
// (see purses.ts), a DOMException by its name, and a TypeError, which fetch
// gives when a request never completed (offline, CORS, or a policy that
// blocks it). Read by name rather than by class, so this module needs
// nothing from the SDK at run time.

import { when } from './format.ts';
import type { LinkProblem } from './link.ts';

/** What the purse was doing when it failed, which decides the words. */
export type Doing =
  /** Reading a mint's info and keys, to show it before adding it. */
  | 'read'
  /** Taking a note from the faucet. */
  | 'take'
  /** Opening the purse, or reading what it holds. */
  | 'open'
  /** Making a request (Receive). */
  | 'ask'
  /** Reading what can go for a request, or paying it (Send). */
  | 'send'
  /** Taking in a hand-back. */
  | 'accept'
  /** Sending again what a send kept for later. */
  | 'settle';

/** What the words may name. */
export interface Context {
  /** The server's host. */
  host: string;
  /** The faucet's taps an hour for one address, if known. */
  perHour?: number | undefined;
  /** The time now, for when to try again; `Date.now()` if not given. */
  now?: number;
}

/** How the purse should go on after an error. */
export interface Said {
  /** The plain words. */
  words: string;
  /** Hide the faucet's Take button: this faucet's notes cannot be taken here. */
  noFaucet?: boolean;
  /** Read the store again (open a new SDK purse) before the next try. */
  reopen?: boolean;
}

/** The words for a link that does not read. */
export function linkWords(problem: LinkProblem): string {
  if (problem === 'insecure') {
    return 'This purse reaches mints only over https. Ask for the link that starts with https://';
  }
  return 'Paste the whole mint link, starting with https://';
}

/** The words when the server answers, but not with the mint the link names. */
export const notListed = (host: string): string => `The server at ${host} does not list the mint this link names.`;

/**
 * The words when the mint's server cannot be reached: from a link, check
 * it; from Holdings, which reads the mint again each minute, wait; for a
 * request, a send, a take or a resend, what it means for that.
 */
function noAnswer(host: string, doing: Doing): string {
  const first = `The mint at ${host} did not answer.`;
  switch (doing) {
    case 'open':
      return `${first} The purse asks again in a minute.`;
    case 'ask':
      return `${first} A request needs the mint’s answer; try again in a moment.`;
    case 'send':
      return `${first} Nothing was sent; try again in a moment.`;
    case 'accept':
      return `${first} Taking in needs its keys. The hand-back keeps until its tokens fade, so try again later.`;
    case 'settle':
      return `${first} This purse tries again the next time it opens; you can also try now.`;
    default:
      return `${first} Check the link, or try again later.`;
  }
}

/** A detail of an SDK error, as text: amounts are bigints. */
const detail = (fields: Record<string, unknown>, key: string): string => {
  const details = fields['details'];
  const value = typeof details === 'object' && details !== null ? (details as Record<string, unknown>)[key] : undefined;
  return typeof value === 'bigint' || typeof value === 'number' || typeof value === 'string' ? String(value) : '';
};

/** Plain words for `error`, met while `doing` it. */
export function say(error: unknown, doing: Doing, context: Context): Said {
  const { host } = context;
  const fields = typeof error === 'object' && error !== null ? (error as Record<string, unknown>) : {};
  const name = typeof fields['name'] === 'string' ? fields['name'] : '';
  const code = typeof fields['code'] === 'string' ? fields['code'] : '';
  const status = typeof fields['status'] === 'number' ? fields['status'] : 0;
  const retryAfterMs = typeof fields['retryAfterMs'] === 'number' ? fields['retryAfterMs'] : undefined;

  if (name === 'WorglError') {
    // what a request, a send or a take refuses, in its own words
    if (doing === 'ask' && code === 'unsplittable') {
      return { words: `This mint cannot make ${detail(fields, 'amount')} from its notes. Try another amount.` };
    }
    if (doing === 'ask' && code === 'too_many_notes') {
      return { words: `That would take more than ${detail(fields, 'max')} notes. Ask for less, or in larger notes.` };
    }
    if (doing === 'accept' && (code === 'bad_signature' || code === 'wrong_issuer' || code === 'unsigned')) {
      return { words: 'This hand-back did not check out against the mint’s keys. Nothing was taken in.' };
    }
    if (doing === 'accept' && code === 'unknown_generation') {
      return { words: 'Some of these tokens have faded: the mint has dropped their layer. Nothing was taken in.' };
    }
    if (doing === 'send' && code === 'denomination') {
      return {
        words: `This request asks for a note of ${detail(fields, 'amount')}, which this mint does not make. Ask them for a new request.`,
      };
    }
    if (doing === 'send' && code === 'policy') {
      return { words: 'This mint’s rules refuse this send as it stands. Ask them for a new request.' };
    }
    switch (code) {
      case 'store_blocked':
        return { words: 'The purse is open in another tab with an older version. Close its other tabs, then try again.' };
      case 'store_changed':
        return { words: 'Another tab changed the purse meanwhile. Nothing was kept; try again.', reopen: true };
      case 'unknown_format':
        return { words: 'A newer version of the purse saved here. Reload the page to update it.' };
      case 'malformed':
        return doing === 'open'
          ? { words: 'What this browser holds for the purse cannot be read, so the purse leaves it as it is.' }
          : { words: `The answer from ${host} is not a mint’s.` };
      case 'no_layers':
        return { words: `The mint at ${host} lists no layers, so it cannot hold tokens.` };
      case 'bad_signature':
      case 'unknown_generation':
      case 'wrong_issuer':
        return { words: 'That server’s answer did not check out. Nothing was kept.' };
      case 'faucet_amount':
      case 'unsplittable':
      case 'too_many_notes':
      case 'zero_amount':
        // from a take, the faucet's; from anything else, these never name the faucet
        return doing === 'take'
          ? { words: 'This faucet hands out notes its mint cannot make. Nothing was kept.', noFaucet: true }
          : { words: fallback(doing) };
      default:
        return { words: fallback(doing) };
    }
  }

  if (name === 'MintError') {
    if (doing === 'take' && status === 429) {
      const given =
        code === 'exhausted'
          ? 'The faucet has given out its notes for today.'
          : context.perHour === undefined
            ? 'The faucet has given all its taps this hour.'
            : `The faucet has given its ${context.perHour} ${context.perHour === 1 ? 'tap' : 'taps'} this hour.`;
      // the faucet's Retry-After says when it hands out a note again
      const now = context.now ?? Date.now();
      return { words: retryAfterMs === undefined ? given : `${given} Try again at ${when(now + retryAfterMs, now)}.` };
    }
    if (doing === 'take' && code === 'unknown_origin') {
      return { words: 'This faucet does not hand out notes to this purse.', noFaucet: true };
    }
    if (code === 'unknown_mint' || (doing === 'read' && status === 404)) {
      return { words: doing === 'open' ? `The server at ${host} no longer lists this mint.` : notListed(host) };
    }
    if (code === 'paused') {
      return { words: `The mint at ${host} is paused. Try again later.` };
    }
    if (code === 'layer_full') {
      return { words: 'The mint’s newest layer is full. Try again after its next rotation.' };
    }
    if (doing === 'take') {
      return { words: 'The faucet did not answer. Try again in a moment.' };
    }
    return { words: noAnswer(host, doing) };
  }

  if (name === 'FaucetChanged') {
    return { words: 'This faucet hands out another mint’s notes now. Nothing was taken.', noFaucet: true };
  }
  if (name === 'NotSupportedError') {
    return { words: 'This browser cannot keep tokens here. Open the purse in a current browser, over https.' };
  }
  if (name === 'QuotaExceededError' || name === 'UnknownError' || name === 'AbortError' || name === 'InvalidStateError' || name === 'VersionError') {
    return { words: 'This browser refused to save. Nothing changed.' };
  }
  if (name === 'TypeError' || name === 'TimeoutError' || name === 'NetworkError') {
    return { words: doing === 'take' ? 'The faucet did not answer. Try again in a moment.' : noAnswer(host, doing) };
  }
  return { words: fallback(doing) };
}

/** The words when the purse's records refused a request it had made, which it then withdrew. */
export const notRecorded = 'This browser refused to save the request, so it was not made.';

/** The words when the purse's records refused to mark a request cancelled: its keys are kept, and it stays open. */
export const notCancelled = 'This browser refused to save, so the request was not cancelled. It stays open.';

/** For what nothing above names: never a raw message. */
function fallback(doing: Doing): string {
  return doing === 'take' ? 'The faucet did not answer. Try again in a moment.' : 'Something went wrong. Try again in a moment.';
}
