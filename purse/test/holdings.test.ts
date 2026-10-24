import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import type { HeldNote, MintInfo } from 'worgl';

import {
  arrivalsOf,
  comeBackAt,
  fadedAt,
  firstFade,
  holdingsFrom,
  laneOf,
  lanePositions,
  laneTotal,
  savedHoldings,
} from '../src/holdings.ts';

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

describe('notes on their way a layer on', () => {
  test('are drawn where they were until the mint answers, and the total stays the SDK’s', () => {
    const notes = [held(0, 64n, 'spendable', 1)];
    const moving = [
      { id: '5:7', v: 64, generation: 5n },
      { id: '6:8', v: 4, generation: 6n },
      // one past the last layer: not drawn
      { id: '9:1', v: 1, generation: 9n },
    ];
    const arrivals = new Map([['5:7', { at: 1_792_390_000_000, source: 'kept' as const }]]);
    const h = holdingsFrom(info(), notes, 999n, NEXT - 3600n, 2, arrivals, moving);
    assert.deepEqual(
      h.notes.map((n) => [n.id, n.v, n.pos, n.moving]),
      [['6:0', 64, 1, false], ['5:7', 64, 0, true], ['6:8', 4, 1, true]],
    );
    assert.equal(h.total, 999);
    const first = h.notes[1];
    assert.equal(first === undefined ? undefined : laneOf(first.pos, h.layers), 'fade');
    assert.equal(first?.expiresAt, (Number(NEXT) - 2) * 1000);
    assert.equal(first?.readyAt, undefined);
    assert.equal(h.notes[2]?.expiresAt, (Number(NEXT + PERIOD) - 2) * 1000);
    assert.equal(laneTotal(h, 'fade'), 64);
    // a note keep-alive moved says so
    assert.equal(first?.source, 'kept');
    assert.equal(first?.arrivedAt, 1_792_390_000_000);
  });

  test('whose layer went since are drawn in the first, where an answer would land; two layers back, they are gone', () => {
    // oldest 5: generation 4 went at the last rotation, generation 3 the one before
    const moving = [
      { id: '4:2', v: 64, generation: 4n },
      { id: '3:5', v: 32, generation: 3n },
    ];
    const h = holdingsFrom(info(), [held(0, 64n, 'spendable', 1)], 160n, NEXT, 0, new Map(), moving);
    assert.deepEqual(
      h.notes.map((n) => [n.id, n.pos, n.moving]),
      [['6:0', 1, false], ['4:2', 0, true]],
    );
    assert.equal(h.notes[1]?.expiresAt, Number(NEXT) * 1000);
    // the SDK counts the 32 still; its note and the one it would bring are both gone, so the purse does not
    assert.equal(h.total, 128);
    assert.equal(laneTotal(h, 'fade') + laneTotal(h, 'ready'), h.total);
  });

  test('are none by default', () => {
    const h = holdingsFrom(info(), [held(0, 64n, 'expiring', 0)], 64n, NEXT, 0, new Map());
    assert.deepEqual(h.notes.map((n) => n.moving), [false]);
  });
});

describe('what fades first', () => {
  test('is the lowest layer’s notes at the first expiry, or nothing', () => {
    const notes = [held(0, 64n, 'expiring', 0), held(1, 4n, 'expiring', 0), held(2, 64n, 'spendable', 1)];
    const h = holdingsFrom(info(), notes, 132n, NEXT, 0, new Map());
    assert.deepEqual(firstFade(h), { at: Number(NEXT) * 1000, amount: 68 });
    assert.equal(firstFade(holdingsFrom(info(), [], 0n, undefined, 0, new Map())), null);
    assert.equal(firstFade(holdingsFrom(info({ paused: true }), notes, 132n, undefined, 0, new Map())), null);
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

describe('where a note came from', () => {
  const OTHER = 'cd'.repeat(32);
  // newest first, as the log reads back
  const log = [
    { t: 3000, kind: 'kept' as const, mint: MINT, serials: ['7:1', '7:2'] },
    { t: 2500, kind: 'faucet' as const, mint: OTHER, serials: ['7:3'] },
    { t: 2000, kind: 'lost' as const, mint: MINT, serials: [] },
    { t: 1500, kind: 'faded' as const, mint: MINT, serials: ['6:9'] },
    { t: 1000, kind: 'kept' as const, mint: MINT, serials: ['7:1'] },
    { t: 500, kind: 'faucet' as const, mint: MINT, serials: ['7:2', '8:4'] },
  ];

  test('is the latest of the mint’s faucet and kept entries', () => {
    const known = arrivalsOf(log, MINT, new Map());
    assert.deepEqual([...known].sort(), [
      ['7:1', { at: 3000, source: 'kept' }],
      ['7:2', { at: 3000, source: 'kept' }],
      ['8:4', { at: 500, source: 'faucet' }],
    ]);
  });

  test('for a note keep-alive just moved, is its hop until its run’s entry is in', () => {
    const known = arrivalsOf(log, MINT, new Map([['9:5', 4000], ['7:1', 4000]]));
    assert.deepEqual(known.get('9:5'), { at: 4000, source: 'kept' });
    // the log's entry wins over the hop
    assert.deepEqual(known.get('7:1'), { at: 3000, source: 'kept' });
    assert.equal(arrivalsOf(log, OTHER, new Map()).get('7:3')?.source, 'faucet');
  });
});

describe('holdings as the purse saved them', () => {
  const at = Number(NEXT) * 1000;
  const saved = { layers: 4, periodSecs: Number(PERIOD), paused: false, readAt: 0, fades: { at: at + 28_800_000, amount: 64 } };

  test('place what fades first by its time, so the date to come back by can be worked out without the mint', () => {
    const now = at - 3_600_000;
    const h = savedHoldings(saved, now);
    assert.deepEqual(h?.notes.map((n) => [n.v, n.pos, n.expiresAt]), [[64, 1, at + 28_800_000]]);
    assert.equal(h?.firstExpiry, at + 28_800_000);
    assert.deepEqual(h?.schedule, { startMs: at - 28_800_000, nextMs: at, periodMs: 28_800_000 });
    // an hour before they fade, as for a mint that answers
    assert.equal(h === undefined ? undefined : comeBackAt(h, now), at + 25_200_000);
    // in the last period before it, they are in the first layer
    assert.equal(savedHoldings(saved, at + 1000)?.notes[0]?.pos, 0);
  });

  test('are none when nothing fades, the mint was paused, or the time has gone', () => {
    assert.equal(savedHoldings({ ...saved, fades: null }, 0), undefined);
    assert.equal(savedHoldings({ ...saved, fades: undefined }, 0), undefined);
    assert.equal(savedHoldings({ ...saved, paused: true }, 0), undefined);
    assert.equal(savedHoldings({ ...saved, periodSecs: undefined }, 0), undefined);
    assert.equal(savedHoldings(saved, at + 28_800_000), undefined);
  });
});
