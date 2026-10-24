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
  | 'open';

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
 * it; from Holdings, which reads the mint again each minute, wait.
 */
const noAnswer = (host: string, doing: Doing): string =>
  doing === 'open'
    ? `The mint at ${host} did not answer. The purse asks again in a minute.`
    : `The mint at ${host} did not answer. Check the link, or try again later.`;

/** Plain words for `error`, met while `doing` it. */
export function say(error: unknown, doing: Doing, context: Context): Said {
  const { host } = context;
  const fields = typeof error === 'object' && error !== null ? (error as Record<string, unknown>) : {};
  const name = typeof fields['name'] === 'string' ? fields['name'] : '';
  const code = typeof fields['code'] === 'string' ? fields['code'] : '';
  const status = typeof fields['status'] === 'number' ? fields['status'] : 0;
  const retryAfterMs = typeof fields['retryAfterMs'] === 'number' ? fields['retryAfterMs'] : undefined;

  if (name === 'WorglError') {
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
        return { words: 'This faucet hands out notes its mint cannot make. Nothing was kept.', noFaucet: true };
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

/** For what nothing above names: never a raw message. */
function fallback(doing: Doing): string {
  return doing === 'take' ? 'The faucet did not answer. Try again in a moment.' : 'Something went wrong. Try again in a moment.';
}

/** An error's name, code and status, as the SDK and the browser set them; empty where it has none. */
export function fieldsOf(error: unknown): { name: string; code: string; status: number } {
  const fields = typeof error === 'object' && error !== null ? (error as Record<string, unknown>) : {};
  return {
    name: typeof fields['name'] === 'string' ? fields['name'] : '',
    code: typeof fields['code'] === 'string' ? fields['code'] : '',
    status: typeof fields['status'] === 'number' ? fields['status'] : 0,
  };
}

/**
 * Whether trying again later may succeed, as the SDK's `isTransient` tells
 * it: a request that never completed, or a mint that was paused, busy or
 * could not store the change. `layer_full` is refused every time until the
 * note's own layer goes.
 */
export function transient(error: unknown): boolean {
  const { name, code, status } = fieldsOf(error);
  if (name === 'MintError') {
    return code !== 'layer_full' && (['paused', 'busy', 'storage', 'internal'].includes(code) || status >= 500);
  }
  return name === 'TypeError' || (typeof DOMException === 'function' && error instanceof DOMException);
}

/**
 * What went wrong with the store, if it was the store: another tab holds it
 * in an older version (`blocked`), another change saved first (`changed`),
 * or the browser refused to save (`refused`).
 */
export function storeKind(error: unknown): 'blocked' | 'changed' | 'refused' | undefined {
  const { name, code } = fieldsOf(error);
  if (name === 'WorglError' && code === 'store_blocked') {
    return 'blocked';
  }
  if (name === 'WorglError' && code === 'store_changed') {
    return 'changed';
  }
  if (['QuotaExceededError', 'UnknownError', 'InvalidStateError', 'AbortError', 'VersionError'].includes(name)) {
    return 'refused';
  }
  return undefined;
}
