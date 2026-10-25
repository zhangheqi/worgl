import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import type { AskRecord, LogEntry } from '../src/entries.ts';
import { type RowContext, byTime, dayName, effectOf, footOf, footWords, register, rowWords } from '../src/register.ts';

const MINT = 'ab'.repeat(32);
const OTHER = 'cd'.repeat(32);
// Wed 21 Oct 2026, local time
const at = (day: number, hour: number, minute = 0): number => new Date(2026, 9, day, hour, minute).getTime();
const NOW = at(21, 14, 32);

let id = 0;
const entry = (more: Partial<LogEntry> & Pick<LogEntry, 'kind' | 't'>): LogEntry => ({
  id: ++id,
  mint: MINT,
  amount: 0,
  serials: [],
  ...more,
});

const ctx = (more: Partial<RowContext> = {}): RowContext => ({ kept: false, ask: undefined, host: 'playground.worgl.1y.io', now: NOW, ...more });

describe('what an entry changed', () => {
  test('is what came in or went, and nothing for keep-alive or a send turned away', () => {
    assert.equal(effectOf(entry({ kind: 'faucet', t: 0, amount: 64 })), 64);
    assert.equal(effectOf(entry({ kind: 'received', t: 0, amount: 5 })), 5);
    assert.equal(effectOf(entry({ kind: 'faded', t: 0, amount: 4 })), -4);
    assert.equal(effectOf(entry({ kind: 'lost', t: 0, amount: 8 })), -8);
    assert.equal(effectOf(entry({ kind: 'kept', t: 0, amount: 128 })), 0);
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 5, asked: 5, state: 'turned' })), 0);
  });

  test('for a send, is what it asked while on its way, then what went', () => {
    // the total counts the change on its way and leaves out theirs: a send on its way has taken what it asks
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 5, asked: 5, state: 'sending' })), -5);
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 5, asked: 5, state: 'waiting' })), -5);
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 5, asked: 5, went: 5, back: 59, state: 'handback' })), -5);
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 5, asked: 5, state: 'done' })), -5);
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 20, asked: 20, went: 16, state: 'partial' })), -16);
    // part of a send whose answer gave no value: not known
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 20, asked: 20, state: 'partial' })), undefined);
  });

  test('for a lost send, is what the dropped transfers spent and what the answered ones paid them', () => {
    // one transfer, dropped: its note, theirs and the change together
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 5, asked: 5, lost: 64, lostTheirs: 5, state: 'lost' })), -64);
    // two: 16 answered, and a note of 8 for their 4 dropped
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 20, asked: 20, lost: 8, lostTheirs: 4, state: 'lost' })), -24);
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 16, asked: 20, went: 16, lost: 8, lostTheirs: 4, state: 'lost' })), -24);
  });

  test('for a send part of which was turned away, counts nothing of that part, as its note came back', () => {
    // three transfers: 8 answered, one for their 4 dropped with a note of 8, one for their 4 turned away
    const mixed = { kind: 'sent' as const, t: 0, amount: 16, asked: 16, lost: 8, lostTheirs: 4, dropped: 1, turnedAway: 1 };
    assert.equal(effectOf(entry({ ...mixed, went: 8, state: 'lost' })), -16);
    // without what went, and while the rest waits, what came back of it is not known
    assert.equal(effectOf(entry({ ...mixed, state: 'lost' })), undefined);
    assert.equal(effectOf(entry({ ...mixed, state: 'waiting' })), undefined);
    // turned away whole: every note came back, and nothing changed (never -0)
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 5, asked: 5, transfers: 1, turnedAway: 1, state: 'turned' })), 0);
  });

  test('for a send the mint dropped part of while the rest waits, counts the dropped notes from then on', () => {
    // two transfers for their 4 and 16: the note of 64 for the 4 is dropped, the 16 waits
    const waiting = { kind: 'sent' as const, t: 0, amount: 20, asked: 20, lost: 64, lostTheirs: 4, dropped: 1 };
    assert.equal(effectOf(entry({ ...waiting, state: 'waiting' })), -80);
    // the 16 answered later: lost, with what went
    assert.equal(effectOf(entry({ ...waiting, went: 16, state: 'lost' })), -80);
  });

  test('for a send whose end the purse did not see in full, is not known', () => {
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 20, asked: 20, transfers: 2, blind: true, state: 'done' })), undefined);
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 16, asked: 20, went: 16, transfers: 2, blind: true, state: 'partial' })), undefined);
    // while it is still on its way, it has taken what it asks, seen or not
    assert.equal(effectOf(entry({ kind: 'sent', t: 0, amount: 20, asked: 20, transfers: 2, blind: true, state: 'sending' })), -20);
  });
});

describe('the register', () => {
  test('is by day, newest first, by when each happened rather than when it was logged', () => {
    const log = [
      // a fade logged on the open after it, at the rotation's time
      entry({ kind: 'faded', t: at(20, 16), amount: 4 }),
      entry({ kind: 'faucet', t: at(21, 9, 10), amount: 64 }),
      entry({ kind: 'received', t: at(20, 11, 20), amount: 5 }),
    ].reverse();
    assert.deepEqual(
      byTime(log).map((each) => each.kind),
      ['faucet', 'faded', 'received'],
    );
    const days = register(log, MINT, 65, NOW);
    assert.deepEqual(
      days.map((day) => [day.name, day.rows.map((row) => row.entry.kind)]),
      [
        ['Wednesday 21 October', ['faucet']],
        ['Tuesday 20 October', ['faded', 'received']],
      ],
    );
  });

  test('works what was held after each row back from what is held now', () => {
    const log = byTime([
      entry({ kind: 'received', t: at(20, 11, 20), amount: 5 }),
      entry({ kind: 'faucet', t: at(21, 9, 10), amount: 64 }),
      entry({ kind: 'received', t: at(21, 10, 5), amount: 64 }),
      entry({ kind: 'sent', t: at(21, 14, 2), amount: 5, asked: 5, went: 5, back: 59, state: 'handback' }),
      entry({ kind: 'kept', t: at(21, 14, 10), amount: 64 }),
      entry({ kind: 'faucet', t: at(21, 14, 20), amount: 64 }),
    ]);
    const held = register(log, MINT, 192, NOW).flatMap((day) => day.rows.map((row) => row.held));
    assert.deepEqual(held, [192, 128, 128, 133, 69, 5]);
  });

  test('shows the other mints’ entries not at all', () => {
    const log = [entry({ kind: 'faucet', t: at(21, 9), amount: 64 }), entry({ kind: 'faucet', t: at(21, 10), amount: 32, mint: OTHER })];
    const rows = register(log, MINT, 64, NOW).flatMap((day) => day.rows);
    assert.deepEqual(
      rows.map((row) => [row.entry.amount, row.held]),
      [[64, 64]],
    );
  });

  test('leaves Held blank before a row whose change is not known, before the first read, and where it would go below nothing', () => {
    const log = byTime([
      entry({ kind: 'faucet', t: at(21, 9), amount: 64 }),
      entry({ kind: 'sent', t: at(21, 10), amount: 20, asked: 20, state: 'partial' }),
      entry({ kind: 'faucet', t: at(21, 11), amount: 64 }),
    ]);
    assert.deepEqual(
      register(log, MINT, 100, NOW).flatMap((day) => day.rows.map((row) => row.held)),
      [100, 36, undefined],
    );
    assert.deepEqual(
      register(log, MINT, undefined, NOW).flatMap((day) => day.rows.map((row) => row.held)),
      [undefined, undefined, undefined],
    );
    // the log lacks an entry (one that could not be written): no number below nothing
    const short = byTime([entry({ kind: 'faucet', t: at(21, 9), amount: 64 }), entry({ kind: 'faucet', t: at(21, 11), amount: 64 })]);
    assert.deepEqual(
      register(short, MINT, 100, NOW).flatMap((day) => day.rows.map((row) => row.held)),
      [100, 36],
    );
    assert.deepEqual(
      register(short, MINT, 32, NOW).flatMap((day) => day.rows.map((row) => row.held)),
      [32, undefined],
    );
  });

  test('names a day of another year with its year', () => {
    assert.equal(dayName(new Date(2025, 11, 31, 9).getTime(), NOW), 'Wednesday 31 December 2025');
  });
});

describe('the foot', () => {
  test('sums what faded and was lost since the oldest entry kept', () => {
    const log = [
      entry({ kind: 'faded', t: at(20, 16), amount: 5 }),
      entry({ kind: 'lost', t: at(21, 8), amount: 8 }),
      entry({ kind: 'sent', t: at(21, 9), amount: 5, asked: 5, lost: 64, lostTheirs: 5, state: 'lost' }),
      entry({ kind: 'faucet', t: at(19, 9), amount: 64 }),
      entry({ kind: 'faded', t: at(21, 8), amount: 100, mint: OTHER }),
    ];
    const foot = footOf(log, MINT);
    assert.deepEqual(foot, { faded: 5, lost: 72, since: at(19, 9) });
    assert.deepEqual(footWords(foot, NOW), {
      lab: 'Faded · 5 tokens',
      body: 'Since Mon 19 Oct, 5 tokens faded: they sat in the first layer when the mint rotated. 72 tokens were lost on transfers whose answers the mint no longer kept.',
    });
  });

  test('says when nothing faded, and when there is nothing yet', () => {
    const foot = footOf([entry({ kind: 'faucet', t: at(21, 9), amount: 64 })], MINT);
    assert.deepEqual(footWords(foot, NOW), { lab: 'Nothing has faded', body: 'Since 09:00, nothing here has faded.' });
    assert.equal(footWords(footOf([], MINT), NOW).lab, 'Nothing yet');
  });

  test('counts what the mint dropped of a send whose rest still waits', () => {
    const log = [entry({ kind: 'sent', t: at(21, 9), amount: 20, asked: 20, lost: 64, lostTheirs: 4, dropped: 1, transfers: 2, state: 'waiting' })];
    assert.equal(footOf(log, MINT).lost, 64);
    // a send that waits with nothing dropped has lost nothing yet
    assert.equal(footOf([entry({ kind: 'sent', t: at(21, 9), amount: 20, asked: 20, state: 'waiting' })], MINT).lost, 0);
  });
});

describe('a row’s words', () => {
  test('say how a take came, in notes, when the entry has their values', () => {
    const took = rowWords(entry({ kind: 'faucet', t: at(21, 9), amount: 64, serials: ['7:1'], values: [64] }), ctx());
    assert.equal(took.title, 'Took 64 from the faucet');
    assert.equal(took.sub, 'One note');
    assert.equal(took.delta, 64);
    assert.deepEqual(took.detail, ['One note of 64, from the faucet at playground.worgl.1y.io.']);
    // an entry from before the purse kept values says the tokens
    const older = rowWords(entry({ kind: 'faucet', t: at(21, 9), amount: 64, serials: ['7:1'] }), ctx());
    assert.equal(older.sub, undefined);
    assert.deepEqual(older.detail, ['64 tokens, from the faucet at playground.worgl.1y.io.']);
  });

  test('say what a take of a hand-back answered', () => {
    const ask: AskRecord = {
      id: 'ee'.repeat(32),
      mint: MINT,
      server: 'https://playground.worgl.1y.io',
      amount: 5,
      notes: [4, 1],
      atMost: undefined,
      json: '{}',
      t: at(21, 10),
      state: 'taken',
      got: 5,
      endedAt: at(21, 10, 5),
    };
    const words = rowWords(entry({ kind: 'received', t: at(21, 10, 5), amount: 5, ask: ask.id, serials: ['8:1', '8:2'], values: [1, 4] }), ctx({ ask }));
    assert.equal(words.title, 'Received 5');
    assert.equal(words.sub, 'As 4 + 1');
    assert.deepEqual(words.detail, [
      'Notes of 4 and 1, taken in from their hand-back for your request for 5, made at 10:00. That request is taken in full.',
      'Each note went to a key this purse made for that request, so only this purse could take them in.',
    ]);
    const part = rowWords(entry({ kind: 'received', t: at(21, 10, 5), amount: 4, ask: ask.id, values: [4] }), ctx({ ask: { ...ask, state: 'open', got: 4 } }));
    assert.match(part.detail[0] ?? '', /4 of its 5 have come so far\.$/);
    assert.equal(part.detail[1], 'The note went to a key this purse made for that request, so only this purse could take it in.');
    const one = rowWords(entry({ kind: 'received', t: at(21, 10, 5), amount: 1, ask: ask.id, values: [1] }), ctx({ ask: { ...ask, state: 'open', got: 1 } }));
    assert.match(one.detail[0] ?? '', /1 of its 5 has come so far\.$/);
    // a request this purse no longer keeps
    assert.deepEqual(rowWords(entry({ kind: 'received', t: 0, amount: 64, serials: ['8:3'], values: [64] }), ctx()).detail, [
      'One note of 64, taken in from their hand-back.',
      'The note went to a key this purse made for your request, so only this purse could take it in.',
    ]);
  });

  test('say what keep-alive moved, how, and that the mint can tell', () => {
    const words = rowWords(entry({ kind: 'kept', t: at(21, 14, 10), amount: 5, serials: ['9:1', '9:2'], values: [4, 1] }), ctx());
    assert.equal(words.title, 'Kept 5 alive');
    assert.equal(words.sub, 'As 4 + 1 · one layer on');
    assert.equal(words.delta, 0);
    assert.deepEqual(words.detail, [
      '5 tokens sat in the first layer and would have faded at the mint’s next rotation. The purse moved each of the two notes one layer on, in a transfer of its own, to a fresh key.',
      'The mint can tell which notes one visit moved.',
    ]);
    assert.equal(rowWords(entry({ kind: 'kept', t: 0, amount: 64, serials: ['9:1'] }), ctx()).sub, 'One note · one layer on');
    const eight = entry({ kind: 'kept', t: 0, amount: 36, serials: [], values: [8, 8, 4, 4, 4, 4, 2, 2] });
    assert.equal(rowWords(eight, ctx()).sub, 'Eight notes · one layer on');
    // a run whose new notes were not found among the answers: no count to say
    const unfound = rowWords(entry({ kind: 'kept', t: 0, amount: 1, serials: [], values: [] }), ctx());
    assert.equal(unfound.sub, 'One layer on');
    assert.match(unfound.detail[0] ?? '', /The purse moved it one layer on, in a transfer of its own, to a fresh key\.$/);
  });

  test('say what faded, and when the mint rotated', () => {
    const words = rowWords(entry({ kind: 'faded', t: at(21, 8), amount: 5, gone: [1, 4] }), ctx());
    assert.equal(words.title, 'Faded 5');
    assert.equal(words.sub, 'As 4 + 1 · in the first layer');
    assert.equal(words.delta, -5);
    assert.equal(words.detail[0], 'Notes of 4 and 1 sat in the first layer when the mint rotated at 08:00, so they faded with it.');
    assert.equal(rowWords(entry({ kind: 'faded', t: at(21, 8), amount: 64, gone: [64] }), ctx()).detail[0]?.endsWith('so it faded with it.'), true);
    // a mint rotated by hand, and one with no schedule: the time is only a bound
    const after = rowWords(entry({ kind: 'faded', t: at(21, 8), amount: 64, gone: [64], fadeAt: 'after' }), ctx());
    assert.equal(after.detail[0], 'One note of 64 sat in the first layer when the mint rotated, some time after 08:00, so it faded with it.');
    const by = rowWords(entry({ kind: 'faded', t: at(21, 8), amount: 64, gone: [64], fadeAt: 'by' }), ctx());
    assert.equal(by.detail[0], 'One note of 64 sat in the first layer when the mint rotated, at 08:00 or before, so it faded with it.');
  });

  test('say what a send spent, what went and came back, and offer its hand-back while it is kept', () => {
    const sent = entry({
      kind: 'sent',
      t: at(21, 14, 2),
      amount: 5,
      asked: 5,
      went: 5,
      back: 59,
      transfers: 1,
      spent: ['7:3'],
      spentValues: [64],
      ask: 'ff'.repeat(32),
      state: 'handback',
    });
    const words = rowWords(sent, ctx({ kept: true }));
    assert.equal(words.title, 'Sent 5');
    assert.equal(words.sub, 'From your note of 64 · 59 back');
    assert.equal(words.delta, -5);
    assert.deepEqual(words.detail, [
      'One transfer, from your note of 64.',
      '5 went to them and 59 came back to you. Whether they have taken it in, this purse cannot see.',
      'You can show the hand-back again until their tokens fade.',
    ]);
    assert.equal(words.opens, 'handback');
    assert.equal(words.action, 'Show the hand-back again');
    // once the hand-back is gone: nothing to open
    const gone = rowWords(sent, ctx());
    assert.equal(gone.opens, undefined);
    assert.equal(gone.detail[2], 'Their hand-back is no longer kept: its tokens have faded, taken in or not.');
    // before the purse has read what it keeps: nothing said of the hand-back either way
    assert.equal(rowWords(sent, ctx({ kept: undefined })).detail.length, 2);
    // settled on a later open: when the mint answered
    const late = rowWords({ ...sent, t2: at(21, 16, 30) }, ctx({ kept: true }));
    assert.equal(late.detail[1], '5 went to them and 59 came back to you, when the mint answered at 16:30. Whether they have taken it in, this purse cannot see.');
  });

  test('say a several-note send’s transfers, and fall back to their count without values', () => {
    const words = rowWords(
      entry({ kind: 'sent', t: at(21, 14), amount: 69, asked: 69, went: 69, back: 0, transfers: 2, spent: ['7:1', '7:2'], state: 'done' }),
      ctx(),
    );
    assert.equal(words.sub, 'Two transfers · nothing back');
    assert.equal(words.detail[0], 'Two transfers, one per note.');
    assert.equal(words.detail[1], '69 went to them, and nothing came back. Whether they have taken it in, this purse cannot see.');
    // what came back not known: no claim either way
    const unknown = rowWords(entry({ kind: 'sent', t: at(21, 14), amount: 5, asked: 5, transfers: 1, spentValues: [64], state: 'done' }), ctx());
    assert.equal(unknown.sub, 'From your note of 64');
    assert.equal(unknown.detail[1], '5 went to them. Whether they have taken it in, this purse cannot see.');
  });

  test('say what became of a send turned away, lost or still on its way, and offer to show it', () => {
    const turned = rowWords(entry({ kind: 'sent', t: 0, amount: 5, asked: 5, transfers: 1, state: 'turned', layerFull: true }), ctx());
    assert.equal(turned.title, 'Turned away: 5');
    assert.equal(turned.sub, 'Nothing went · your note is back');
    assert.equal(turned.detail[1], 'The mint turned it away because its newest layer was full, so your note is back with you and nothing went.');
    assert.equal(turned.delta, 0);
    const lost = rowWords(entry({ kind: 'sent', t: 0, amount: 5, asked: 5, transfers: 1, lost: 64, lostTheirs: 5, state: 'lost' }), ctx());
    assert.equal(lost.opens, 'lost');
    assert.equal(lost.action, 'Show what happened');
    assert.equal(lost.delta, -64);
    assert.equal(lost.sub, 'The mint no longer kept its answer');
    assert.equal(lost.detail[1], 'The mint no longer keeps the answer to it, so the note it spent is gone: the 5 for them and your change, 64 in all.');
    const waiting = rowWords(entry({ kind: 'sent', t: 0, amount: 5, asked: 5, transfers: 1, state: 'waiting' }), ctx());
    assert.equal(waiting.sub, 'Waiting for the mint');
    assert.equal(waiting.action, 'Show the send');
  });

  test('say what part of a lost send went, with a change that is the Held column’s step', () => {
    const part = entry({ kind: 'sent', t: 0, amount: 16, asked: 20, went: 16, back: 48, lost: 8, lostTheirs: 4, dropped: 1, transfers: 2, state: 'lost' });
    const words = rowWords(part, ctx());
    assert.equal(words.title, 'Sent 16 of 20');
    assert.equal(words.sub, '8 lost · the mint no longer kept an answer');
    assert.equal(words.delta, -24);
    assert.deepEqual(words.detail.slice(1), [
      '16 of 20 went to them and 48 came back to you.',
      'The mint no longer keeps the answer to one of its transfers, so the note it spent is gone: the 4 for them and your change, 8 in all.',
      'Their hand-back is no longer kept: its tokens have faded, taken in or not.',
    ]);
    // while its hand-back is kept: what it carries
    assert.equal(
      rowWords(part, ctx({ kept: true })).detail[3],
      'Their hand-back carries their 16. Ask them to make a new request for the other 4.',
    );
  });

  test('say how many of a lost send’s transfers the mint dropped, and what of it was theirs', () => {
    // two of three dropped, on notes that held just what was theirs
    const two = rowWords(
      entry({ kind: 'sent', t: 0, amount: 24, asked: 24, lost: 16, lostTheirs: 16, dropped: 2, transfers: 3, state: 'lost' }),
      ctx(),
    );
    assert.equal(two.title, 'Sent 8 of 24');
    assert.equal(two.sub, '16 lost · the mint no longer kept some answers');
    assert.equal(two.detail[1], '8 of 24 went to them.');
    assert.equal(two.detail[2], 'The mint no longer keeps the answers to two of its transfers, so the two notes they spent are gone: the 16 for them.');
    // one answered, one dropped and one turned away: what went is only what the entry says
    const mixed = rowWords(
      entry({ kind: 'sent', t: 0, amount: 8, asked: 16, went: 8, lost: 8, lostTheirs: 4, dropped: 1, turnedAway: 1, transfers: 3, state: 'lost' }),
      ctx(),
    );
    assert.equal(mixed.delta, -16);
    assert.deepEqual(mixed.detail.slice(1), [
      '8 of 16 went to them.',
      'The mint no longer keeps the answer to one of its transfers, so the note it spent is gone: the 4 for them and your change, 8 in all.',
      'The mint turned one of its transfers away, and its note is back with you.',
      'Their hand-back is no longer kept: its tokens have faded, taken in or not.',
    ]);
    const unsaid = rowWords(entry({ kind: 'sent', t: 0, amount: 16, asked: 16, lost: 8, lostTheirs: 4, dropped: 1, turnedAway: 1, transfers: 3, state: 'lost' }), ctx());
    assert.equal(unsaid.delta, undefined);
    assert.equal(unsaid.detail.length, 3);
  });

  test('say when the purse did not see what became of all of a send', () => {
    const blind = 'The purse did not see what the mint made of all of its transfers, so what it changed is not known.';
    const done = rowWords(entry({ kind: 'sent', t: 0, amount: 20, asked: 20, transfers: 2, blind: true, state: 'done' }), ctx({ kept: undefined }));
    assert.equal(done.delta, undefined);
    assert.deepEqual(done.detail, ['Two transfers, one per note.', blind]);
    const part = rowWords(entry({ kind: 'sent', t: 0, amount: 16, asked: 20, went: 16, transfers: 2, blind: true, state: 'partial' }), ctx({ kept: undefined }));
    assert.equal(part.sub, '16 of 20 went');
    assert.deepEqual(part.detail, ['Two transfers, one per note.', '16 of 20 went to them.', blind]);
  });

  test('say what the mint dropped of a send whose rest still waits', () => {
    const words = rowWords(
      entry({ kind: 'sent', t: 0, amount: 20, asked: 20, lost: 64, lostTheirs: 4, dropped: 1, transfers: 2, state: 'waiting' }),
      ctx(),
    );
    assert.equal(words.delta, -80);
    assert.equal(
      words.detail[1],
      'The mint no longer keeps the answer to one of its transfers, so the note it spent is gone: the 4 for them and your change, 64 in all. The rest is not lost: the mint has not answered it yet, and this purse sends it again when it can.',
    );
  });

  test('say what the mint turned away of a send whose rest still waits', () => {
    const words = rowWords(entry({ kind: 'sent', t: 0, amount: 20, asked: 20, turnedAway: 1, transfers: 2, state: 'waiting' }), ctx());
    assert.equal(
      words.detail[1],
      'The mint turned one of its transfers away, and its note is back with you. The rest is not lost: the mint has not answered it yet, and this purse sends it again when it can.',
    );
  });

  test('say what a part-sent hand-back carries, naming it', () => {
    const words = rowWords(
      entry({ kind: 'sent', t: 0, amount: 16, asked: 20, went: 16, back: 48, turnedAway: 1, transfers: 2, state: 'partial' }),
      ctx({ kept: true }),
    );
    assert.equal(words.detail[1], '16 of 20 went to them and 48 came back to you. The mint turned the rest away, and its note is back with you.');
    assert.equal(words.detail[2], 'Their hand-back carries their 16. Ask them to make a new request for the other 4.');
    const two = rowWords(entry({ kind: 'sent', t: 0, amount: 8, asked: 24, went: 8, turnedAway: 2, transfers: 3, state: 'partial' }), ctx());
    assert.match(two.detail[1] ?? '', /The mint turned the rest away, and their notes are back with you\.$/);
    // what went not known: no change to show
    assert.equal(rowWords(entry({ kind: 'sent', t: 0, amount: 20, asked: 20, transfers: 2, state: 'partial' }), ctx()).delta, undefined);
  });
});
