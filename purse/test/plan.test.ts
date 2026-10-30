import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  type Denoms,
  type Planned,
  type Policy,
  type Spendable,
  atMostOptions,
  droppedValue,
  plan,
  policyFrom,
  shortCase,
  shown,
  split,
  withRefused,
} from '../src/plan.ts';

const POWERS: Denoms = { kind: 'powers' };
const ANY: Denoms = { kind: 'any' };
const PLAIN: Policy = { denominations: POWERS, maxOutputs: 16 };
const LAYERS = 4;

let serial = 0;
/** A note of `v` at layer `pos`. */
const note = (v: number, pos = 2): Spendable => ({ id: `3:${serial++}`, v, pos });

/** The parts of a plan that went, or the refusal. */
function parts(planned: Planned | undefined): { note: string; theirs: number[]; change: number[]; lands: number }[] {
  assert.ok(planned?.ok === true, `planned: ${JSON.stringify(planned)}`);
  return planned.parts.map((part) => ({ note: part.note.id, theirs: part.theirs, change: part.change, lands: part.lands }));
}

describe('the split of an amount into notes', () => {
  test('follows the denominations and the cap, as the SDK splits', () => {
    assert.deepEqual(split(POWERS, 13, undefined, 256), [8, 4, 1]);
    assert.deepEqual(split(POWERS, 13, 4, 256), [4, 4, 4, 1]);
    assert.deepEqual(split(POWERS, 13, 5, 256), [4, 4, 4, 1]);
    assert.equal(split(POWERS, 2 ** 20, 1, 256), 'too_many');
    assert.deepEqual(split(POWERS, 0, undefined, 256), []);
    assert.equal(split(POWERS, 5, 0, 256), 'inexact');
    assert.deepEqual(split(ANY, 7, undefined, 256), [7]);
    assert.deepEqual(split(ANY, 7, 3, 256), [3, 3, 1]);
    const list: Denoms = { kind: 'list', values: [1, 2, 5] };
    assert.deepEqual(split(list, 9, undefined, 256), [5, 2, 2]);
    assert.deepEqual(split(list, 9, 2, 256), [2, 2, 2, 2, 1]);
    const fives: Denoms = { kind: 'list', values: [5] };
    assert.equal(split(fives, 7, undefined, 256), 'inexact');
    assert.equal(split(fives, 5, 4, 256), 'inexact');
    assert.equal(split(POWERS, 255, undefined, 7), 'too_many');
  });
});

describe('the plan of a send', () => {
  test('spends one note when one covers it, change and all', () => {
    const n = note(64);
    assert.deepEqual(parts(plan([n], [4, 1], PLAIN, LAYERS)), [{ note: n.id, theirs: [4, 1], change: [32, 16, 8, 2, 1], lands: 3 }]);
  });

  test('takes the largest notes first when no one note covers it', () => {
    const a = note(64);
    const b = note(64);
    assert.deepEqual(parts(plan([a, b], [64, 4, 1], PLAIN, LAYERS)), [
      { note: b.id, theirs: [64], change: [], lands: 3 },
      { note: a.id, theirs: [4, 1], change: [32, 16, 8, 2, 1], lands: 3 },
    ]);
  });

  test('spends a large note on a small request when no smaller one covers it', () => {
    const big = note(4096);
    const held = [note(64), note(64), note(64), note(64), big];
    assert.deepEqual(parts(plan(held, [64, 32, 4], PLAIN, LAYERS)), [
      { note: big.id, theirs: [64, 32, 4], change: [2048, 1024, 512, 256, 128, 16, 8, 4], lands: 3 },
    ]);
  });

  test('refuses as the SDK does: too little, no note big enough, too many outputs', () => {
    assert.deepEqual(plan([note(64), note(64)], [128], PLAIN, LAYERS), { ok: false, why: 'fragmented', largest: 64, atMost: 64 });
    assert.deepEqual(plan([note(64)], [64, 4], PLAIN, LAYERS), { ok: false, why: 'unaffordable', spendable: 64, needed: 68 });
    assert.deepEqual(plan([note(64)], [2, 1], { denominations: POWERS, maxOutputs: 2 }, LAYERS), {
      ok: false,
      why: 'too_many_outputs',
      maxOutputs: 2,
    });
  });

  test('spends a fading note like any other, and its change lands one layer on', () => {
    const fading = note(8, 0);
    assert.deepEqual(parts(plan([fading], [5], PLAIN, LAYERS)), [{ note: fading.id, theirs: [5], change: [2, 1], lands: 1 }]);
  });

  test('takes notes of one value in the order the purse lists them', () => {
    const first = note(64, 1);
    const second = note(64, 2);
    assert.equal(parts(plan([first, second], [5], PLAIN, LAYERS))[0]?.note, first.id);
    assert.equal(parts(plan([second, first], [5], PLAIN, LAYERS))[0]?.note, second.id);
  });

  test('finds the largest note a request may ask for and still go, or none', () => {
    // the spec's example: notes of [3, 3] held, against a request for [4, 2]. At most 2 asks for three 2s,
    // and two notes of 3 carry only two of them, so it is 1
    assert.deepEqual(plan([note(3), note(3)], [4, 2], PLAIN, LAYERS), { ok: false, why: 'fragmented', largest: 3, atMost: 1 });
    const fours: Policy = { denominations: { kind: 'list', values: [4] }, maxOutputs: 16 };
    assert.deepEqual(plan([note(3), note(3)], [4], fours, LAYERS), { ok: false, why: 'fragmented', largest: 3, atMost: undefined });
  });

  test('says nothing past what a number holds exactly', () => {
    assert.equal(plan([note(2 ** 60)], [5], PLAIN, LAYERS), undefined);
    assert.equal(plan([note(64)], [Number.MAX_SAFE_INTEGER, 5], PLAIN, LAYERS), undefined);
  });
});

describe('the sizes Ask in smaller notes offers', () => {
  test('are the powers under the plain split’s largest, at most eight, none for 1 or under any', () => {
    assert.deepEqual(atMostOptions(POWERS, 20), [8, 4, 2, 1]);
    assert.deepEqual(atMostOptions(POWERS, 128), [64, 32, 16, 8, 4, 2, 1]);
    assert.deepEqual(atMostOptions(POWERS, 4096), [2048, 1024, 512, 256, 128, 64, 32, 16]);
    assert.deepEqual(atMostOptions(POWERS, 1000), [256, 128, 64, 32, 16, 8, 4]);
    assert.deepEqual(atMostOptions(POWERS, 1), []);
    assert.deepEqual(atMostOptions(POWERS, 2), [1]);
    assert.deepEqual(atMostOptions(ANY, 20), []);
    assert.deepEqual(atMostOptions({ kind: 'list', values: [1, 2, 5, 10] }, 20), [5, 2, 1]);
  });
});

describe('the policy from a mint’s info', () => {
  test('falls back to powers of two and 16 outputs', () => {
    assert.deepEqual(policyFrom({}), { policy: { denominations: POWERS, maxOutputs: 16 }, mintLayer: undefined, layers: 0 });
    assert.deepEqual(policyFrom(null).policy, PLAIN);
    assert.deepEqual(policyFrom({ policy: { denominations: 'nonsense', max_outputs: -1 } }).policy, PLAIN);
  });

  test('reads a list, its outputs, the mint layer and the layers', () => {
    assert.deepEqual(
      policyFrom({ policy: { denominations: { list: [1, 2, 5] }, max_outputs: 4 }, mint_layer: 3, layers: [{}, {}, {}, {}] }),
      { policy: { denominations: { kind: 'list', values: [1, 2, 5] }, maxOutputs: 4 }, mintLayer: 3, layers: 4 },
    );
    assert.deepEqual(policyFrom({ policy: { denominations: 'any' } }).policy.denominations, ANY);
    assert.deepEqual(policyFrom({ policy: { denominations: 'powers_of_two' } }).policy.denominations, POWERS);
  });
});

describe('what Confirm draws from a plan', () => {
  test('counts what is ready and resting after, by where the change lands', () => {
    const resting = plan([note(64)], [4, 1], PLAIN, LAYERS);
    assert.ok(resting?.ok === true);
    const r = shown(resting.parts, 64, 64, LAYERS);
    assert.deepEqual([r.readyAfter, r.laterAfter, r.changeLands, r.back], [0, 123, 'rest', [32, 16, 8, 2, 1]]);
    const ready = plan([note(8, 1)], [5], PLAIN, LAYERS);
    assert.ok(ready?.ok === true);
    const d = shown(ready.parts, 8, 0, LAYERS);
    assert.deepEqual([d.readyAfter, d.laterAfter, d.changeLands], [3, 0, 'ready']);
    const mixed = plan([note(4, 1), note(4, 2)], [3, 3], PLAIN, LAYERS);
    assert.ok(mixed?.ok === true);
    const m = shown(mixed.parts, 8, 0, LAYERS);
    assert.deepEqual([m.readyAfter, m.laterAfter, m.changeLands], [1, 1, 'mixed']);
    const whole = plan([note(64), note(64)], [64, 64], PLAIN, LAYERS);
    assert.ok(whole?.ok === true);
    const w = shown(whole.parts, 128, 0, LAYERS);
    assert.deepEqual([w.readyAfter, w.laterAfter, w.changeLands], [0, 0, undefined]);
  });
});

describe('the case of a request that cannot go now', () => {
  const refused = (planned: Planned | undefined): Exclude<Planned, { ok: true }> => {
    assert.ok(planned !== undefined && !planned.ok);
    return planned;
  };

  test('is over when the purse holds less, and empty when it holds nothing', () => {
    assert.deepEqual(shortCase(refused(plan([], [256, 32, 8, 4], PLAIN, LAYERS)), [], [], [256, 32, 8, 4], PLAIN, LAYERS), { kind: 'over', has: 0 });
    const ready = [note(64)];
    const frozen = [note(64, 3)];
    assert.deepEqual(shortCase(refused(plan(ready, [256, 32, 8, 4], PLAIN, LAYERS)), ready, frozen, [256, 32, 8, 4], PLAIN, LAYERS), {
      kind: 'over',
      has: 128,
    });
  });

  test('is none or later only when waiting would let it go as it is', () => {
    const frozen = [note(64, 3), note(32, 3)];
    assert.deepEqual(shortCase(refused(plan([], [16, 4], PLAIN, LAYERS)), [], frozen, [16, 4], PLAIN, LAYERS), { kind: 'none', has: 96 });
    const ready = [note(64)];
    assert.deepEqual(shortCase(refused(plan(ready, [128, 16, 4, 2], PLAIN, LAYERS)), ready, [note(64, 3), note(64, 3), note(32, 3)], [128, 16, 4, 2], PLAIN, LAYERS), {
      kind: 'notes',
      largest: 64,
      atMost: 64,
      maxOut: 128,
      waitHelps: false,
    });
    const later = shortCase(refused(plan(ready, [64, 32, 4], PLAIN, LAYERS)), ready, [note(64, 3)], [64, 32, 4], PLAIN, LAYERS);
    assert.deepEqual(later, { kind: 'later', has: 128 });
  });

  test('is notes when no ready note is big enough, saying whether waiting helps', () => {
    const ready = [note(64), note(64)];
    assert.deepEqual(shortCase(refused(plan(ready, [128], PLAIN, LAYERS)), ready, [], [128], PLAIN, LAYERS), {
      kind: 'notes',
      largest: 64,
      atMost: 64,
      maxOut: 128,
      waitHelps: false,
    });
    assert.deepEqual(shortCase(refused(plan(ready, [128], PLAIN, LAYERS)), ready, [note(128, 3)], [128], PLAIN, LAYERS), {
      kind: 'notes',
      largest: 64,
      atMost: 64,
      maxOut: 128,
      waitHelps: true,
    });
  });

  test('is outputs when the change would make too many notes', () => {
    const tight: Policy = { denominations: POWERS, maxOutputs: 2 };
    const ready = [note(64)];
    assert.deepEqual(shortCase(refused(plan(ready, [2, 1], tight, LAYERS)), ready, [], [2, 1], tight, LAYERS), { kind: 'outputs', maxOutputs: 2 });
  });
});

describe('a send the mint dropped', () => {
  /** A transfer as the SDK writes it: one input, and its outputs. */
  const transfer = (...amounts: number[]): string =>
    JSON.stringify({ inputs: [{ note: {}, signature: '00' }], outputs: amounts.map((amount, i) => ({ amount, holder: `${i}`.padStart(64, '0') })) });

  test('lost the whole note: theirs and the change, which is what this purse still expected', () => {
    assert.deepEqual(droppedValue([{ request: transfer(5, 2, 1), atStake: 3n }]), { lost: 8, theirs: 5 });
  });

  test('adds up every dropped transfer', () => {
    assert.deepEqual(
      droppedValue([
        { request: transfer(64), atStake: 0n },
        { request: transfer(5, 1, 2, 8, 16, 32), atStake: 59n },
      ]),
      { lost: 128, theirs: 69 },
    );
  });

  test('counts only the change of a transfer that does not read', () => {
    assert.deepEqual(droppedValue([{ request: '{', atStake: 3n }]), { lost: 3, theirs: 0 });
    assert.deepEqual(droppedValue([{ request: JSON.stringify({ outputs: [{ amount: 'x' }] }), atStake: 3n }]), { lost: 3, theirs: 0 });
  });

  test('keeps what each submit and settle dropped or turned away, and nothing of what it kept', () => {
    const first = withRefused({}, [
      { request: transfer(4, 60), atStake: 60n, outcome: 'dropped' },
      { request: transfer(4), atStake: undefined, outcome: 'kept' },
    ]);
    assert.deepEqual(first, { lost: 64, lostTheirs: 4, dropped: 1 });
    const later = withRefused(first, [
      { request: transfer(8, 56), atStake: 56n, outcome: 'dropped' },
      { request: transfer(4), atStake: undefined, outcome: 'cancelled' },
    ]);
    assert.deepEqual(later, { lost: 128, lostTheirs: 12, dropped: 2, turnedAway: 1 });
    // a dropped transfer the purse did not stand to lose by is no loss of its
    assert.deepEqual(withRefused({}, [{ request: transfer(4), atStake: undefined, outcome: 'dropped' }]), {});
  });
});
