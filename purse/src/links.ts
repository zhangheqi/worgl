// The two links a send passes between purses, as a link or a QR code:
//
// - `#ask=`: the receiver's request, which the sender opens to send to it.
// - `#take=`: the sender's hand-back, the receiver's notes from the mint's
//   answers, which the receiver opens to take them in.
//
// Each is a small frame of lines, `kind/version`, the server's URL, then
// what it carries, deflated and written in base64url. Requests and notes
// name no server, so the frame does. Read from any host, or as a bare
// fragment, with whitespace anywhere in the packed part ignored, since chat
// apps wrap long links, and with the words a message puts round a link
// (a full stop, brackets, a sentence going on) left out. Pure, apart from
// the deflate it is given.

import { inflate as inflateRaw, deflate } from './deflate.ts';
import { parseServer } from './link.ts';

/** The two kinds of link. */
export type LinkKind = 'ask' | 'take';

/** A request, as its link carries it. */
export interface AskLink {
  kind: 'ask';
  /** The server the request's mint is reached at. */
  server: string;
  /** The SDK's PaymentRequest JSON, verbatim. */
  json: string;
}

/** A hand-back, as its link carries it. */
export interface TakeLink {
  kind: 'take';
  server: string;
  /** The mint's issuer key, 64 lowercase hex. */
  mint: string;
  /** The request it answers: its first note's one-time key, 64 lowercase hex. */
  ask: string;
  /** The tokens it carries, for words shown before the mint is asked. */
  amount: number;
  /** The receiver's notes, each as the SDK hands them over. */
  notes: string[];
}

/**
 * Why a text is not a link the purse can read:
 *
 * - `empty`: nothing.
 * - `not_a_link`: no `ask=` or `take=` in it.
 * - `mint_link`: a mint's link (`#mint=…&at=…`), not a request or a hand-back.
 * - `too_long`: longer than any request or hand-back.
 * - `unreadable`: damaged, cut short, or of the other kind than its hash says.
 * - `newer`: written by a newer purse.
 * - `server`: it names a server the purse cannot reach.
 */
export type LinkProblem = 'empty' | 'not_a_link' | 'mint_link' | 'too_long' | 'unreadable' | 'newer' | 'server';

/** What {@link decode} makes of a text. */
export type Decoded =
  | { ok: true; link: AskLink | TakeLink }
  | { ok: false; problem: LinkProblem; kind?: LinkKind };

/**
 * The most characters read: more than the largest real link, a hand-back
 * of 256 notes (about 40,000 characters), with room for a message round it.
 */
export const MAX_TEXT = 65_536;

/** The most bytes a packed part may inflate to. */
export const MAX_INFLATED = 131_072;

/** The most notes a hand-back carries: the SDK's MAX_REQUESTED_NOTES. */
export const MAX_NOTES = 256;

/** The frame version this purse writes and reads. */
const VERSION = 1;

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';

/** Bytes as base64url, unpadded (RFC 4648 §5). */
export function base64url(bytes: Uint8Array): string {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const a = bytes[i] ?? 0;
    const b = bytes[i + 1];
    const c = bytes[i + 2];
    const n = (a << 16) | ((b ?? 0) << 8) | (c ?? 0);
    out += B64[(n >> 18) & 63];
    out += B64[(n >> 12) & 63];
    if (b !== undefined) {
      out += B64[(n >> 6) & 63];
    }
    if (c !== undefined) {
      out += B64[n & 63];
    }
  }
  return out;
}

/** base64url back to bytes, padding allowed; `undefined` for any other character or a cut-off length. */
export function fromBase64url(text: string): Uint8Array<ArrayBuffer> | undefined {
  const body = text.replace(/={1,2}$/, '');
  if (!/^[A-Za-z0-9_-]*$/.test(body) || body.length % 4 === 1) {
    return undefined;
  }
  const out = new Uint8Array(Math.floor((body.length * 3) / 4));
  let at = 0;
  for (let i = 0; i < body.length; i += 4) {
    const chunk = body.slice(i, i + 4);
    let n = 0;
    for (let j = 0; j < 4; j++) {
      n = (n << 6) | (j < chunk.length ? B64.indexOf(chunk[j] as string) : 0);
    }
    out[at++] = (n >> 16) & 255;
    if (chunk.length > 2) {
      out[at++] = (n >> 8) & 255;
    }
    if (chunk.length > 3) {
      out[at++] = n & 255;
    }
  }
  return out;
}

/** A frame's fields, refused if one would break the frame. */
function frame(lines: readonly string[]): Uint8Array {
  for (const line of lines) {
    if (/[\r\n]/.test(line)) {
      throw new RangeError('a link field holds a line break');
    }
  }
  return new TextEncoder().encode(lines.join('\n'));
}

/** The packed part of a request's link: its server and its JSON. */
export async function encodeAsk(server: string, json: string): Promise<string> {
  return base64url(await deflate(frame([`ask/${VERSION}`, server, json])));
}

/** The packed part of a hand-back's link. */
export async function encodeTake(
  server: string,
  mint: string,
  ask: string,
  amount: number,
  notes: readonly string[],
): Promise<string> {
  if (notes.length === 0 || notes.length > MAX_NOTES) {
    throw new RangeError(`a hand-back carries 1 to ${MAX_NOTES} notes`);
  }
  if (!Number.isSafeInteger(amount) || amount < 1) {
    throw new RangeError('a hand-back carries a whole number of tokens, 1 or more');
  }
  return base64url(await deflate(frame([`take/${VERSION}`, server, mint, ask, String(amount), ...notes])));
}

/** The link others open: `base` (the purse's address, or nothing for a bare fragment) and the packed part. */
export const linkFor = (kind: LinkKind, packed: string, base = ''): string => `${base}#${kind}=${packed}`;

/** What reads the packed part: the purse's inflate, or a stand-in in tests. */
export type Inflater = (bytes: Uint8Array, cap: number) => Promise<Uint8Array>;

const HEX = /^[0-9a-fA-F]{64}$/;

/** The most words after a link that are tried as text round it: a sentence going on after a link. */
const MAX_TRAILING = 16;

/**
 * Reads a request or a hand-back link from what was opened, pasted or
 * scanned. The packed part runs from `ask=` or `take=` to the first
 * character base64url has no use for (`&`, a full stop, a bracket);
 * whitespace inside it is a wrapped line, unless the words after it make
 * it unreadable, when they are left off one by one.
 */
export async function decode(text: string, inflater: Inflater = inflateRaw): Promise<Decoded> {
  const trimmed = text.trim();
  if (trimmed === '') {
    return { ok: false, problem: 'empty' };
  }
  if (trimmed.length > MAX_TEXT) {
    return { ok: false, problem: 'too_long' };
  }
  const found = /#(ask|take)=/.exec(trimmed) ?? /^(ask|take)=/.exec(trimmed);
  if (found === null) {
    return { ok: false, problem: /#mint=|^mint=|&at=/.test(trimmed) ? 'mint_link' : 'not_a_link' };
  }
  const kind = found[1] as LinkKind;
  const rest = trimmed.slice(found.index + found[0].length);
  const chunks = (/^[A-Za-z0-9_\-=\s]*/.exec(rest)?.[0] ?? '').split(/\s+/).filter((chunk) => chunk !== '');
  let read: Decoded = { ok: false, problem: 'unreadable', kind };
  for (let n = chunks.length; n >= Math.max(1, chunks.length - MAX_TRAILING); n--) {
    read = await readPacked(kind, chunks.slice(0, n).join(''), inflater);
    if (read.ok || read.problem !== 'unreadable') {
      return read;
    }
  }
  return read;
}

/** Reads the packed part of a link of `kind`. */
async function readPacked(kind: LinkKind, packed: string, inflater: Inflater): Promise<Decoded> {
  const unreadable = { ok: false, problem: 'unreadable', kind } as const;
  const bytes = fromBase64url(packed);
  if (bytes === undefined || bytes.length === 0) {
    return unreadable;
  }
  let lines: string[];
  try {
    const raw = await inflater(bytes, MAX_INFLATED);
    if (raw.length > MAX_INFLATED) {
      return unreadable;
    }
    lines = new TextDecoder('utf-8', { fatal: true }).decode(raw).split('\n');
  } catch {
    return unreadable;
  }
  const head = /^(ask|take)\/(\d{1,4})$/.exec(lines[0] ?? '');
  if (head === null || head[1] !== kind) {
    return unreadable;
  }
  const version = Number(head[2]);
  if (version > VERSION) {
    return { ok: false, problem: 'newer', kind };
  }
  if (version < 1) {
    return unreadable;
  }
  if (kind === 'ask' ? lines.length !== 3 : lines.length < 6 || lines.length > 5 + MAX_NOTES) {
    return unreadable;
  }
  const server = parseServer(lines[1] ?? '');
  if (!server.ok) {
    return { ok: false, problem: 'server', kind };
  }
  if (kind === 'ask') {
    const json = lines[2] ?? '';
    return json.length < 2 ? unreadable : { ok: true, link: { kind, server: server.server, json } };
  }
  const [, , mint = '', ask = '', amountText = '', ...notes] = lines;
  if (!HEX.test(mint) || !HEX.test(ask) || !/^[1-9]\d{0,15}$/.test(amountText)) {
    return unreadable;
  }
  const amount = Number(amountText);
  if (!Number.isSafeInteger(amount) || notes.some((note) => note.length < 1 || note.length > 4096 || /\s/.test(note))) {
    return unreadable;
  }
  return {
    ok: true,
    link: { kind, server: server.server, mint: mint.toLowerCase(), ask: ask.toLowerCase(), amount, notes },
  };
}
