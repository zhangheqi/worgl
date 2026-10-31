import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  FILE_MAX,
  checkingWords,
  coverLab,
  coverOf,
  coverRow,
  coverWords,
  fileName,
  fileProblem,
  fileWords,
  passOf,
  passProblem,
  passWords,
  pendingWords,
  restorePlan,
  restoredWords,
  savedWords,
  storageWords,
  wantsBackup,
} from '../src/cover.ts';

const MINT = 'ab'.repeat(32);
const OTHER = 'cd'.repeat(32);
const THIRD = 'ef'.repeat(32);
// Fri 30 Oct 2026, local time
const at = (day: number, hour: number, minute = 0): number => new Date(2026, 9, day, hour, minute).getTime();
const NOW = at(30, 14, 15);
const FORBIDDEN = /\b(wallet|cash|coin|account|pay)/i;

/** Every line said here, checked at the end for words the purse never uses. */
const said: string[] = [];
const keep = <T>(value: T): T => {
  said.push(JSON.stringify(value));
  return value;
};

describe('the passphrase', () => {
  test('needs eight characters, typed the same twice', () => {
    assert.equal(passProblem('', ''), 'short');
    assert.equal(passProblem('seven77', 'seven77'), 'short');
    assert.equal(passProblem('eight888', 'eight889'), 'differ');
    assert.equal(passProblem('eight888', 'eight888'), undefined);
  });

  test('counts characters, not UTF-16 units', () => {
    // seven emoji are fourteen units, and still seven characters
    assert.equal(passProblem('🔑🔑🔑🔑🔑🔑🔑', '🔑🔑🔑🔑🔑🔑🔑'), 'short');
    assert.equal(passProblem('🔑🔑🔑🔑🔑🔑🔑🔑', '🔑🔑🔑🔑🔑🔑🔑🔑'), undefined);
  });

  test('is composed before it is counted, compared or sealed', () => {
    const composed = 'é'.normalize('NFC');
    const decomposed = 'é'.normalize('NFD');
    assert.equal(passProblem(composed.repeat(8), decomposed.repeat(8)), undefined);
    // four letters and four marks are four characters
    assert.equal(passProblem(decomposed.repeat(4), decomposed.repeat(4)), 'short');
    assert.equal(passOf(decomposed.repeat(8)), composed.repeat(8));
  });

  test('says what is wrong', () => {
    assert.equal(keep(passWords('short')), 'Use a passphrase of 8 characters or more.');
    assert.equal(keep(passWords('differ')), 'The two passphrases differ. Type the same one twice.');
  });
});

describe('the file', () => {
  test('is named by the local time it was sealed', () => {
    assert.equal(fileName(at(30, 9, 5)), 'worgl-purse-2026-10-30-0905.worgl');
  });

  test('is checked before any passphrase', () => {
    const sealed = JSON.stringify({ format: 'worgl-purse', version: 1 });
    assert.equal(fileProblem(sealed.length, sealed), undefined);
    assert.equal(fileProblem(0, ''), 'empty');
    assert.equal(fileProblem(FILE_MAX + 1, undefined), 'too_big');
    assert.equal(fileProblem(10, 'not json!!'), 'not_sealed');
    assert.equal(fileProblem(20, '{"format":"other"}'), 'not_sealed');
    assert.equal(fileProblem(4, 'null'), 'not_sealed');
    for (const problem of ['empty', 'too_big', 'not_sealed'] as const) {
      assert.match(keep(fileWords(problem)), /\.$/);
    }
  });

  test('says what a save holds', () => {
    assert.equal(
      keep(savedWords([{ name: 'Playground', unit: 'tokens', amount: 64 }], at(30, 14, 2))),
      'Saved at 14:02. The file holds 64 tokens. Keep it somewhere apart from this device; you need it and the passphrase to bring them back.',
    );
    assert.equal(
      keep(
        savedWords(
          [
            { name: 'Playground', unit: 'tokens', amount: 64 },
            { name: 'Market', unit: 'credits', amount: 1 },
            { name: 'Empty', unit: 'tokens', amount: 0 },
          ],
          at(30, 14, 2),
        ),
      ),
      'Saved at 14:02. The file holds 64 tokens of Playground, 1 credit of Market. Keep it somewhere apart from this device; you need it and the passphrase to bring them back.',
    );
    assert.match(keep(savedWords([], at(30, 14, 2))), /holds nothing yet/);
  });

  test('downloaded, counts only once the person says it is there', () => {
    assert.equal(
      keep(pendingWords('worgl-purse-2026-10-30-0905.worgl')),
      'The browser is saving worgl-purse-2026-10-30-0905.worgl. Check that it is with your files, then say so below: until then it does not count as a backup.',
    );
  });
});

describe('what the last backup covers', () => {
  const notes = [
    { id: '7:1', v: 32 },
    { id: '7:2', v: 16 },
    { id: '8:4', v: 4 },
    { id: '8:5', v: 1 },
  ];

  test('is the notes held now that it names, by value', () => {
    const cover = coverOf(notes, ['7:1', '8:5', '6:9'], 53);
    assert.equal(cover.covered, 33);
    assert.equal(cover.total, 53);
    assert.deepEqual(cover.notes, [
      { v: 32, in: true },
      { v: 16, in: false },
      { v: 4, in: false },
      { v: 1, in: true },
    ]);
  });

  test('covers nothing on its way back, which the total counts', () => {
    const cover = coverOf(notes, ['7:1', '7:2', '8:4', '8:5'], 60);
    assert.equal(cover.covered, 53);
    assert.equal(cover.total, 60);
  });

  test('covers nothing without a backup', () => {
    assert.equal(coverOf(notes, undefined, 53).covered, 0);
  });

  test('asks for a new backup under half covered', () => {
    assert.equal(wantsBackup({ covered: 0, total: 0 }), false);
    assert.equal(wantsBackup({ covered: 0, total: 1 }), true);
    assert.equal(wantsBackup({ covered: 32, total: 64 }), false);
    assert.equal(wantsBackup({ covered: 31, total: 64 }), true);
  });

  test('says so', () => {
    const t = at(30, 9, 15);
    assert.equal(keep(coverWords(undefined, undefined, NOW)), 'There is no backup of this purse yet.');
    assert.equal(keep(coverWords({ covered: 64, total: 64 }, t, NOW)), 'Your last backup covers all 64 tokens.');
    assert.equal(
      keep(coverWords({ covered: 52, total: 64 }, t, NOW)),
      'Your last backup covers 52 of 64 tokens. The rest have moved or arrived since 09:15.',
    );
    assert.equal(
      keep(coverWords({ covered: 0, total: 64 }, t, NOW)),
      'Your last backup covers 0 of 64 tokens. Every token here has moved or arrived since 09:15.',
    );
    assert.equal(
      keep(coverWords({ covered: 0, total: 0 }, t, NOW)),
      'The purse holds nothing now, so there is nothing for a backup to cover.',
    );
    assert.equal(
      keep(coverWords({ covered: 1, total: 4 }, at(28, 9, 15), NOW, 'Market')),
      'Your last backup covers 1 of 4 tokens of Market. The rest have moved or arrived since Wed 09:15.',
    );
    assert.equal(keep(coverWords(undefined, undefined, NOW, 'Market')), 'There is no backup of Market yet.');
    assert.equal(coverLab({ covered: 52, total: 64 }), '52 / 64 covered');
    assert.equal(coverLab(undefined), 'None');
    assert.equal(coverRow({ covered: 52, total: 64 }, t, NOW), 'Covers 52 of 64 · 09:15');
    assert.equal(coverRow(undefined, undefined, NOW), 'None yet');
    // a backup whose cover is not read yet, or not read at all, is still one
    assert.equal(coverRow(undefined, t, NOW), 'Saved 09:15');
  });
});

describe('a restore', () => {
  test('puts a mint the purse keeps on its own server, a new one on the server the backup names, by that name alone', () => {
    const plan = restorePlan(
      [
        { mint: MINT, server: 'https://elsewhere.example', extra: { saved: at(30, 9, 5) } },
        { mint: OTHER, server: 'https://market.example', extra: { name: '  Market\nstall ', unit: 'credits', saved: 'yesterday' } },
        { mint: THIRD, server: undefined, extra: { name: 'Lost' } },
      ],
      [{ mint: MINT, server: 'https://playground.worgl.1y.io' }],
    );
    assert.deepEqual(plan, {
      mints: [
        { mint: MINT, server: 'https://playground.worgl.1y.io', known: true, saved: at(30, 9, 5) },
        // a file can come from anyone: the name and unit it gives are not taken
        { mint: OTHER, server: 'https://market.example', known: false, saved: undefined },
      ],
      unplaced: [THIRD],
    });
  });

  test('places a new mint only at a server a link could name', () => {
    const plan = restorePlan(
      [
        { mint: MINT, server: 'http://mint.example', extra: {} },
        { mint: OTHER, server: 'https://market.example/?x=1', extra: {} },
        { mint: THIRD, server: 'https://ok.example/', extra: {} },
        { mint: 'nothex', server: 'https://ok.example', extra: {} },
      ],
      [],
    );
    assert.deepEqual(plan.mints.map((each) => [each.mint, each.server]), [[THIRD, 'https://ok.example']]);
    assert.deepEqual(plan.unplaced, [MINT, OTHER, 'nothex']);
  });

  test('takes the time a backup was sealed only as a time', () => {
    const saved = (value: unknown): number | undefined =>
      restorePlan([{ mint: MINT, server: 'https://a.example', extra: { saved: value } }], []).mints[0]?.saved;
    assert.equal(saved(at(30, 9, 5)), at(30, 9, 5));
    for (const value of [undefined, 0, -1, 1.5, '1700000000000', Number.MAX_SAFE_INTEGER + 2]) {
      assert.equal(saved(value), undefined, String(value));
    }
  });

  test('says what came of each mint', () => {
    const one = { name: 'Playground', unit: 'tokens', brought: [32, 16, 4], faded: [] as number[], dropped: [] as number[], transfers: 0 };
    assert.deepEqual(keep(restoredWords([one], 0)), [
      'Restored 52 tokens, in three notes. The purse checks each with the mint before they move.',
    ]);
    assert.deepEqual(keep(restoredWords([{ ...one, brought: [] }], 0)), ['Nothing in the backup was new to this purse.']);
    assert.deepEqual(keep(restoredWords([{ ...one, brought: [], faded: [4] }], 0)), [
      'The backup held one note of 4 this purse did not, but it has faded since.',
    ]);
    assert.deepEqual(keep(restoredWords([{ ...one, brought: [], faded: [4, 4] }], 0)), [
      'The backup held two notes of 4 this purse did not, but they have all faded since.',
    ]);
    // a faded note is counted as a note, by value, and is not among those checked
    assert.deepEqual(keep(restoredWords([{ ...one, brought: [32, 16], faded: [4], dropped: [8], transfers: 2 }], 1)), [
      'Restored 48 tokens, in two notes. One note of 4 it held had faded since. The purse checks each with the mint before they move. The backup had spent 8 this purse still held, so those are dropped. Two transfers it held go again.',
      'The backup also holds a mint at no server this purse can reach from it. Add it by its link, then restore the file again.',
    ]);
    assert.deepEqual(keep(restoredWords([{ ...one, brought: [64], transfers: 1 }], 0)), [
      'Restored 64 tokens, in one note. The purse checks it with the mint before it moves. A transfer it held goes again.',
    ]);
    assert.deepEqual(
      keep(restoredWords([one, { name: 'Market', unit: 'credits', brought: [1], faded: [], dropped: [], transfers: 0 }], 0)),
      [
        'Restored 52 tokens of Playground, in three notes. The purse checks each with the mint before they move.',
        'Restored 1 credit of Market, in one note. The purse checks it with the mint before it moves.',
      ],
    );
  });
});

describe('the checks of restored notes', () => {
  test('say how far they are, and what they found', () => {
    assert.equal(
      keep(checkingWords({ left: 4, done: 2, live: 16, spent: 0, faded: 0, unanswered: 0 })),
      'Checking six restored notes with the mint, a few seconds apart: 2 done.',
    );
    assert.equal(
      keep(checkingWords({ left: 0, done: 6, live: 40, spent: 12, faded: 0, unanswered: 0 })),
      'Checked six restored notes: 40 live, 12 already spent, now dropped.',
    );
    assert.equal(
      keep(checkingWords({ left: 0, done: 3, live: 0, spent: 0, faded: 8, unanswered: 0 })),
      'Checked three restored notes: 8 faded, now dropped.',
    );
    assert.equal(
      keep(checkingWords({ left: 0, done: 4, live: 40, spent: 12, faded: 8, unanswered: 0 })),
      'Checked four restored notes: 40 live, 12 already spent and 8 faded, both now dropped.',
    );
    assert.equal(
      keep(checkingWords({ left: 0, done: 1, live: 0, spent: 0, faded: 0, unanswered: 1 })),
      'Checked one restored note. The mint did not answer for one; the purse asks again on its next open.',
    );
  });
});

describe('storage', () => {
  test('says whether this browser keeps the purse, and how to make Safari keep it', () => {
    const kept = keep(storageWords(true, true));
    assert.equal(kept.length, 2);
    assert.match(kept[0] ?? '', /agreed to keep/);
    const loose = keep(storageWords(undefined, false));
    assert.equal(loose.length, 3);
    assert.match(loose[1] ?? '', /Home Screen/);
  });
});

describe('Backup’s words', () => {
  test('never say wallet, cash, coin, account or pay', () => {
    assert.ok(said.length > 20, `${said.length} lines checked`);
    for (const line of said) {
      assert.doesNotMatch(line, FORBIDDEN, line);
    }
  });
});
