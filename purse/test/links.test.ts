import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { fallbackDeflate, fallbackInflate, nativeDeflate, nativeInflate } from '../src/deflate.ts';
import {
  MAX_INFLATED,
  MAX_TEXT,
  base64url,
  decode,
  encodeAsk,
  encodeTake,
  fromBase64url,
  linkFor,
} from '../src/links.ts';

const SERVER = 'https://playground.worgl.1y.io';
const PURSE = 'https://purse.worgl.1y.io/';

/** A seeded stream of bytes, so the tests draw the same keys each run. */
function bytes(seed: number): (n: number) => Uint8Array {
  let s = seed;
  return (n) => {
    const out = new Uint8Array(n);
    for (let i = 0; i < n; i++) {
      s = (s * 1103515245 + 12345) >>> 0;
      out[i] = s >>> 24;
    }
    return out;
  };
}

const hex = (b: Uint8Array): string => [...b].map((x) => x.toString(16).padStart(2, '0')).join('');

const random = bytes(7);
const MINT = hex(random(32));
const ASK = hex(random(32));

/** A request's JSON in the shape the SDK writes it, for `amounts`. */
const requestJson = (amounts: readonly number[]): string =>
  JSON.stringify({ mint: MINT, outputs: amounts.map((amount) => ({ amount, holder: hex(random(32)) })) });

/** `n` notes of 190 base64url characters each. */
const notes190 = (n: number): string[] => Array.from({ length: n }, () => base64url(random(143)).slice(0, 190));

/**
 * `n` notes as the SDK writes them: a version byte, the mint's key, the
 * serial (generation, index), the amount, the holder's key and the
 * signature, 149 bytes in base64url (199 characters).
 */
function realNotes(n: number): string[] {
  return Array.from({ length: n }, (_, i) => {
    const note = new Uint8Array(149);
    note[0] = 1;
    note.set(Uint8Array.from(MINT.match(/../g) ?? [], (x) => Number.parseInt(x, 16)), 1);
    note[40] = 3;
    note.set(random(2), 43);
    note[52] = 2 ** (i % 7);
    note.set(random(96), 53);
    return base64url(note);
  });
}

/** The packed part of a link. */
const packedOf = (link: string): string => link.slice(link.indexOf('=') + 1);

/** A frame as given, deflated and written as a packed part. */
const rawPacked = async (frame: string): Promise<string> => base64url(await nativeDeflate(new TextEncoder().encode(frame)));

describe('links', () => {
  test('carry a request there and back, for 1 to 256 notes', async () => {
    for (const n of [1, 2, 7, 34, 256]) {
      const json = requestJson(Array.from({ length: n }, (_, i) => 2 ** (i % 7)));
      const link = linkFor('ask', await encodeAsk(SERVER, json), PURSE);
      assert.deepEqual(await decode(link), { ok: true, link: { kind: 'ask', server: SERVER, json } }, `${n} notes`);
    }
  });

  test('carry a hand-back there and back, for 1 to 256 notes', async () => {
    for (const n of [1, 8, 256]) {
      const notes = notes190(n);
      const link = linkFor('take', await encodeTake(SERVER, MINT, ASK, 5 * n, notes), PURSE);
      assert.deepEqual(
        await decode(link),
        { ok: true, link: { kind: 'take', server: SERVER, mint: MINT, ask: ASK, amount: 5 * n, notes } },
        `${n} notes`,
      );
    }
  });

  test('read from any host, from a bare fragment, or wrapped by a chat app', async () => {
    const json = requestJson([4, 1]);
    const p = await encodeAsk(SERVER, json);
    const want = { ok: true, link: { kind: 'ask', server: SERVER, json } };
    const wrapped = `${p.slice(0, 40)} \n ${p.slice(40, 90)}\n\t${p.slice(90)}`;
    for (const text of [
      `https://purse.worgl.1y.io/#ask=${p}`,
      `http://127.0.0.1:4173/#ask=${p}`,
      `#ask=${p}`,
      `ask=${p}`,
      `  #ask=${wrapped}  `,
      `#ask=${p}&x=1`,
      `#ask=${p}${'='.repeat((4 - (p.length % 4)) % 4)}`,
    ]) {
      assert.deepEqual(await decode(text), want, text);
    }
  });

  test('tell a request from a hand-back, a mint link and plain text', async () => {
    const take = await encodeTake(SERVER, MINT, ASK, 5, notes190(1));
    assert.deepEqual(await decode(`#ask=${take}`), { ok: false, problem: 'unreadable', kind: 'ask' });
    assert.deepEqual(await decode(`${PURSE}#mint=${MINT}&at=${encodeURIComponent(SERVER)}`), { ok: false, problem: 'mint_link' });
    assert.deepEqual(await decode('mint=abc'), { ok: false, problem: 'mint_link' });
    assert.deepEqual(await decode('hello there'), { ok: false, problem: 'not_a_link' });
    assert.deepEqual(await decode(''), { ok: false, problem: 'empty' });
    assert.deepEqual(await decode('   '), { ok: false, problem: 'empty' });
  });

  test('say when a newer purse wrote them, and refuse versions that never were', async () => {
    const json = requestJson([4]);
    assert.deepEqual(await decode(`#ask=${await rawPacked(`ask/2\n${SERVER}\n${json}`)}`), { ok: false, problem: 'newer', kind: 'ask' });
    for (const head of ['ask/0', 'ask/x', 'ask', 'ask/12345', 'Ask/1']) {
      assert.deepEqual(
        await decode(`#ask=${await rawPacked(`${head}\n${SERVER}\n${json}`)}`),
        { ok: false, problem: 'unreadable', kind: 'ask' },
        head,
      );
    }
  });

  test('refuse fields that do not read', async () => {
    const json = requestJson([4]);
    const note = notes190(1)[0] ?? '';
    const take = (lines: readonly string[]): Promise<string> => rawPacked(['take/1', ...lines].join('\n'));
    const unreadable = async (kind: 'ask' | 'take', packed: string, why: string): Promise<void> => {
      assert.deepEqual(await decode(`#${kind}=${packed}`), { ok: false, problem: 'unreadable', kind }, why);
    };
    await unreadable('ask', await rawPacked(`ask/1\n${SERVER}\n${json}\nmore`), 'an ask of 4 lines');
    await unreadable('ask', await rawPacked(`ask/1\n${SERVER}\n{`), 'an ask with no JSON');
    await unreadable('take', await take([SERVER, MINT, ASK, '5']), 'a take with no notes');
    await unreadable('take', await take([SERVER, MINT, ASK, '5', ...notes190(257)]), '257 notes');
    for (const amount of ['0', '007', '-1', '1.5', '', '12345678901234567']) {
      await unreadable('take', await take([SERVER, MINT, ASK, amount, note]), `amount ${amount}`);
    }
    await unreadable('take', await take([SERVER, MINT.slice(1), ASK, '5', note]), 'a 63-digit mint');
    await unreadable('take', await take([SERVER, MINT, `${ASK.slice(1)}g`, '5', note]), 'an ask id that is not hex');
    await unreadable('take', await take([SERVER, MINT, ASK, '5', 'a b']), 'a note with a space');
    await unreadable('take', await take([SERVER, MINT, ASK, '5', '']), 'an empty note');
    await unreadable('take', await take([SERVER, MINT, ASK, '5', 'x'.repeat(4097)]), 'a note too long');
  });

  test('take the mint and the request in either case, and keep them in lowercase', async () => {
    const notes = notes190(1);
    const p = await rawPacked(['take/1', SERVER, MINT.toUpperCase(), ASK.toUpperCase(), '5', ...notes].join('\n'));
    assert.deepEqual(await decode(`#take=${p}`), {
      ok: true,
      link: { kind: 'take', server: SERVER, mint: MINT, ask: ASK, amount: 5, notes },
    });
  });

  test('name only a server the purse can reach, as the client writes it', async () => {
    const json = requestJson([4]);
    for (const server of ['http://example.com', 'ftp://x', 'https://u@x', 'not a url']) {
      assert.deepEqual(await decode(`#ask=${await rawPacked(`ask/1\n${server}\n${json}`)}`), { ok: false, problem: 'server', kind: 'ask' }, server);
    }
    assert.deepEqual(await decode(`#ask=${await rawPacked(`ask/1\nhttps://x/\n${json}`)}`), {
      ok: true,
      link: { kind: 'ask', server: 'https://x', json },
    });
  });

  test('refuse what is too long, a bomb, or not base64url', async () => {
    const longest = `#ask=${'A'.repeat(MAX_TEXT - 5)}`;
    assert.equal(longest.length, MAX_TEXT);
    assert.deepEqual(await decode(longest), { ok: false, problem: 'unreadable', kind: 'ask' });
    assert.deepEqual(await decode(`${longest}A`), { ok: false, problem: 'too_long' });
    const bomb = await rawPacked('a'.repeat(200_000));
    assert.ok(bomb.length < 400, `the bomb is small (${bomb.length})`);
    let most = 0;
    const counting = (input: Uint8Array, cap: number): Promise<Uint8Array> =>
      nativeInflate(input, cap, (size) => {
        most = Math.max(most, size);
      });
    assert.deepEqual(await decode(`#ask=${bomb}`, counting), { ok: false, problem: 'unreadable', kind: 'ask' });
    assert.ok(most > MAX_INFLATED && most <= MAX_INFLATED + 65_536, `stopped within a chunk past the cap (${most})`);
    let fallbackMost = 0;
    await assert.rejects(
      fallbackInflate(fromBase64url(bomb) ?? new Uint8Array(), MAX_INFLATED, (size) => {
        fallbackMost = Math.max(fallbackMost, size);
      }),
      RangeError,
    );
    assert.ok(fallbackMost <= MAX_INFLATED + 65_536, `the fallback stopped too (${fallbackMost})`);
    const p = await encodeAsk(SERVER, requestJson([4]));
    for (const bad of [`${p.slice(0, 10)}+${p.slice(11)}`, `${p.slice(0, 10)}/${p.slice(11)}`, `${p}A`.slice(0, 4 * Math.floor(p.length / 4) + 1)]) {
      assert.equal((await decode(`#ask=${bad}`)).ok, false, bad);
    }
    assert.deepEqual(await decode(`#ask=${p.slice(0, 10)}+${p.slice(11)}`), { ok: false, problem: 'unreadable', kind: 'ask' });
  });

  test('are never written with a line break inside a field', async () => {
    await assert.rejects(encodeAsk(SERVER, '{"a":\n1}'), RangeError);
    await assert.rejects(encodeTake(SERVER, MINT, ASK, 5, ['abc\ndef']), RangeError);
    await assert.rejects(encodeTake(SERVER, MINT, ASK, 5, []), RangeError);
    await assert.rejects(encodeTake(SERVER, MINT, ASK, 0, ['abc']), RangeError);
  });

  test('inflate what either deflate wrote', async () => {
    const frame = new TextEncoder().encode(['take/1', SERVER, MINT, ASK, '20', ...realNotes(8)].join('\n'));
    assert.deepEqual(await fallbackInflate(await nativeDeflate(frame), MAX_INFLATED), frame);
    assert.deepEqual(await nativeInflate(await fallbackDeflate(frame), MAX_INFLATED), frame);
    assert.deepEqual(await fallbackInflate(await fallbackDeflate(frame), MAX_INFLATED), frame);
  });

  test('read the largest hand-back the purse can make: 256 notes, as Ask in smaller notes allows', async () => {
    // keys and signatures as random as real ones, which deflate does not shrink: about 36,000 characters
    const notes = realNotes(256).map((text) => {
      const bytes = fromBase64url(text) ?? new Uint8Array(149);
      bytes.set(crypto.getRandomValues(new Uint8Array(96)), 53);
      return base64url(bytes);
    });
    const link = linkFor('take', await encodeTake(SERVER, MINT, ASK, 4096, notes), PURSE);
    assert.ok(link.length > 30_000 && link.length < MAX_TEXT, `a 256-note hand-back is ${link.length} characters`);
    assert.deepEqual(await decode(link), { ok: true, link: { kind: 'take', server: SERVER, mint: MINT, ask: ASK, amount: 4096, notes } });
    // and inside a message round it
    assert.equal((await decode(`Here are your tokens: ${link} . Thanks!`)).ok, true);
  });

  test('read a link out of the message round it', async () => {
    const json = requestJson([16, 4]);
    const p = await encodeAsk(SERVER, json);
    const link = `${PURSE}#ask=${p}`;
    const want = { ok: true, link: { kind: 'ask', server: SERVER, json } };
    const wrapped = `${link.slice(0, 60)}\n${link.slice(60)}`;
    for (const text of [
      `${link}.`,
      `${link},`,
      `Here is my request: ${link} please send when you can`,
      `${link}\nthanks`,
      `${wrapped}\nthanks!`,
      `<${link}>`,
      `(${link})`,
      `"${link}"`,
      `“${link}”`,
      `[my request](${link})`,
    ]) {
      assert.deepEqual(await decode(text), want, text);
    }
    // what is not the link stays unreadable, and a newer frame is said so at once
    assert.deepEqual(await decode(`#ask=${p.slice(0, 20)} and some words`), { ok: false, problem: 'unreadable', kind: 'ask' });
    const newer = await rawPacked(`ask/2\n${SERVER}\n${json}`);
    assert.deepEqual(await decode(`#ask=${newer} thanks`), { ok: false, problem: 'newer', kind: 'ask' });
  });

  test('stay as short as measured', async () => {
    const ask = linkFor('ask', await encodeAsk(SERVER, requestJson([4, 1])), PURSE);
    assert.ok(ask.length <= 330, `a 2-note request is ${ask.length} characters`);
    const take = linkFor('take', await encodeTake(SERVER, MINT, ASK, 255, realNotes(8)), PURSE);
    assert.ok(take.length <= 1450, `an 8-note hand-back is ${take.length} characters`);
  });

  test('write base64url without padding, and read it back with or without', () => {
    for (let n = 0; n < 12; n++) {
      const b = random(n);
      const text = base64url(b);
      assert.doesNotMatch(text, /[=+/]/);
      assert.deepEqual(fromBase64url(text), b);
      assert.deepEqual(fromBase64url(`${text}${'='.repeat((4 - (text.length % 4)) % 4)}`), b);
    }
    assert.equal(fromBase64url('A'), undefined);
    assert.equal(fromBase64url('AB*C'), undefined);
  });

  test('keep the packed part of a pasted URL whole, up to the next &', async () => {
    const p = await encodeAsk(SERVER, requestJson([2]));
    const decoded = await decode(`see ${PURSE}#ask=${p}&from=chat`);
    assert.equal(decoded.ok, true);
    assert.equal(packedOf(`#ask=${p}`), p);
  });
});
