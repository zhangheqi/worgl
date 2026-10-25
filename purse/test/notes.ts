// Notes and transfers as the mint and the SDK write them, for the tests:
// a note's 149 bytes (`note-format.md`) with a signature of zeros, and a
// transfer's JSON (`http-api.md`).

import { type NoteFields, decodeNote } from '../src/keep.ts';

export const ISSUER = '11'.repeat(32);
export const OTHER = '33'.repeat(32);

/** The bytes of an unsigned big-endian integer of `size` bytes. */
function be(n: bigint, size: number): number[] {
  return Array.from({ length: size }, (_, i) => Number((n >> BigInt(8 * (size - 1 - i))) & 0xffn));
}

export interface NoteSpec {
  issuer?: string;
  generation?: bigint;
  index?: number;
  amount?: bigint;
  version?: number;
  /** Bytes kept: 149 for a whole note. */
  length?: number;
}

/** A note's text: unpadded base64url, 199 characters for its 149 bytes. */
export function noteText({ issuer = ISSUER, generation = 7n, index = 0, amount = 64n, version = 1, length = 149 }: NoteSpec = {}): string {
  const bytes = [
    version,
    ...Buffer.from(issuer, 'hex'),
    ...be(generation, 8),
    ...be(BigInt(index), 4),
    ...be(amount, 8),
    ...Buffer.from('22'.repeat(32), 'hex'),
    ...new Array<number>(64).fill(0),
  ];
  return Buffer.from(bytes.slice(0, length)).toString('base64url');
}

/** A transfer spending `notes`, with `outputs` outputs. */
export const transfer = (notes: readonly string[], outputs = notes.length): string =>
  JSON.stringify({
    inputs: notes.map((note) => ({ note, signature: 'AA' })),
    outputs: Array.from({ length: outputs }, () => ({ amount: 64, holder: '22'.repeat(32) })),
  });

/** The notes a transfer's inputs spend, decoded. */
export function decodeNoteParts(request: string): NoteFields[] {
  const body = JSON.parse(request) as { inputs: { note: string }[] };
  return body.inputs.flatMap(({ note }) => {
    const fields = decodeNote(note);
    return fields === undefined ? [] : [fields];
  });
}
