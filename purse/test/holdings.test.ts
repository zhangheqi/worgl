import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import type { HeldNote, MintInfo } from 'worgl';

import { fadedAt, holdingsFrom, laneOf, lanePositions, laneTotal } from '../src/holdings.ts';

const MINT = 'ab'.repeat(32);
const NEXT = 1_792_400_400n; // a rotation, in Unix seconds by the mint's clock
const PERIOD = 28_800n;

const info = (more: Partial<MintInfo> = {}): MintInfo => ({
  issuer: MINT,
  sequence: 9n,
  layers: 4,
  oldest: 5n,
  paused: false,
  rotation: { periodSecs: PERIOD, nextAt: NEXT },
  supply: { minted: 192n, destroyed: 0n, burned: 0n, outstanding: 192n },
  ...more,
});

const held = (
  index: number,
  amount: bigint,
  state: HeldNote['state'],
  position: number | undefined,
): HeldNote => ({
  serial: { generation: 5n + BigInt(position ?? -1), index },
  amount,
  state,
  position,
  spendableAt: state === 'frozen' ? NEXT : undefined,
  expiresAt: position === undefined ? undefined : NEXT + BigInt(position) * PERIOD,
});

describe('holdings', () => {
  test('put each layer in its lane', () => {
    assert.deepEqual([0, 1, 2, 3].map((pos) => laneOf(pos, 4)), ['fade', 'ready', 'ready', 'rest']);
    assert.deepEqual([0, 1].map((pos) => laneOf(pos, 2)), ['fade', 'rest']);
    assert.deepEqual(lanePositions('ready', 4), [1, 2]);
    assert.deepEqual(lanePositions('ready', 2), []);
  });

  test('from the SDK keep the notes that count, in this device’s clock', () => {
    const notes = [
      held(0, 64n, 'expiring', 0),
      held(1, 64n, 'spendable', 1),
      held(2, 64n, 'frozen', 3),
      held(3, 8n, 'expired', undefined),
      held(4, 4n, 'unknown', undefined),
    ];
    const arrivals = new Map([['8:2', { at: 1_792_390_000_000, source: 'faucet' as const }]]);
    // the mint's clock runs 2 s ahead of this device's
    const h = holdingsFrom(info(), notes, 192n, NEXT - 3600n, 2, arrivals);
    assert.deepEqual(
      h.notes.map((n) => [n.id, n.v, n.pos]),
      [['5:0', 64, 0], ['6:1', 64, 1], ['8:2', 64, 3]],
    );
    assert.equal(h.total, 192);
    assert.equal(h.layers, 4);
    assert.deepEqual(h.schedule, {
      startMs: (Number(NEXT - PERIOD) - 2) * 1000,
      nextMs: (Number(NEXT) - 2) * 1000,
      periodMs: 28_800_000,
    });
    assert.equal(h.firstExpiry, (Number(NEXT) - 3600 - 2) * 1000);
    assert.equal(h.notes[2]?.readyAt, (Number(NEXT) - 2) * 1000);
    assert.equal(h.notes[2]?.source, 'faucet');
    assert.equal(h.notes[2]?.arrivedAt, 1_792_390_000_000);
    assert.equal(h.notes[0]?.source, undefined);
    assert.deepEqual(['fade', 'ready', 'rest'].map((lane) => laneTotal(h, lane as 'fade')), [64, 64, 64]);
  });

  test('of a paused mint or one with no schedule have no schedule', () => {
    assert.equal(holdingsFrom(info({ paused: true }), [], 0n, undefined, 0, new Map()).schedule, undefined);
    assert.equal(holdingsFrom(info({ rotation: undefined }), [], 0n, undefined, 0, new Map()).schedule, undefined);
  });
});

describe('a faded note', () => {
  test('faded at the rotation that dropped its layer', () => {
    const now = (Number(NEXT) - 100) * 1000;
    // oldest 5: generation 4 went at the last rotation, generation 2 three rotations ago
    assert.equal(fadedAt(4n, info(), 0, now), Number(NEXT - PERIOD) * 1000);
    assert.equal(fadedAt(2n, info(), 0, now), Number(NEXT - 3n * PERIOD) * 1000);
    assert.equal(fadedAt(4n, info(), 2, now), (Number(NEXT - PERIOD) - 2) * 1000);
    assert.equal(fadedAt(4n, info({ rotation: undefined }), 0, now), now);
    assert.equal(fadedAt(5n, info(), 0, now), now);
  });
});
