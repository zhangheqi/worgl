import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { base64url } from '../src/links.ts';
import { holdersOf, readNote, sentTo, serialsSpent, valueOf } from '../src/notes.ts';

const hex = (b: Uint8Array): string => [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
const key = (seed: number): Uint8Array => Uint8Array.from({ length: 32 }, (_, i) => (seed * 31 + i * 7) & 255);
const MINT = key(1);

/** A note as docs/note-format lays it out: version, issuer, generation, index, amount, holder, signature. */
function note(o: { generation: number; index: number; amount: number; holder: Uint8Array; version?: number }): string {
  const bytes = new Uint8Array(149);
  const view = new DataView(bytes.buffer);
  bytes[0] = o.version ?? 1;
  bytes.set(MINT, 1);
  view.setBigUint64(33, BigInt(o.generation));
  view.setUint32(41, o.index);
  view.setBigUint64(45, BigInt(o.amount));
  bytes.set(o.holder, 53);
  bytes.fill(0xab, 85);
  return base64url(bytes);
}

/** A transfer's JSON as the SDK signs it: one input, its outputs. */
const transfer = (input: string, outputs: readonly [number, Uint8Array][]): string =>
  JSON.stringify({ inputs: [{ note: input, signature: 'ab'.repeat(64) }], outputs: outputs.map(([amount, holder]) => ({ amount, holder: hex(holder) })) });

describe('notes', () => {
  test('read a note’s serial, value and key from its 149 bytes', () => {
    const text = note({ generation: 3, index: 739, amount: 32, holder: key(9) });
    assert.equal(text.length, 199);
    assert.deepEqual(readNote(text), { mint: hex(MINT), id: '3:739', amount: 32, holder: hex(key(9)) });
  });

  test('read nothing from text that is not a note of format 1', () => {
    const good = note({ generation: 3, index: 1, amount: 4, holder: key(2) });
    assert.equal(readNote(note({ generation: 3, index: 1, amount: 4, holder: key(2), version: 2 })), undefined);
    assert.equal(readNote(note({ generation: 3, index: 1, amount: 0, holder: key(2) })), undefined);
    assert.equal(readNote(good.slice(0, -4)), undefined);
    assert.equal(readNote(`${good}AAAA`), undefined);
    assert.equal(readNote('not a note'), undefined);
    assert.equal(readNote(''), undefined);
  });

  test('add up what notes carry, or say it is not known', () => {
    const notes = [1, 4, 16].map((amount, i) => note({ generation: 3, index: i, amount, holder: key(i) }));
    assert.equal(valueOf(notes), 21);
    assert.equal(valueOf([]), 0);
    assert.equal(valueOf([...notes, 'junk']), undefined);
  });

  test('read a request’s one-time keys, in lowercase', () => {
    const json = JSON.stringify({ mint: hex(MINT), outputs: [{ amount: 1, holder: hex(key(5)).toUpperCase() }, { amount: 4, holder: hex(key(6)) }] });
    assert.deepEqual(holdersOf(json), [hex(key(5)), hex(key(6))]);
    assert.deepEqual(holdersOf('{'), []);
    assert.deepEqual(holdersOf('{"outputs": [{"amount": 1}]}'), []);
  });

  test('tell what transfers pay a request’s keys from the change they pay back', () => {
    // a send of 5 (1 + 4) from a note of 64: 59 back as 2 + 8 + 16 + 32 + 1 in two transfers
    const theirs = [key(5), key(6)];
    const spent = note({ generation: 2, index: 105, amount: 64, holder: key(7) });
    const spent2 = note({ generation: 2, index: 106, amount: 8, holder: key(8) });
    const transfers = [
      transfer(spent, [[1, key(5)], [16, key(20)], [32, key(21)], [15, key(22)]]),
      transfer(spent2, [[4, theirs[1] as Uint8Array], [4, key(23)]]),
    ];
    assert.deepEqual(sentTo(transfers, theirs.map(hex)), { theirs: 5, change: 67 });
    assert.deepEqual(sentTo(transfers.slice(0, 1), theirs.map(hex)), { theirs: 1, change: 63 });
    assert.deepEqual(sentTo([], theirs.map(hex)), { theirs: 0, change: 0 });
    assert.equal(sentTo(['{"outputs": [{"amount": "1", "holder": "aa"}]}'], []), undefined);
    assert.equal(sentTo(['not json'], []), undefined);
    assert.deepEqual(serialsSpent(transfers), ['2:105', '2:106']);
    assert.deepEqual(serialsSpent(['not json', '{"inputs": [{"note": "junk"}]}']), []);
  });
});
