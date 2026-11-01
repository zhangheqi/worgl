import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  NAME_MAX,
  TIMETABLE,
  addedWords,
  clockWords,
  faucetRow,
  heldWords,
  keyPairs,
  nameOf,
  nameProblem,
  renamedWords,
  rotatesRow,
  startName,
  timetable,
  typedKey,
  typedServer,
} from '../src/mint-list.ts';

const MINT = '8e8efcdf'.repeat(8);
const KEY = '0123456789abcdef'.repeat(4);
const HOUR = 3_600_000;

describe('timetable', () => {
  test('the next four rotations, a period apart, the next first', () => {
    const next = Date.UTC(2026, 9, 31, 16);
    assert.deepEqual(timetable({ startMs: next - 8 * HOUR, nextMs: next, periodMs: 8 * HOUR }), [next, next + 8 * HOUR, next + 16 * HOUR, next + 24 * HOUR]);
    assert.equal(TIMETABLE, 4);
  });

  test('none for a mint that rotates by hand, or is paused', () => {
    assert.deepEqual(timetable(undefined), []);
  });

  test('none for a schedule with no period', () => {
    assert.deepEqual(timetable({ startMs: 0, nextMs: 0, periodMs: 0 }), []);
  });
});

describe('a card’s rows and clock', () => {
  test('how it rotates, and its layers', () => {
    assert.equal(rotatesRow(4, 28_800, false), 'Every 8 h · 4 layers');
    assert.equal(rotatesRow(4, undefined, false), 'By hand · 4 layers');
    assert.equal(rotatesRow(4, 28_800, true), 'Paused · 4 layers');
    assert.equal(rotatesRow(1, 1200, false), 'Every 20 min · 1 layer');
  });

  test('what a rotation does, and how long a visit keeps its tokens', () => {
    assert.equal(clockWords(28_800, false), 'Each rotation drops the first layer. A visit keeps this mint’s tokens for 8 to 16 hours.');
    assert.equal(
      clockWords(3600, false),
      'Each rotation drops the first layer. A visit keeps this mint’s tokens for 1 to 2 hours, so it suits services more than people.',
    );
  });

  test('why there is no timetable', () => {
    assert.match(clockWords(undefined, false), /^It rotates when its operator says/);
    assert.match(clockWords(28_800, true), /^Paused: it takes no transfers/);
    assert.match(clockWords(undefined, true), /^Paused/);
  });

  test('a faucet', () => {
    assert.equal(faucetRow(64, 10), '64 a tap · 10 taps an hour');
    assert.equal(faucetRow(1, 1), '1 a tap · 1 tap an hour');
  });
});

describe('keyPairs', () => {
  test('eight groups of eight, two to a pair', () => {
    assert.deepEqual(keyPairs(KEY), ['01234567 89abcdef', '01234567 89abcdef', '01234567 89abcdef', '01234567 89abcdef']);
  });

  test('every digit, in order', () => {
    assert.equal(keyPairs(MINT).join('').replaceAll(' ', ''), MINT);
  });
});

describe('a mint’s name', () => {
  test('kept composed, its spaces run together, trimmed', () => {
    assert.equal(nameOf('  Café   tokens \n'), 'Café tokens');
    assert.equal(nameOf('Café'), 'Café');
  });

  test('one is needed', () => {
    assert.match(nameProblem('   ') ?? '', /^Give it a name/);
  });

  test('of at most NAME_MAX characters, counted as characters, not code units', () => {
    assert.equal(nameProblem('x'.repeat(NAME_MAX)), undefined);
    assert.equal(nameProblem('🎟'.repeat(NAME_MAX)), undefined);
    assert.equal(nameProblem('x'.repeat(NAME_MAX + 1)), `Keep the name to ${NAME_MAX} characters or fewer.`);
  });

  test('a new mint starts with the playground’s name or its host, told apart from one taken', () => {
    assert.equal(startName(MINT, 'https://play.example', true, []), 'Playground');
    assert.equal(startName(MINT, 'https://mint.example.org:8443', false, []), 'mint.example.org:8443');
    assert.equal(startName(MINT, 'https://mint.example.org', false, ['mint.example.org']), 'mint.example.org · 8e8efcdf');
    assert.equal(startName(MINT, 'https://play.example', true, ['Playground']), 'Playground · 8e8efcdf');
  });
});

describe('a server and key typed', () => {
  test('a server with no scheme is taken as https', () => {
    assert.deepEqual(typedServer(' mint.example.org/ '), { ok: true, server: 'https://mint.example.org' });
    assert.deepEqual(typedServer('https://mint.example.org/v'), { ok: true, server: 'https://mint.example.org/v' });
  });

  test('plain http only to this machine', () => {
    assert.deepEqual(typedServer('http://127.0.0.1:8080'), { ok: true, server: 'http://127.0.0.1:8080' });
    const insecure = typedServer('http://mint.example.org');
    assert.equal(insecure.ok, false);
    assert.match(insecure.ok ? '' : insecure.words, /only over https/);
  });

  test('an empty or broken address', () => {
    for (const text of ['', '   ', 'https://', 'ftp://mint.example.org', 'https://mint.example.org/?a=1']) {
      const read = typedServer(text);
      assert.equal(read.ok, false, text);
      assert.match(read.ok ? '' : read.words, /^Type the server’s address/, text);
    }
  });

  test('a key in any case, with spaces and line breaks between groups', () => {
    assert.deepEqual(typedKey(` ${keyPairs(KEY.toUpperCase()).join('\n')} `), { ok: true, key: KEY });
  });

  test('what a key lacks', () => {
    const words = (text: string): string => {
      const read = typedKey(text);
      return read.ok ? '' : read.words;
    };
    assert.match(words(''), /^Type the mint’s key/);
    assert.equal(words(KEY.slice(1)), 'The key has 63 hex digits; a mint’s key has 64.');
    assert.equal(words('a'), 'The key has 1 hex digit; a mint’s key has 64.');
    assert.match(words(`${KEY.slice(1)}g`), /only hex digits/);
  });
});

describe('what Mints says', () => {
  test('of a mint held already, here or at another server', () => {
    assert.equal(heldWords('Playground', 'https://play.example', 'https://play.example'), 'This purse holds this mint already, as Playground.');
    assert.equal(
      heldWords('Playground', 'https://play.example', 'https://elsewhere.example:8443'),
      'This purse holds this mint already, as Playground, at play.example, and keeps it there. This names elsewhere.example:8443.',
    );
  });

  test('once one is added or renamed', () => {
    assert.equal(addedWords('mint.example.org'), 'Added mint.example.org. Holdings shows it now.');
    assert.equal(renamedWords('Club points'), 'Renamed it Club points.');
  });
});
