import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { ASK_CAP, parseAmount } from '../src/amount.ts';

describe('an amount to ask for', () => {
  test('reads whole numbers, with spaces, leading zeros or full-width digits', () => {
    for (const [text, amount] of [
      ['20', 20],
      [' 20 ', 20],
      ['00012', 12],
      ['２０', 20],
      ['1', 1],
      ['4096', 4096],
    ] as const) {
      assert.deepEqual(parseAmount(text), { ok: true, amount }, text);
    }
  });

  test('says what is wrong with anything else', () => {
    assert.deepEqual(parseAmount(''), { ok: false, problem: 'empty' });
    assert.deepEqual(parseAmount('   '), { ok: false, problem: 'empty' });
    for (const text of ['2.5', '1e3', '-3', '20 tokens', '+4', '0x10']) {
      assert.deepEqual(parseAmount(text), { ok: false, problem: 'not_whole' }, text);
    }
    assert.deepEqual(parseAmount('0'), { ok: false, problem: 'zero' });
    assert.deepEqual(parseAmount('000'), { ok: false, problem: 'zero' });
    assert.deepEqual(parseAmount('4097'), { ok: false, problem: 'too_big' });
    assert.deepEqual(parseAmount('99999999999999999999'), { ok: false, problem: 'too_big' });
  });

  test('ask at most 4096 at a time, or another cap', () => {
    assert.equal(ASK_CAP, 4096);
    assert.deepEqual(parseAmount('10', 8), { ok: false, problem: 'too_big' });
  });
});
