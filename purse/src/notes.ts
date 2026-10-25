// What the purse reads of a note and of a transfer itself, from their
// documented formats: a note's 149 bytes (docs/note-format) and a
// transfer's JSON, the body of `POST /v1/transfer` (docs/http-api). The
// SDK hands notes and transfers over as text and reads neither for the
// purse, which needs a note's serial, value and one-time key to tell what a
// send came to and which request a hand-back answers, from what was signed
// rather than from a balance or a link's own words. Pure, so it is tested.

import { fromBase64url } from './links.ts';

/** A note's fields, as its bytes carry them. */
export interface NoteFields {
  /** The mint's issuer key, 64 lowercase hex. */
  mint: string;
  /** Its serial as the purse names it, `generation:index` (holdings.ts noteId). */
  id: string;
  amount: number;
  /** The one-time key it is paid to, 64 lowercase hex. */
  holder: string;
}

/** The length of a note in bytes, and the only format version read. */
const NOTE_BYTES = 149;
const NOTE_VERSION = 1;

const hex = (bytes: Uint8Array): string => [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');

/** A big-endian unsigned integer of `bytes`, while it stays a safe integer; `undefined` past that. */
function uint(bytes: Uint8Array): number | undefined {
  let n = 0;
  for (const b of bytes) {
    n = n * 256 + b;
  }
  return Number.isSafeInteger(n) ? n : undefined;
}

/** A note's fields; `undefined` for text that is not a note of format 1, or a value past what a JS number holds. */
export function readNote(text: string): NoteFields | undefined {
  const bytes = fromBase64url(text);
  if (bytes === undefined || bytes.length !== NOTE_BYTES || bytes[0] !== NOTE_VERSION) {
    return undefined;
  }
  const generation = uint(bytes.subarray(33, 41));
  const index = uint(bytes.subarray(41, 45));
  const amount = uint(bytes.subarray(45, 53));
  if (generation === undefined || index === undefined || amount === undefined || amount < 1) {
    return undefined;
  }
  return { mint: hex(bytes.subarray(1, 33)), id: `${generation}:${index}`, amount, holder: hex(bytes.subarray(53, 85)) };
}

/** What notes are worth together; `undefined` when one of them does not read. */
export function valueOf(notes: readonly string[]): number | undefined {
  let total = 0;
  for (const note of notes) {
    const fields = readNote(note);
    if (fields === undefined) {
      return undefined;
    }
    total += fields.amount;
  }
  return total;
}

/** The one-time keys of a request, from its JSON: `{"mint", "outputs": [{"amount", "holder"}]}`. */
export function holdersOf(json: string): string[] {
  return outputsOf(json)?.map((output) => output.holder) ?? [];
}

/** The outputs a request or a transfer names, as its JSON writes them; `undefined` when it does not read. */
function outputsOf(json: string): { amount: number; holder: string }[] | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return undefined;
  }
  const outputs = typeof parsed === 'object' && parsed !== null ? (parsed as { outputs?: unknown }).outputs : undefined;
  if (!Array.isArray(outputs)) {
    return undefined;
  }
  const out: { amount: number; holder: string }[] = [];
  for (const output of outputs as unknown[]) {
    const { amount, holder } = (typeof output === 'object' && output !== null ? output : {}) as { amount?: unknown; holder?: unknown };
    if (typeof amount !== 'number' || !Number.isSafeInteger(amount) || amount < 1 || typeof holder !== 'string') {
      return undefined;
    }
    out.push({ amount, holder: holder.toLowerCase() });
  }
  return out;
}

/** The serials of the notes `transfers` (each a transfer's JSON, `{"inputs": [{"note", "signature"}], …}`) spend. */
export function serialsSpent(transfers: readonly string[]): string[] {
  return transfers.flatMap((transfer) => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(transfer);
    } catch {
      return [];
    }
    const inputs = typeof parsed === 'object' && parsed !== null ? (parsed as { inputs?: unknown }).inputs : undefined;
    return (Array.isArray(inputs) ? (inputs as unknown[]) : []).flatMap((input) => {
      const note = typeof input === 'object' && input !== null ? (input as { note?: unknown }).note : undefined;
      return typeof note === 'string' ? (readNote(note)?.id ?? []) : [];
    });
  });
}

/**
 * What `transfers` (each a transfer's JSON) pay: to the keys of the request
 * they answer (`theirs`), and to anyone else, which for a send of this
 * purse's is its own change (`change`). `undefined` when one does not read.
 */
export function sentTo(transfers: readonly string[], holders: Iterable<string>): { theirs: number; change: number } | undefined {
  const keys = new Set([...holders].map((holder) => holder.toLowerCase()));
  let theirs = 0;
  let change = 0;
  for (const transfer of transfers) {
    const outputs = outputsOf(transfer);
    if (outputs === undefined) {
      return undefined;
    }
    for (const output of outputs) {
      if (keys.has(output.holder)) {
        theirs += output.amount;
      } else {
        change += output.amount;
      }
    }
  }
  return { theirs, change };
}
