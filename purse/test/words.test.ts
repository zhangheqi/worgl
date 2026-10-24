import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { type Holdings, type Note, comeBackAt } from '../src/holdings.ts';
import {
  caption,
  comeBackHeading,
  comeBackRow,
  fieldLabel,
  firstToGo,
  laneDescription,
  laneTip,
  legend,
  noteTip,
  numberDescription,
  plain,
  rotatedWords,
  sentence,
  tipWords,
  tookWords,
} from '../src/words.ts';

// The prototype's afternoon: Wed 14:32 on a mint of four layers that rotates
// every eight hours, at 00:00, 08:00 and 16:00.
const HOUR = 3_600_000;
const at = (day: number, hours: number, minutes = 0): number => new Date(2026, 9, day, hours, minutes).getTime();
const NOW = at(21, 14, 32);
const NEXT = at(21, 16);

let serial = 0;
/** A note at layer `pos`, which fades `pos + 1` rotations from now. */
function note(v: number, pos: number, extra: Partial<Note> = {}): Note {
  return {
    id: `7:${serial++}`,
    v,
    pos,
    expiresAt: NEXT + pos * 8 * HOUR,
    readyAt: pos === 3 ? NEXT : undefined,
    arrivedAt: undefined,
    source: undefined,
    ...extra,
  };
}

function holdings(notes: Note[], more: Partial<Holdings> = {}): Holdings {
  const ordered = [...notes].sort((a, b) => a.pos - b.pos);
  const first = ordered[0];
  return {
    layers: 4,
    notes,
    total: notes.reduce((t, n) => t + n.v, 0),
    schedule: { startMs: NEXT - 8 * HOUR, nextMs: NEXT, periodMs: 8 * HOUR },
    paused: false,
    firstExpiry: first?.expiresAt,
    ...more,
  };
}

const READY = (): Note[] => [note(64, 2), note(4, 2), note(1, 2)];
const RESTING = (): Note[] => [64, 32, 16, 8, 2, 1].map((v) => note(v, 3));
const FADING = (): Note[] => [note(4, 0), note(1, 0)];

describe('the sentence', () => {
  test('says what is ready, what rests until when, and when the first fades', () => {
    const h = holdings([...READY(), ...RESTING()]);
    assert.equal(plain(sentence(h, NOW)), '69 are ready; 123 rest until 16:00. Nothing fades before Thu 08:00.');
    assert.deepEqual(sentence(h, NOW).filter((part) => typeof part !== 'string'), [
      { n: 69, lane: 'ready' },
      { n: 123, lane: 'rest' },
    ]);
  });

  test('says it in the singular for one token', () => {
    const h = holdings([note(1, 2), note(1, 3)]);
    assert.equal(plain(sentence(h, NOW)), '1 is ready; 1 rests until 16:00. Nothing fades before Thu 08:00.');
  });

  test('says when nothing is ready', () => {
    const h = holdings([note(64, 3)]);
    assert.equal(plain(sentence(h, NOW)), 'Nothing is ready; 64 rest until 16:00. Nothing fades before Thu 16:00.');
  });

  test('says what fades, and that nothing keeps it alive yet', () => {
    const h = holdings([note(64, 1), ...RESTING(), ...FADING()]);
    assert.equal(
      plain(sentence(h, NOW)),
      '64 are ready; 123 rest until 16:00; 5 fade then unless they move.',
    );
    const alone = holdings([note(1, 0)]);
    assert.equal(plain(sentence(alone, NOW)), 'Nothing is ready; 1 fades at 16:00 unless it moves.');
  });

  test('with keep-alive, says the purse keeps them alive', () => {
    const h = holdings([note(64, 1), ...RESTING(), ...FADING()]);
    const parts = sentence(h, NOW, true);
    assert.equal(
      plain(parts),
      '64 are ready; 123 rest until 16:00; 5 would fade then, so this purse keeps them alive while it is open.',
    );
    assert.ok(parts.some((part) => typeof part === 'object' && 'em' in part && part.em === 'alive'));
  });

  test('says when the purse is empty', () => {
    assert.equal(plain(sentence(holdings([]), NOW)), 'This purse is empty.');
  });

  test('names the next rotation as a weekday once it is not today', () => {
    const late = at(21, 23, 10);
    const next = at(22, 0);
    const h = holdings([note(8, 3, { expiresAt: next + 24 * HOUR })], {
      schedule: { startMs: next - 8 * HOUR, nextMs: next, periodMs: 8 * HOUR },
    });
    assert.equal(plain(sentence(h, late)), 'Nothing is ready; 8 rest until Thu 00:00. Nothing fades before Fri 00:00.');
  });

  test('says when the mint has no schedule or is paused', () => {
    const hand = holdings([note(8, 2)], { schedule: undefined, firstExpiry: undefined });
    assert.equal(plain(sentence(hand, NOW)), '8 are ready. Nothing fades until the mint rotates.');
    const paused = holdings([note(8, 2)], { schedule: undefined, firstExpiry: undefined, paused: true });
    assert.equal(plain(sentence(paused, NOW)), '8 are ready. The mint is paused, so nothing fades until it resumes.');
  });
});

describe('the come-back date', () => {
  test('is an hour before the first expiry', () => {
    const h = holdings([...READY(), ...RESTING()]);
    assert.equal(comeBackAt(h, NOW), at(22, 7));
    assert.deepEqual(comeBackRow(h, NOW, true), { lead: 'Open again before ', date: 'Thu 07:00' });
    const fading = holdings([...FADING()]);
    assert.deepEqual(comeBackRow(fading, NOW, true), { lead: 'Open again before ', date: '15:00' });
  });

  test('keeps a quarter period on a mint that rotates every few minutes', () => {
    const start = at(21, 14, 30);
    const h = holdings([note(64, 3, { expiresAt: start + 20 * 60_000 })], {
      schedule: { startMs: start, nextMs: start + 5 * 60_000, periodMs: 5 * 60_000 },
      firstExpiry: start + 20 * 60_000,
    });
    assert.equal(comeBackAt(h, start), start + 20 * 60_000 - 75_000);
  });

  test('is the first expiry itself once less than the margin is left, never a time gone', () => {
    // the playground's last hour: fading notes go at 16:00, and it is 15:30
    const h = holdings([...FADING()]);
    const late = at(21, 15, 30);
    assert.equal(comeBackAt(h, late), NEXT);
    assert.deepEqual(comeBackRow(h, late, true), { lead: 'Open again before ', date: '16:00' });
    assert.equal(comeBackAt(h, at(21, 14, 59)), at(21, 15));
    assert.equal(comeBackAt(h, at(21, 15)), NEXT);
    // the rotation has come, and the read after it has not
    assert.equal(comeBackAt(h, NEXT), undefined);
    assert.deepEqual(comeBackRow(h, NEXT + 1000), { none: 'The mint is rotating now.' });
  });

  test('is when the first tokens fade until keep-alive comes', () => {
    assert.deepEqual(comeBackRow(holdings([...READY(), ...RESTING()]), NOW), {
      lead: 'First tokens fade at ',
      date: 'Thu 08:00',
    });
    assert.deepEqual(comeBackRow(holdings([...FADING()]), at(21, 15, 30)), { lead: 'First tokens fade at ', date: '16:00' });
    assert.deepEqual(comeBackRow(holdings([...FADING()]), NEXT + 1000), { none: 'The mint is rotating now.' });
    assert.equal(comeBackHeading(), 'First to fade');
    assert.equal(comeBackHeading(true), 'Open again before');
  });

  test('is none with nothing held, no schedule or a paused mint', () => {
    assert.deepEqual(comeBackRow(holdings([]), NOW), { none: 'Nothing to fade yet.' });
    assert.deepEqual(comeBackRow(holdings([]), NOW, true), { none: 'Nothing to keep alive yet.' });
    assert.deepEqual(comeBackRow(holdings([note(4, 2)], { schedule: undefined, firstExpiry: undefined }), NOW), {
      none: 'This mint rotates only by hand.',
    });
    assert.deepEqual(comeBackRow(holdings([note(4, 2)], { paused: true, schedule: undefined }), NOW), {
      none: 'This mint is paused.',
    });
    assert.equal(comeBackAt(holdings([]), NOW), undefined);
  });

  test('says on desktop which tokens go first', () => {
    assert.equal(
      firstToGo(holdings([...READY(), ...RESTING()]), NOW),
      'Your 69 ready tokens go then unless they move.',
    );
    assert.equal(firstToGo(holdings([note(1, 3)]), NOW), 'Your 1 resting token goes then unless it moves.');
    assert.equal(
      firstToGo(holdings([...READY(), ...FADING()]), NOW, true),
      'The purse keeps tokens alive only while it is open. Left closed, your 5 fading tokens go at 16:00.',
    );
    assert.equal(firstToGo(holdings([]), NOW), 'Nothing to fade yet.');
    assert.equal(firstToGo(holdings([]), NOW, true), 'Nothing to keep alive yet.');
  });
});

describe('the tips', () => {
  test('of the resting lane say what rests and when it is ready', () => {
    const h = holdings([...READY(), ...RESTING()]);
    assert.deepEqual(laneTip(h, 'rest', NOW), {
      head: 'Resting · 123',
      lines: [
        'Notes of 64, 32, 16, 8, 2 and 1 in the last layer. Notes that just moved rest here.',
        'Ready at 16:00. They fade Thu 16:00 unless they move.',
      ],
    });
    assert.equal(laneTip(h, 'rest', NOW, true).lines[1], 'Ready at 16:00. If this purse stays closed, they fade Thu 16:00.');
    assert.deepEqual(laneTip(holdings(READY()), 'rest', NOW).lines, [
      'Nothing rests in the last layer.',
      'What you take in, and change from a send, usually rests here until the next rotation.',
    ]);
  });

  test('of the fading lane say when they fade, and that the purse keeps them alive only once it does', () => {
    const h = holdings([note(64, 1), ...FADING()]);
    assert.deepEqual(laneTip(h, 'fade', NOW), {
      head: 'Fading · 5',
      lines: ['Notes of 4 and 1 in the first layer. They fade at 16:00 unless they move.'],
    });
    assert.equal(
      laneTip(h, 'fade', NOW, true).lines[1],
      'While this purse is open, it keeps them alive: one transfer at a time, it moves them a layer further from the first, down the field.',
    );
    assert.deepEqual(laneTip(holdings(READY()), 'fade', NOW).lines, ['Nothing is in the first layer.']);
  });

  test('of the ready lane say what can move and when it fades', () => {
    const h = holdings([...READY(), ...RESTING()]);
    assert.deepEqual(laneTip(h, 'ready', NOW), {
      head: 'Ready · 69',
      lines: ['Notes of 64, 4 and 1. Sends use them.', 'They can move now. They fade Thu 08:00 unless they move.'],
    });
    assert.equal(laneTip(h, 'ready', NOW, true).lines[1], 'They can move now. If this purse stays closed, they fade Thu 08:00.');
    const two = holdings([note(64, 2), note(4, 1), note(1, 1)]);
    assert.deepEqual(laneTip(two, 'ready', NOW).lines, [
      'Notes of 64, 4 and 1. Sends use them.',
      'They can move now. The 4 and 1 fade Thu 00:00, the rest Thu 08:00, unless they move.',
      'The upper of the two ready layers is one closer to the first: the 4 and 1 there start to fade at 16:00.',
    ]);
    assert.equal(
      laneTip(two, 'ready', NOW, true).lines[1],
      'They can move now. If this purse stays closed, the 4 and 1 fade Thu 00:00, the rest Thu 08:00.',
    );
    const one = holdings([note(64, 2)]);
    assert.deepEqual(laneTip(one, 'ready', NOW).lines, [
      'One note of 64. Sends use it.',
      'It can move now. It fades Thu 08:00 unless it moves.',
    ]);
    assert.deepEqual(laneTip(holdings(RESTING()), 'ready', NOW).lines, [
      'Nothing is ready right now.',
      'Resting notes are ready at 16:00.',
    ]);
  });

  test('of a note say what it is, where it came from and when it moves', () => {
    const h = holdings([...READY(), ...RESTING(), ...FADING()]);
    const took = note(64, 3, { arrivedAt: at(21, 14, 20), source: 'faucet' });
    assert.deepEqual(noteTip(h, took, 'Playground', NOW), {
      head: 'Note of 64 · resting',
      lines: ['From the Playground faucet at 14:20.', 'Ready 16:00 · fades Thu 16:00 unless it moves'],
    });
    assert.deepEqual(noteTip(h, note(4, 2), 'Playground', NOW), {
      head: 'Note of 4 · ready',
      lines: ['Can move now · fades Thu 08:00 unless it moves'],
    });
    assert.deepEqual(noteTip(h, note(4, 2), 'Playground', NOW, true).lines, ['Can move now · fades Thu 08:00 if left closed']);
    assert.deepEqual(noteTip(h, note(1, 0), 'Playground', NOW).lines, ['Fades at 16:00 unless it moves.']);
    assert.deepEqual(noteTip(h, note(1, 0), 'Playground', NOW, true).lines, [
      'Fades at 16:00 unless it moves. While this purse is open, it keeps it alive.',
    ]);
  });
});

describe('the labels', () => {
  test('name each lane for assistive tech', () => {
    const h = holdings([...READY(), ...RESTING(), ...FADING()]);
    const step = ' The left and right arrow keys step through its notes.';
    assert.equal(
      laneDescription(h, 'fade', NOW),
      `Fading: 5 tokens, notes of 4 and 1. They fade at 16:00 unless they move. Show where in the field.${step}`,
    );
    assert.equal(
      laneDescription(h, 'rest', NOW),
      `Resting: 123 tokens, notes of 64, 32, 16, 8, 2 and 1. Ready at 16:00. Show where in the field.${step}`,
    );
    assert.equal(
      laneDescription(h, 'ready', NOW),
      `Ready: 69 tokens, notes of 64, 4 and 1. They can move now. Show where in the field.${step}`,
    );
    assert.equal(
      laneDescription(holdings(READY()), 'fade', NOW),
      'Fading: 0 tokens, no notes. Show where in the field.',
    );
    assert.equal(numberDescription(69, 'ready'), '69, ready: show where in the field');
    assert.equal(numberDescription(5, 'fade'), '5, fading: show where in the field');
  });

  test('name the field, its layers and its notes', () => {
    assert.equal(fieldLabel(4), 'The field: your notes on the mint’s four layers, one square per token');
    assert.equal(fieldLabel(12), 'The field: your notes on the mint’s 12 layers, one square per token');
    assert.equal(caption(9), 'One square is one token · 9 notes · first layer at the top');
    assert.equal(caption(1), 'One square is one token · 1 note · first layer at the top');
  });

  test('read a tip out as one line', () => {
    assert.equal(
      tipWords({ head: 'Note of 64 · resting', lines: ['From the Playground faucet at 14:20.', 'Ready 16:00 · fades Thu 16:00 unless it moves'] }),
      'Note of 64 · resting. From the Playground faucet at 14:20. Ready 16:00 · fades Thu 16:00 unless it moves.',
    );
  });

  test('say what the faucet gave and what a rotation made ready', () => {
    const h = holdings([...READY(), ...RESTING()]);
    assert.equal(tookWords(64, 'rest', h, NOW), 'Took 64 from the faucet. It rests until 16:00.');
    assert.equal(tookWords(64, 'ready', h, NOW), 'Took 64 from the faucet. It is ready now.');
    assert.equal(rotatedWords(NEXT, h), 'The mint rotated at 16:00. 69 are ready now.');
    assert.equal(rotatedWords(NEXT, holdings([note(1, 2)])), 'The mint rotated at 16:00. 1 is ready now.');
    assert.equal(rotatedWords(NEXT, holdings(RESTING())), 'The mint rotated at 16:00. Nothing is ready now.');
  });
});

describe('until keep-alive comes', () => {
  test('the first run says what the three states mean', () => {
    assert.deepEqual(legend(), [
      { kind: 'ready', name: 'Ready', words: 'Can move now.' },
      { kind: 'rest', name: 'Resting', words: 'Just arrived; ready at the next rotation.' },
      { kind: 'fade', name: 'Fading', words: 'Sat still too long; fades at the next rotation unless it moves.' },
    ]);
    assert.equal(legend(true)[2]?.words, 'Sat still too long; kept alive while the purse is open.');
  });

  test('nothing the purse says promises to keep tokens alive', () => {
    const held = [
      holdings([]),
      holdings([...READY(), ...RESTING(), ...FADING()]),
      holdings([...FADING()]),
      holdings([note(4, 1), note(1, 2), note(8, 3)]),
    ];
    const said: string[] = legend().map((row) => row.words);
    for (const h of held) {
      const row = comeBackRow(h, NOW);
      said.push(plain(sentence(h, NOW)), firstToGo(h, NOW), comeBackHeading(), 'none' in row ? row.none : row.lead + row.date);
      for (const lane of ['fade', 'ready', 'rest'] as const) {
        said.push(laneDescription(h, lane, NOW), tipWords(laneTip(h, lane, NOW)));
      }
      for (const each of h.notes) {
        said.push(tipWords(noteTip(h, each, 'Playground', NOW)));
      }
    }
    for (const words of said) {
      assert.doesNotMatch(words, /alive|keeps? (it|them)|open (the purse )?again|stays closed|left closed/i, words);
    }
  });
});
