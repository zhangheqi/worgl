import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { when } from '../src/format.ts';
import { type Holdings, type Note, comeBackAt, rotation } from '../src/holdings.ts';
import {
  KEEPING_ELSEWHERE,
  KEEPING_ELSEWHERE_NOTE,
  KEEPS_ALIVE,
  caption,
  comeBackHeading,
  comeBackRow,
  elsewhereNote,
  elsewhereRow,
  fieldLabel,
  firstToGo,
  keepStoreWords,
  keepingCount,
  keepingLead,
  keepingNote,
  keepingStart,
  keptWords,
  laneDescription,
  laneTip,
  layerFullWords,
  legend,
  lostWords,
  noteTip,
  numberDescription,
  pausedKeepWords,
  plain,
  policyWords,
  recentChange,
  recentTitle,
  rotatedWords,
  rotatesQuickly,
  sentence,
  tipWords,
  tookWords,
  uncheckedWords,
  unreachableFirstWords,
  unreachableFor,
  unreachableUnknownWords,
  unreachableWords,
  visitSpan,
  visitWords,
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
    moving: false,
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
    assert.equal(plain(sentence(h, NOW, false)), '69 are ready; 123 rest until 16:00. Nothing fades before Thu 08:00.');
    assert.equal(plain(sentence(h, NOW)), '69 are ready; 123 rest until 16:00. Nothing fades before Thu 08:00.');
    assert.deepEqual(sentence(h, NOW, false).filter((part) => typeof part !== 'string'), [
      { n: 69, lane: 'ready' },
      { n: 123, lane: 'rest' },
    ]);
  });

  test('says it in the singular for one token', () => {
    const h = holdings([note(1, 2), note(1, 3)]);
    assert.equal(plain(sentence(h, NOW, false)), '1 is ready; 1 rests until 16:00. Nothing fades before Thu 08:00.');
  });

  test('says when nothing is ready', () => {
    const h = holdings([note(64, 3)]);
    assert.equal(plain(sentence(h, NOW, false)), 'Nothing is ready; 64 rest until 16:00. Nothing fades before Thu 16:00.');
  });

  test('says what fades, and that nothing keeps it alive while the purse cannot', () => {
    const h = holdings([note(64, 1), ...RESTING(), ...FADING()]);
    assert.equal(
      plain(sentence(h, NOW, false)),
      '64 are ready; 123 rest until 16:00; 5 fade then unless they move.',
    );
    const alone = holdings([note(1, 0)]);
    assert.equal(plain(sentence(alone, NOW, false)), 'Nothing is ready; 1 fades at 16:00 unless it moves.');
  });

  test('with keep-alive, says the purse keeps them alive', () => {
    assert.equal(KEEPS_ALIVE, true);
    const h = holdings([note(64, 1), ...RESTING(), ...FADING()]);
    const parts = sentence(h, NOW);
    assert.equal(
      plain(parts),
      '64 are ready; 123 rest until 16:00; 5 would fade then, so this purse keeps them alive while it is on screen.',
    );
    assert.ok(parts.some((part) => typeof part === 'object' && 'em' in part && part.em === 'alive'));
    const alone = holdings([note(1, 0)]);
    assert.equal(plain(sentence(alone, NOW)), 'Nothing is ready; 1 would fade at 16:00, so this purse keeps it alive while it is on screen.');
  });

  test('counts notes on their way a layer on as fading until they land', () => {
    const h = holdings([note(64, 1), note(64, 0, { moving: true }), note(4, 0)]);
    assert.equal(plain(sentence(h, NOW)), '64 are ready; 68 would fade at 16:00, so this purse keeps them alive while it is on screen.');
  });

  test('says when the purse is empty', () => {
    assert.equal(plain(sentence(holdings([]), NOW, false)), 'This purse is empty.');
    assert.equal(plain(sentence(holdings([]), NOW)), 'This purse is empty.');
  });

  test('names the next rotation as a weekday once it is not today', () => {
    const late = at(21, 23, 10);
    const next = at(22, 0);
    const h = holdings([note(8, 3, { expiresAt: next + 24 * HOUR })], {
      schedule: { startMs: next - 8 * HOUR, nextMs: next, periodMs: 8 * HOUR },
    });
    assert.equal(plain(sentence(h, late, false)), 'Nothing is ready; 8 rest until Thu 00:00. Nothing fades before Fri 00:00.');
  });

  test('says when the mint has no schedule or is paused', () => {
    const hand = holdings([note(8, 2)], { schedule: undefined, firstExpiry: undefined });
    assert.equal(plain(sentence(hand, NOW, false)), '8 are ready. Nothing fades until the mint rotates.');
    const paused = holdings([note(8, 2)], { schedule: undefined, firstExpiry: undefined, paused: true });
    // the app passes false for a paused mint: the purse cannot move its tokens
    assert.equal(plain(sentence(paused, NOW, false)), '8 are ready. The mint is paused, so nothing fades until it resumes.');
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

  test('is when the first tokens fade while the purse cannot keep them alive', () => {
    assert.deepEqual(comeBackRow(holdings([...READY(), ...RESTING()]), NOW, false), {
      lead: 'First tokens fade at ',
      date: 'Thu 08:00',
    });
    assert.deepEqual(comeBackRow(holdings([...FADING()]), at(21, 15, 30), false), { lead: 'First tokens fade at ', date: '16:00' });
    assert.deepEqual(comeBackRow(holdings([...FADING()]), NEXT + 1000, false), { none: 'The mint is rotating now.' });
    assert.equal(comeBackHeading(false), 'First to fade');
    assert.equal(comeBackHeading(), 'Open again before');
  });

  test('says each state, with keep-alive and without', () => {
    const states: [string, Holdings, number, ReturnType<typeof comeBackRow>, ReturnType<typeof comeBackRow>][] = [
      ['empty', holdings([]), NOW, { none: 'Nothing to keep alive yet.' }, { none: 'Nothing to fade yet.' }],
      [
        'paused',
        holdings([note(4, 2)], { paused: true, schedule: undefined }),
        NOW,
        { none: 'This mint is paused.' },
        { none: 'This mint is paused.' },
      ],
      [
        'by hand',
        holdings([note(4, 2)], { schedule: undefined, firstExpiry: undefined }),
        NOW,
        { none: 'This mint rotates only by hand.' },
        { none: 'This mint rotates only by hand.' },
      ],
      [
        'rotating now',
        holdings([...FADING()]),
        NEXT + 1000,
        { none: 'The mint is rotating now.' },
        { none: 'The mint is rotating now.' },
      ],
      [
        'dated',
        holdings([...READY(), ...RESTING()]),
        NOW,
        { lead: 'Open again before ', date: 'Thu 07:00' },
        { lead: 'First tokens fade at ', date: 'Thu 08:00' },
      ],
    ];
    for (const [name, h, now, kept, fallback] of states) {
      assert.deepEqual(comeBackRow(h, now, true), kept, name);
      assert.deepEqual(comeBackRow(h, now, false), fallback, name);
    }
    assert.equal(comeBackAt(holdings([]), NOW), undefined);
  });

  test('says on desktop which tokens go first', () => {
    assert.equal(
      firstToGo(holdings([...READY(), ...RESTING()]), NOW, false),
      'Your 69 ready tokens go then unless they move.',
    );
    assert.equal(firstToGo(holdings([note(1, 3)]), NOW, false), 'Your 1 resting token goes then unless it moves.');
    assert.equal(
      firstToGo(holdings([...READY(), ...FADING()]), NOW),
      'Kept alive only while the purse is on screen. Left closed or hidden, your 5 fading tokens go at 16:00.',
    );
    assert.equal(firstToGo(holdings([]), NOW, false), 'Nothing to fade yet.');
    assert.equal(firstToGo(holdings([]), NOW), 'Nothing to keep alive yet.');
  });
});

describe('the tips', () => {
  test('of the resting lane say what rests and when it is ready', () => {
    const h = holdings([...READY(), ...RESTING()]);
    assert.deepEqual(laneTip(h, 'rest', NOW, false), {
      head: 'Resting · 123',
      lines: [
        'Notes of 64, 32, 16, 8, 2 and 1 in the last layer. Notes that just moved rest here.',
        'Ready at 16:00. They fade Thu 16:00 unless they move.',
      ],
    });
    assert.equal(laneTip(h, 'rest', NOW).lines[1], 'Ready at 16:00. If this purse stays closed or hidden, they fade at Thu 16:00.');
    assert.equal(
      laneTip(holdings([note(64, 3)]), 'rest', NOW).lines[1],
      'Ready at 16:00. If this purse stays closed or hidden, it fades at Thu 16:00.',
    );
    assert.deepEqual(laneTip(holdings(READY()), 'rest', NOW, false).lines, [
      'Nothing rests in the last layer.',
      'What you take in, and change from a send, usually rests here until the next rotation.',
    ]);
  });

  test('of the fading lane say when they fade, that the purse keeps them alive while open, and what the mint sees', () => {
    const h = holdings([note(64, 1), ...FADING()]);
    assert.deepEqual(laneTip(h, 'fade', NOW, false), {
      head: 'Fading · 5',
      lines: ['Notes of 4 and 1 in the first layer. They fade at 16:00 unless they move.'],
    });
    assert.deepEqual(laneTip(h, 'fade', NOW), {
      head: 'Fading · 5',
      lines: [
        'Notes of 4 and 1 in the first layer. They fade at 16:00 unless they move.',
        'While this purse is on screen, it keeps them alive: one transfer at a time, it moves them a layer further from the first, down the field.',
        'The mint can tell which notes one visit moved.',
      ],
    });
    assert.deepEqual(laneTip(holdings(READY()), 'fade', NOW, false).lines, ['Nothing is in the first layer.']);
    assert.deepEqual(laneTip(holdings(READY()), 'fade', NOW).lines, [
      'Nothing is in the first layer.',
      'While this purse is on screen, it keeps notes alive: one transfer at a time, it moves them a layer further from the first, down the field.',
      'The mint can tell which notes one visit moved.',
    ]);
  });

  test('of the ready lane say what can move and when it fades', () => {
    const h = holdings([...READY(), ...RESTING()]);
    assert.deepEqual(laneTip(h, 'ready', NOW, false), {
      head: 'Ready · 69',
      lines: ['Notes of 64, 4 and 1. Sends use them.', 'They can move now. They fade Thu 08:00 unless they move.'],
    });
    assert.equal(laneTip(h, 'ready', NOW).lines[1], 'They can move now. If this purse stays closed or hidden, they fade at Thu 08:00.');
    const two = holdings([note(64, 2), note(4, 1), note(1, 1)]);
    assert.deepEqual(laneTip(two, 'ready', NOW, false).lines, [
      'Notes of 64, 4 and 1. Sends use them.',
      'They can move now. The 4 and 1 fade Thu 00:00, the rest Thu 08:00, unless they move.',
      'The upper of the two ready layers is one closer to the first: the 4 and 1 there start to fade at 16:00.',
    ]);
    assert.equal(
      laneTip(two, 'ready', NOW).lines[1],
      'They can move now. If this purse stays closed or hidden, the 4 and 1 fade at Thu 00:00, the rest at Thu 08:00.',
    );
    // with the rest spread over more layers, a range, as before
    const spread = holdings([note(4, 1), note(64, 2), note(8, 3)], { layers: 5 });
    assert.equal(
      laneTip(spread, 'ready', NOW).lines[1],
      'They can move now. If this purse stays closed or hidden, the 4 fades at Thu 00:00, the rest from Thu 08:00.',
    );
    const one = holdings([note(64, 2)]);
    assert.deepEqual(laneTip(one, 'ready', NOW, false).lines, [
      'One note of 64. Sends use it.',
      'It can move now. It fades Thu 08:00 unless it moves.',
    ]);
    assert.equal(laneTip(one, 'ready', NOW).lines[1], 'It can move now. If this purse stays closed or hidden, it fades at Thu 08:00.');
    assert.deepEqual(laneTip(holdings(RESTING()), 'ready', NOW, false).lines, [
      'Nothing is ready right now.',
      'Resting notes are ready at 16:00.',
    ]);
  });

  test('of a note say what it is, where it came from and when it moves', () => {
    const h = holdings([...READY(), ...RESTING(), ...FADING()]);
    const took = note(64, 3, { arrivedAt: at(21, 14, 20), source: 'faucet' });
    assert.deepEqual(noteTip(h, took, 'Playground', NOW, false), {
      head: 'Note of 64 · resting',
      lines: ['From the Playground faucet at 14:20.', 'Ready 16:00 · fades Thu 16:00 unless it moves'],
    });
    assert.deepEqual(noteTip(h, note(4, 2), 'Playground', NOW, false), {
      head: 'Note of 4 · ready',
      lines: ['Can move now · fades Thu 08:00 unless it moves'],
    });
    assert.deepEqual(noteTip(h, note(4, 2), 'Playground', NOW).lines, ['Can move now · fades Thu 08:00 if left closed or hidden']);
    assert.deepEqual(noteTip(h, note(1, 0), 'Playground', NOW, false).lines, ['Fades at 16:00 unless it moves.']);
    assert.deepEqual(noteTip(h, note(1, 0), 'Playground', NOW).lines, [
      'Fades at 16:00 unless it moves. While this purse is on screen, it keeps it alive.',
    ]);
  });

  test('of a note on its way say it is moving, and when it fades only while the purse cannot move it', () => {
    const h = holdings([note(64, 0, { moving: true }), note(64, 1)]);
    const moving = note(64, 0, { moving: true });
    assert.deepEqual(noteTip(h, moving, 'Playground', NOW), {
      head: 'Note of 64 · fading',
      lines: ['Moving a layer on now, to a fresh key.'],
    });
    // the mint did not answer, or is paused: it waits, and the words above say why
    assert.deepEqual(noteTip(h, moving, 'Playground', NOW, false).lines, ['Waiting to move a layer on. Fades at 16:00 unless it moves.']);
  });

  test('of a note keep-alive moved say when it was kept', () => {
    const h = holdings([note(64, 1), note(4, 2)]);
    const kept = note(64, 1, { arrivedAt: at(21, 14, 20), source: 'kept' });
    assert.deepEqual(noteTip(h, kept, 'Playground', NOW), {
      head: 'Note of 64 · ready',
      lines: ['Kept alive at 14:20.', 'Can move now · fades Thu 00:00 if left closed or hidden'],
    });
  });
});

describe('the labels', () => {
  test('name each lane for assistive tech', () => {
    const h = holdings([...READY(), ...RESTING(), ...FADING()]);
    const step = ' The left and right arrow keys step through its notes.';
    assert.equal(
      laneDescription(h, 'fade', NOW, false),
      `Fading: 5 tokens, notes of 4 and 1. They fade at 16:00 unless they move. Show where in the field.${step}`,
    );
    assert.equal(
      laneDescription(h, 'fade', NOW),
      `Fading: 5 tokens, notes of 4 and 1. They fade at 16:00 unless they move; while this purse is on screen, it keeps them alive. Show where in the field.${step}`,
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

/** The fixtures every promise is checked over. */
const FIXTURES = (): [string, Holdings][] => [
  ['empty', holdings([])],
  ['ready, resting and fading', holdings([...READY(), ...RESTING(), ...FADING()])],
  ['fading alone', holdings([...FADING()])],
  ['mixed layers', holdings([note(4, 1), note(1, 2), note(8, 3)])],
  ['on their way', holdings([note(4, 0, { moving: true }), note(1, 0), note(8, 2)])],
  ['paused', holdings([note(4, 0), note(8, 2)], { paused: true, schedule: undefined })],
  [
    'by hand',
    holdings(
      [note(4, 0, { expiresAt: undefined }), note(8, 2, { expiresAt: undefined, readyAt: undefined })],
      { schedule: undefined, firstExpiry: undefined },
    ),
  ],
];

/** Everything the purse says of some holdings: the sentence, the come-back row and column, each lane, each note. */
function everything(h: Holdings, keepsAlive: boolean): string[] {
  const row = comeBackRow(h, NOW, keepsAlive);
  const said = [
    plain(sentence(h, NOW, keepsAlive)),
    firstToGo(h, NOW, keepsAlive),
    comeBackHeading(keepsAlive),
    'none' in row ? row.none : row.lead + row.date,
  ];
  for (const lane of ['fade', 'ready', 'rest'] as const) {
    said.push(laneDescription(h, lane, NOW, keepsAlive), tipWords(laneTip(h, lane, NOW, keepsAlive)));
  }
  for (const each of h.notes) {
    said.push(tipWords(noteTip(h, each, 'Playground', NOW, keepsAlive)));
  }
  said.push(elsewhereNote('127.0.0.1:4000', h, NOW, keepsAlive));
  return said;
}

/** Everything the purse says when it cannot move tokens, over the fixtures. */
function failures(): string[] {
  const host = 'playground.worgl.1y.io';
  return FIXTURES().flatMap(([, h]) => {
    const stuck = { values: [4, 1], pos: 0, holdings: h, now: NOW };
    return [
      unreachableWords(host, stuck),
      uncheckedWords(host, stuck),
      pausedKeepWords(host, 5),
      layerFullWords(host, stuck),
      policyWords(host, stuck),
      keepStoreWords,
      unreachableFor(host, h, undefined, NOW) ?? '',
      unreachableFor(host, undefined, { at: NEXT, amount: 5 }, NOW) ?? '',
      unreachableUnknownWords(host),
    ];
  });
}

describe('what keep-alive promises', () => {
  test('the first run says what the three states mean', () => {
    assert.deepEqual(legend(), [
      { kind: 'ready', name: 'Ready', words: 'Can move now.' },
      { kind: 'rest', name: 'Resting', words: 'Just arrived; ready at the next rotation.' },
      { kind: 'fade', name: 'Fading', words: 'Sat still too long; kept alive while the purse is on screen.' },
    ]);
    assert.equal(legend(false)[2]?.words, 'Sat still too long; fades at the next rotation unless it moves.');
  });

  test('is true: tokens are kept alive only while the purse is on screen, and left closed or hidden they fade at a time', () => {
    const said = [
      ...legend().map((row) => row.words),
      ...FIXTURES().flatMap(([, h]) => everything(h, true)),
      ...failures(),
    ];
    for (const words of said) {
      // a page in the background keeps nothing alive: never "while it is open"
      assert.doesNotMatch(words, /while (it|the purse|this purse) is open/i, words);
      if (/\bkeeps? (it|them|notes|tokens) alive|kept alive (only )?while|moves (it|them)|asks again|tries again/i.test(words)) {
        assert.match(words, /\bon screen\b/, words);
      }
      if (/stays closed|left closed/.test(words)) {
        // a time, or for a mint with no schedule the rotation it waits for; and the background, which keeps nothing either
        assert.match(words, /\d\d:\d\d|rotation/, words);
        assert.match(words, /closed or hidden/, words);
      }
    }
  });

  test('goes no further than one rotation: no reminder, no lift, no split', () => {
    const said = [...legend().map((row) => row.words), ...FIXTURES().flatMap(([, h]) => everything(h, true)), ...failures()];
    for (const words of said) {
      assert.doesNotMatch(words, /remind|calendar|notif|alarm|e-?mail|push|lift|split|for (days|weeks)/i, words);
    }
    for (const [name, h] of FIXTURES()) {
      const back = comeBackAt(h, NOW);
      if (back !== undefined && h.firstExpiry !== undefined) {
        assert.ok(back <= h.firstExpiry, name);
      }
      if (!h.notes.some((each) => each.pos === 0) || h.schedule === undefined) {
        continue;
      }
      // a visit moves what sits at layer 0 one layer on, and nothing else
      const notes = h.notes.map((each) =>
        each.pos === 0 ? { ...each, pos: 1, expiresAt: (each.expiresAt ?? NEXT) + 8 * HOUR } : each,
      );
      const after = holdings(notes);
      const second = rotation(after, 1);
      assert.ok(second !== undefined);
      assert.equal(after.firstExpiry, second, name);
      assert.ok((comeBackAt(after, NOW) ?? Number.POSITIVE_INFINITY) < second, name);
      // one visit buys at most the second rotation from now: P to 2P
      assert.ok(firstToGo(after, NOW).endsWith(` at ${when(second, NOW)}.`), name);
    }
  });

  test('the fallback never promises', () => {
    const said = [...legend(false).map((row) => row.words), ...FIXTURES().flatMap(([, h]) => everything(h, false))];
    for (const words of said) {
      assert.doesNotMatch(words, /alive|keeps? (it|them)|open (the purse )?again|stays closed|left closed/i, words);
    }
  });
});

describe('a keep-alive run', () => {
  test('counts the transfer under way', () => {
    assert.equal(keepingLead(128), 'Keeping 128 alive');
    assert.equal(keepingCount({ amount: 128, done: 0, total: 2 }), '1 of 2');
    assert.equal(keepingCount({ amount: 128, done: 1, total: 2 }), '2 of 2');
    assert.equal(keepingCount({ amount: 128, done: 2, total: 2 }), '2 of 2');
    assert.equal(KEEPING_ELSEWHERE, 'In another tab');
  });

  test('says how it moves them', () => {
    assert.equal(keepingNote(128, true), 'Moving 128 on, one layer per transfer, a few seconds apart, each to a fresh key.');
    assert.equal(keepingNote(128, false), 'Moving 128 on, one layer per transfer, each to a fresh key.');
    assert.equal(
      KEEPING_ELSEWHERE_NOTE,
      'Another tab of this purse is moving them on, one layer per transfer, a few seconds apart.',
    );
  });

  test('says when it starts, when it ends, and what was lost', () => {
    assert.equal(keepingStart(64, 1, true), 'Keeping 64 alive: one transfer.');
    assert.equal(keepingStart(128, 2, true), 'Keeping 128 alive: two transfers, a few seconds apart.');
    assert.equal(keepingStart(128, 2, false), 'Keeping 128 alive: two transfers.');
    assert.equal(keptWords(128, at(22, 7), NOW), 'Kept 128 alive. Open again before Thu 07:00.');
    assert.equal(keptWords(128, undefined, NOW), 'Kept 128 alive.');
    assert.equal(lostWords(64, 1), 'Lost 64: the mint moved it a layer on, but its reply never reached this purse, and the mint no longer keeps that reply.');
    assert.equal(lostWords(128, 2), 'Lost 128: the mint moved them a layer on, but its reply never reached this purse, and the mint no longer keeps that reply.');
  });
});

describe('what keeps tokens from moving', () => {
  const h = holdings([note(64, 1), ...FADING()]);
  const host = 'playground.worgl.1y.io';
  const both = { values: [4, 1], pos: 0, holdings: h, now: NOW };
  const four = { values: [4], pos: 0, holdings: h, now: NOW };
  const one = { values: [1], pos: 0, holdings: h, now: NOW };

  test('a mint that does not answer', () => {
    assert.equal(
      unreachableWords(host, both),
      'The mint at playground.worgl.1y.io did not answer, so 5 tokens could not move. They fade at 16:00 unless the purse reaches it before then. The purse asks again each minute while it is on screen.',
    );
    assert.equal(
      unreachableWords(host, one),
      'The mint at playground.worgl.1y.io did not answer, so 1 token could not move. It fades at 16:00 unless the purse reaches it before then. The purse asks again each minute while it is on screen.',
    );
    assert.equal(
      unreachableWords(host, { ...both, pos: 1 }),
      'The mint at playground.worgl.1y.io did not answer, so 5 tokens could not move. They fade at Thu 00:00 unless the purse reaches it before then. The purse asks again each minute while it is on screen.',
    );
    const hand = holdings([...FADING()], { schedule: undefined, firstExpiry: undefined });
    assert.match(unreachableWords(host, { ...both, holdings: hand }), /They fade at the next rotation unless/);
  });

  test('an answer that does not check out', () => {
    assert.equal(
      uncheckedWords(host, both),
      'The answer from playground.worgl.1y.io did not check out, so 5 tokens could not move. They fade at 16:00 unless a later answer checks out before then. The purse asks again each minute while it is on screen.',
    );
    assert.equal(
      uncheckedWords(host, one),
      'The answer from playground.worgl.1y.io did not check out, so 1 token could not move. It fades at 16:00 unless a later answer checks out before then. The purse asks again each minute while it is on screen.',
    );
  });

  test('a paused mint', () => {
    assert.equal(
      pausedKeepWords(host, 5),
      'The mint at playground.worgl.1y.io is paused, so 5 tokens could not move. Nothing fades while it is paused; once it resumes, the purse moves them while it is on screen.',
    );
    assert.equal(
      pausedKeepWords(host, 1),
      'The mint at playground.worgl.1y.io is paused, so 1 token could not move. Nothing fades while it is paused; once it resumes, the purse moves it while it is on screen.',
    );
  });

  test('a full next layer, and the mint’s rules', () => {
    assert.equal(layerFullWords(host, both), 'The mint at playground.worgl.1y.io has no room in the next layer, so notes of 4 and 1 could not move. They fade at 16:00.');
    assert.equal(layerFullWords(host, four), 'The mint at playground.worgl.1y.io has no room in the next layer, so one note of 4 could not move. It fades at 16:00.');
    assert.equal(policyWords(host, four), 'The rules of the mint at playground.worgl.1y.io no longer let one note of 4 move, so it fades at 16:00.');
    assert.equal(policyWords(host, both), 'The rules of the mint at playground.worgl.1y.io no longer let notes of 4 and 1 move, so they fade at 16:00.');
  });

  test('a store that refuses to save', () => {
    assert.equal(keepStoreWords, 'This browser refused to save, so the purse stopped moving tokens. It tries again each minute while it is on screen.');
  });
});

describe('when the mint does not answer', () => {
  const host = 'playground.worgl.1y.io';

  test('says what fades and when from the newest holdings: what is fading, or else what goes first', () => {
    const fading = holdings([note(64, 1), ...FADING()]);
    assert.equal(unreachableFor(host, fading, undefined, NOW), unreachableWords(host, { values: [4, 1], pos: 0, holdings: fading, now: NOW }));
    // notes on their way count as fading
    const moving = holdings([note(64, 1), note(4, 0, { moving: true })]);
    assert.match(unreachableFor(host, moving, null, NOW) ?? '', /so 4 tokens could not move\. They fade at 16:00/);
    const later = holdings([note(64, 1), note(4, 2)]);
    assert.equal(
      unreachableFor(host, later, undefined, NOW),
      'The mint at playground.worgl.1y.io did not answer. As the purse last saw them, 64 tokens fade at Thu 00:00 unless the purse reaches it before then. The purse asks again each minute while it is on screen.',
    );
    // nothing held, or no schedule: the read's own words stand
    assert.equal(unreachableFor(host, holdings([]), undefined, NOW), undefined);
    assert.equal(unreachableFor(host, holdings([note(4, 2)], { schedule: undefined, firstExpiry: undefined }), undefined, NOW), undefined);
  });

  test('on an open with no read, says what the purse saved, or that it cannot tell', () => {
    assert.equal(
      unreachableFor(host, undefined, { at: NEXT, amount: 1 }, NOW),
      'The mint at playground.worgl.1y.io did not answer. As the purse last saw them, 1 token fades at 16:00 unless the purse reaches it before then. The purse asks again each minute while it is on screen.',
    );
    assert.equal(unreachableFor(host, undefined, null, NOW), undefined);
    // a time gone: what faded shows once the mint answers
    assert.equal(unreachableFor(host, undefined, { at: NOW - HOUR, amount: 1 }, NOW), undefined);
    assert.equal(
      unreachableFor(host, undefined, undefined, NOW),
      'The mint at playground.worgl.1y.io did not answer, so the purse cannot tell yet when its tokens there fade. The purse asks again each minute while it is on screen.',
    );
    assert.equal(unreachableUnknownWords(host), unreachableFor(host, undefined, undefined, NOW));
    assert.equal(unreachableFirstWords(host, { at: NEXT, amount: 64 }, NOW), unreachableFor(host, undefined, { at: NEXT, amount: 64 }, NOW));
  });
});

describe('another mint’s tokens going first', () => {
  test('the come-back row and column name the mint they are for', () => {
    const h = holdings([note(64, 1), note(4, 2)]);
    assert.equal(elsewhereRow('127.0.0.1:4000'), 'For your tokens at 127.0.0.1:4000');
    assert.equal(elsewhereNote('127.0.0.1:4000', h, NOW), 'For your tokens at 127.0.0.1:4000. Left closed or hidden, 64 there go at Thu 00:00.');
    assert.equal(elsewhereNote('127.0.0.1:4000', holdings([note(1, 1)]), NOW), 'For your tokens at 127.0.0.1:4000. Left closed or hidden, 1 there goes at Thu 00:00.');
    assert.equal(elsewhereNote('127.0.0.1:4000', h, NOW, false), 'For your tokens at 127.0.0.1:4000: 64 there go then unless they move.');
  });
});

describe('a mint that rotates quickly', () => {
  test('says what one visit keeps its tokens for', () => {
    assert.equal(visitWords(3600), 'A visit keeps this mint’s tokens for 1 to 2 hours, so it suits services more than people.');
    assert.equal(visitWords(120), 'A visit keeps this mint’s tokens for 2 to 4 minutes, so it suits services more than people.');
    assert.equal(visitWords(1800), 'A visit keeps this mint’s tokens for 30 to 60 minutes, so it suits services more than people.');
    assert.equal(visitSpan(30), '30 to 60 seconds');
    assert.equal(visitSpan(7200), '2 to 4 hours');
  });

  test('is one that rotates more often than every four hours', () => {
    assert.equal(rotatesQuickly(3600), true);
    assert.equal(rotatesQuickly(14_399), true);
    assert.equal(rotatesQuickly(14_400), false);
    assert.equal(rotatesQuickly(28_800), false);
  });
});

describe('Recent', () => {
  const entry = (kind: 'faucet' | 'faded' | 'kept' | 'lost', amount: number) => ({ kind, amount });

  test('titles each kind of entry', () => {
    assert.equal(recentTitle(entry('faucet', 64)), 'Took 64 from the faucet');
    assert.equal(recentTitle(entry('faded', 4)), 'Faded 4');
    assert.equal(recentTitle(entry('kept', 128)), 'Kept 128 alive');
    assert.equal(recentTitle(entry('lost', 64)), 'Lost 64');
  });

  test('shows what each changed: keep-alive changes nothing', () => {
    assert.equal(recentChange(entry('faucet', 64)), 64);
    assert.equal(recentChange(entry('faded', 4)), -4);
    assert.equal(recentChange(entry('kept', 128)), 0);
    assert.equal(recentChange(entry('lost', 64)), -64);
  });
});
