import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import type { HeldNote, MintInfo, ScheduledTransfer } from 'worgl';

import {
  RUN_MAX_SECS,
  boxes,
  broughtBy,
  budgetSecs,
  decodeNote,
  due,
  dueInput,
  hopOf,
  keptNotes,
  keptOf,
  leftSecs,
  mergeKept,
  movingNotes,
  outputsOf,
  respace,
  spaced,
  spacedPlan,
  spacingFor,
  spentBy,
  spentKey,
  stepOf,
} from '../src/keep.ts';
import { rng } from '../src/rng.ts';
import { ISSUER, OTHER, noteText, transfer } from './notes.ts';

const plan = (n: number): ScheduledTransfer[] => Array.from({ length: n }, (_, i) => ({ request: `r${i}`, afterSecs: 1000 + i }));

describe('the budget', () => {
  test('is the time left less the SDK’s margin, at most two minutes', () => {
    const cases: [number | undefined, number][] = [
      [28_800, 120],
      [400, 120],
      [200, 100],
      [100, 50],
      [0, 0],
      [-5, 0],
      [undefined, 60],
    ];
    for (const [left, budget] of cases) {
      assert.equal(budgetSecs(left), budget, String(left));
    }
  });

  test('is the mint’s clock that counts', () => {
    const info = {
      paused: false,
      rotation: { periodSecs: 28_800n, nextAt: 1_000n },
    } as unknown as MintInfo;
    // the mint's clock 10 s ahead of this device's
    assert.equal(leftSecs(info, 10, 900_000), 90);
    assert.equal(leftSecs({ ...info, paused: true }, 0, 900_000), undefined);
    assert.equal(leftSecs({ ...info, rotation: undefined }, 0, 900_000), undefined);
  });
});

describe('settle’s gaps', () => {
  test('fit the budget in whole seconds', () => {
    const cases: [number, number, { minSecs: number; maxSecs: number }][] = [
      [2, 100, { minSecs: 2, maxSecs: 5 }],
      [30, 60, { minSecs: 2, maxSecs: 2 }],
      [40, 60, { minSecs: 1, maxSecs: 1 }],
      [100, 60, { minSecs: 0, maxSecs: 0 }],
      [1, 0, { minSecs: 0, maxSecs: 0 }],
    ];
    for (const [count, budget, want] of cases) {
      const got = spacingFor(count, budget);
      assert.deepEqual(got, want, `${count} in ${budget}`);
      assert.ok(Number.isInteger(got.minSecs) && Number.isInteger(got.maxSecs));
      assert.equal(spaced(got), count === 2, `${count} in ${budget}`);
    }
  });
});

describe('a respaced plan', () => {
  test('starts one to three seconds in, then two to five apart', () => {
    assert.deepEqual(
      respace(plan(3), 120, () => 0).map((each) => each.afterSecs),
      [1, 3, 5],
    );
    assert.deepEqual(
      respace(plan(3), 120, () => 1).map((each) => each.afterSecs),
      [3, 8, 13],
    );
    assert.equal(spacedPlan(respace(plan(3), 120, () => 0)), true);
  });

  test('keeps the SDK’s order and its requests', () => {
    const given = plan(5);
    assert.deepEqual(
      respace(given, 120, rng(1)).map((each) => each.request),
      given.map((each) => each.request),
    );
    assert.deepEqual(respace([], 120), []);
  });

  test('sends everything at once with no time left', () => {
    assert.deepEqual(
      respace(plan(3), 0).map((each) => each.afterSecs),
      [0, 0, 0],
    );
    assert.deepEqual(
      respace(plan(2), -10).map((each) => each.afterSecs),
      [0, 0],
    );
  });

  test('a near rotation squeezes the gaps, never the order', () => {
    const squeezed = respace(plan(10), 10, () => 1);
    assert.ok((squeezed[9]?.afterSecs ?? 0) <= 10);
    assert.equal(spacedPlan(squeezed), false);
  });

  test('never runs backwards, past its budget, or past two minutes', () => {
    const random = rng(42);
    for (let i = 0; i < 500; i++) {
      const n = 1 + Math.floor(random() * 40);
      const budget = Math.floor(random() * 601);
      const times = respace(plan(n), Math.min(budget, budgetSecs(budget)), random).map((each) => each.afterSecs);
      for (let j = 1; j < times.length; j++) {
        assert.ok((times[j] as number) >= (times[j - 1] as number));
      }
      const last = times[times.length - 1] as number;
      assert.ok(last <= budget + 1e-9, `${last} in ${budget}`);
      assert.ok(last <= RUN_MAX_SECS + 1e-9);
    }
  });
});

describe('a note’s bytes', () => {
  test('give its fields', () => {
    const text = noteText({ generation: 0x0102030405060708n, index: 0x0a0b0c0d, amount: 64n });
    assert.equal(text.length, 199);
    assert.deepEqual(decodeNote(text), {
      issuer: ISSUER,
      generation: 0x0102030405060708n,
      index: 0x0a0b0c0d,
      amount: 64n,
      holder: '22'.repeat(32),
    });
  });

  test('that are not a note give nothing', () => {
    assert.equal(decodeNote(noteText({ length: 148 })), undefined);
    assert.equal(decodeNote(noteText({ version: 2 })), undefined);
    assert.equal(decodeNote(noteText({ amount: 0n })), undefined);
    assert.equal(decodeNote(`${noteText().slice(0, 198)}!`), undefined);
    assert.equal(decodeNote(`${noteText().slice(0, 198)}=`), undefined);
    assert.equal(decodeNote(''), undefined);
  });
});

describe('what a transfer spends', () => {
  test('is the notes in its inputs', () => {
    const a = noteText({ generation: 0x0102030405060708n, index: 0x0a0b0c0d });
    const b = noteText({ generation: 9n, index: 3, amount: 4n });
    const spent = spentBy(transfer([a, b]));
    assert.deepEqual(spent, [
      { mint: ISSUER, id: '72623859790382856:168496141', v: 64, generation: 0x0102030405060708n },
      { mint: ISSUER, id: '9:3', v: 4, generation: 9n },
    ]);
    assert.equal(spentKey(spent[1] as (typeof spent)[number]), `${ISSUER}/9:3`);
    assert.equal(outputsOf(transfer([a, b], 3)), 3);
  });

  test('is nothing for what does not read', () => {
    assert.deepEqual(spentBy('{'), []);
    assert.deepEqual(spentBy('null'), []);
    assert.deepEqual(spentBy('{"inputs":{}}'), []);
    assert.deepEqual(spentBy('{"inputs":[{"note":7},{"note":"x"}]}'), []);
    assert.equal(outputsOf('{'), 0);
  });

  test('is a hop when it spends one note, and no send’s', () => {
    const a = noteText({ index: 1 });
    const b = noteText({ index: 2 });
    assert.deepEqual(hopOf(transfer([a]), new Set()), { mint: ISSUER, id: '7:1', v: 64, generation: 7n });
    assert.equal(hopOf(transfer([a]), new Set([`${ISSUER}/7:1`])), undefined);
    assert.equal(hopOf(transfer([a, b]), new Set()), undefined);
    assert.equal(hopOf('{', new Set()), undefined);
  });
});

describe('moving notes', () => {
  test('are this mint’s notes in outgoing transfers that are not sends’', () => {
    const outgoing = [
      transfer([noteText({ index: 1, amount: 64n })]),
      transfer([noteText({ index: 2, amount: 4n, generation: 8n })]),
      // a send's
      transfer([noteText({ index: 3 })]),
      // another mint's
      transfer([noteText({ index: 4, issuer: OTHER })]),
      // unreadable
      '{"inputs":[{"note":"nope"}]}',
      '{',
    ];
    assert.deepEqual(movingNotes(outgoing, new Set([`${ISSUER}/7:3`]), ISSUER), [
      { id: '7:1', v: 64, generation: 7n },
      { id: '8:2', v: 4, generation: 8n },
    ]);
    assert.deepEqual(movingNotes(outgoing, new Set(), OTHER), [{ id: '7:4', v: 64, generation: 7n }]);
  });
});

describe('kept notes', () => {
  test('are the answer notes one generation on, of the same mint and value, each used once', () => {
    const hops = [
      { mint: ISSUER, id: '7:1', v: 64, generation: 7n },
      { mint: ISSUER, id: '7:2', v: 64, generation: 7n },
      { mint: ISSUER, id: '7:3', v: 4, generation: 7n },
    ];
    const answers = [
      noteText({ generation: 8n, index: 10, amount: 64n, issuer: OTHER }),
      noteText({ generation: 9n, index: 11, amount: 64n }),
      noteText({ generation: 8n, index: 12, amount: 64n }),
      noteText({ generation: 8n, index: 13, amount: 64n }),
      'not a note',
      noteText({ generation: 8n, index: 14, amount: 4n }),
    ];
    assert.deepEqual(keptNotes(hops, answers), ['8:12', '8:13', '8:14']);
    // a hop with no answer note matches nothing
    assert.deepEqual(keptNotes(hops.slice(0, 1), answers.slice(0, 2)), []);
    // hop by hop, in the hops' order
    assert.deepEqual(broughtBy(hops, answers), ['8:12', '8:13', '8:14']);
    assert.deepEqual(broughtBy([hops[2], hops[0]].flatMap((hop) => (hop === undefined ? [] : [hop])), answers.slice(0, 3)), [undefined, '8:12']);
  });
});

describe('what a run kept', () => {
  test('counts each token once, though a note resent after a rotation moves on again', () => {
    const resent = { mint: ISSUER, id: '6:1', v: 64, generation: 6n };
    const fading = { mint: ISSUER, id: '7:2', v: 4, generation: 7n };
    // the resend's answer brought 7:101, in what is now the first layer; the run moved it on to 8:102
    const again = { mint: ISSUER, id: '7:101', v: 64, generation: 7n };
    assert.deepEqual(
      keptOf([
        { hop: resent, brought: '7:101' },
        { hop: fading, brought: '8:5' },
        { hop: again, brought: '8:102' },
      ]),
      { amount: 68, serials: ['8:5', '8:102'], spent: [{ id: '6:1', v: 64 }, { id: '7:2', v: 4 }] },
    );
    // a hop whose answer note was not found still counts
    assert.deepEqual(keptOf([{ hop: fading, brought: undefined }]), { amount: 4, serials: [], spent: [{ id: '7:2', v: 4 }] });
  });

  test('joins the rotation’s entry already in the log: both runs, each token once', () => {
    const old = { kind: 'kept', amount: 64, serials: ['8:1'] };
    // another 64 moved later in the same rotation
    assert.deepEqual(mergeKept(old, { kind: 'kept', amount: 64, serials: ['8:2'] }, [{ id: '7:2', v: 64 }]), {
      kind: 'kept',
      amount: 128,
      serials: ['8:1', '8:2'],
    });
    // the later run moved on a note the earlier one brought
    assert.deepEqual(mergeKept(old, { kind: 'kept', amount: 64, serials: ['9:3'] }, [{ id: '8:1', v: 64 }]), {
      kind: 'kept',
      amount: 64,
      serials: ['9:3'],
    });
  });
});

describe('when a mint is due', () => {
  test('is when it has fading notes, is not paused and nothing blocks it', () => {
    assert.equal(due({ fading: 2, moving: 0, paused: false, blocked: false }), true);
    assert.equal(due({ fading: 2, moving: 1, paused: false, blocked: false }), true);
    assert.equal(due({ fading: 0, moving: 2, paused: false, blocked: false }), false);
    assert.equal(due({ fading: 2, moving: 0, paused: true, blocked: false }), false);
    assert.equal(due({ fading: 2, moving: 0, paused: false, blocked: true }), false);
  });

  test('counts only what dies at the next rotation and can move, and a block only at the layer it hit', () => {
    const held = (state: HeldNote['state'], position: number | undefined): HeldNote => ({
      serial: { generation: 5n, index: 0 },
      amount: 1n,
      state,
      position,
      spendableAt: undefined,
      expiresAt: undefined,
    });
    const info = { oldest: 5n, paused: false } as MintInfo;
    const notes = [held('expiring', 0), held('expiring', 0), held('spendable', 1), held('expired', undefined), held('frozen', 0)];
    assert.deepEqual(dueInput(notes, 1, info, undefined), { fading: 2, moving: 1, paused: false, blocked: false });
    assert.equal(dueInput(notes, 0, info, 5n).blocked, true);
    assert.equal(dueInput(notes, 0, info, 4n).blocked, false);
  });
});

describe('the run’s count', () => {
  test('names the transfer under way, and fills a box for each done', () => {
    assert.equal(stepOf({ amount: 128, done: 0, total: 2 }), 1);
    assert.deepEqual(boxes({ amount: 128, done: 0, total: 2 }, 8), [false, false]);
    assert.equal(stepOf({ amount: 128, done: 1, total: 2 }), 2);
    assert.deepEqual(boxes({ amount: 128, done: 1, total: 2 }, 8), [true, false]);
    assert.equal(stepOf({ amount: 128, done: 2, total: 2 }), 2);
    assert.deepEqual(boxes({ amount: 128, done: 2, total: 2 }, 8), [true, true]);
    const many = boxes({ amount: 40, done: 10, total: 40 }, 16);
    assert.equal(many.length, 16);
    assert.equal(many.filter(Boolean).length, 4);
    assert.deepEqual(boxes({ amount: 0, done: 0, total: 0 }, 8), []);
  });
});
