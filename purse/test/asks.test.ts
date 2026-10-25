import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { RECENT_ASK_MS, askAfterTake, endedToDrop, newestOpen, openAsks } from '../src/asks.ts';
import type { AskRecord } from '../src/entries.ts';

const NOW = new Date(2026, 9, 21, 14, 32).getTime();
const MINUTE = 60_000;

let next = 0;
function ask(extra: Partial<AskRecord> = {}): AskRecord {
  const id = String(next++).padStart(64, '0');
  return {
    id,
    mint: 'a'.repeat(64),
    server: 'https://mint.example.org',
    amount: 20,
    notes: [16, 4],
    atMost: undefined,
    json: '{}',
    t: NOW - 5 * MINUTE,
    state: 'open',
    got: 0,
    endedAt: undefined,
    ...extra,
  };
}

describe('requests made', () => {
  test('count what came in, and are taken once it is all they asked', () => {
    const open = ask();
    const part = askAfterTake(open, 4, NOW);
    assert.equal(part.got, 4);
    assert.equal(part.state, 'open');
    assert.equal(part.endedAt, undefined);
    const exact = askAfterTake(part, 16, NOW);
    assert.equal(exact.got, 20);
    assert.equal(exact.state, 'taken');
    assert.equal(exact.endedAt, NOW);
    const over = askAfterTake(ask(), 25, NOW);
    assert.equal(over.got, 25);
    assert.equal(over.state, 'taken');
    const again = askAfterTake(exact, 5, NOW + MINUTE);
    assert.equal(again.got, 25);
    assert.equal(again.endedAt, NOW, 'a request taken once keeps the time it ended');
  });

  test('list the open ones of a mint, newest first', () => {
    const old = ask({ t: NOW - 60 * MINUTE });
    const newer = ask({ t: NOW - MINUTE });
    const other = ask({ mint: 'b'.repeat(64) });
    const taken = ask({ state: 'taken' });
    const cancelled = ask({ state: 'cancelled' });
    assert.deepEqual(
      openAsks([old, other, taken, newer, cancelled], 'a'.repeat(64)).map((each) => each.id),
      [newer.id, old.id],
    );
  });

  test('open on the newest open one only while it is recent', () => {
    const recent = ask({ t: NOW - 29 * MINUTE });
    assert.equal(newestOpen([recent], recent.mint, NOW)?.id, recent.id);
    assert.equal(newestOpen([recent], recent.mint, NOW + 2 * MINUTE), undefined);
    assert.equal(RECENT_ASK_MS, 30 * MINUTE);
    assert.equal(newestOpen([], recent.mint, NOW), undefined);
    assert.equal(newestOpen([ask({ state: 'cancelled', t: NOW })], recent.mint, NOW), undefined);
  });

  test('keep the newest 50 ended ones and every open one', () => {
    const ended = Array.from({ length: 60 }, (_, i) => ask({ state: i % 2 === 0 ? 'taken' : 'cancelled', t: NOW - i * MINUTE, endedAt: NOW - i * MINUTE }));
    const open = Array.from({ length: 70 }, (_, i) => ask({ t: NOW - 1000 * MINUTE - i }));
    const drop = endedToDrop([...open, ...ended], 50);
    assert.equal(drop.length, 10);
    assert.deepEqual(new Set(drop), new Set(ended.slice(50).map((each) => each.id)));
    assert.deepEqual(endedToDrop(open, 50), []);
  });
});
