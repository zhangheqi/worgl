import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { hostOf, isMintHash, mintHash, parseMintLink, shortKey } from '../src/link.ts';

const KEY = '0d4e4453'.padEnd(64, 'a7');

describe('mint links', () => {
  test('read from a whole URL, from the fragment, or from the part after #', () => {
    const want = { ok: true, link: { mint: KEY, server: 'https://mint.example.org' } };
    for (const text of [
      `https://purse.worgl.1y.io/#mint=${KEY}&at=https%3A%2F%2Fmint.example.org`,
      `#mint=${KEY}&at=https%3A%2F%2Fmint.example.org%2F`,
      `mint=${KEY}&at=https://mint.example.org`,
      `  #at=https%3A%2F%2Fmint.example.org&mint=${KEY}\n`,
    ]) {
      assert.deepEqual(parseMintLink(text), want, text);
    }
  });

  test('take the key in either case and keep it in lowercase', () => {
    const parsed = parseMintLink(`#mint=${KEY.toUpperCase()}&at=https%3A%2F%2Fmint.example.org`);
    assert.deepEqual(parsed, { ok: true, link: { mint: KEY, server: 'https://mint.example.org' } });
  });

  test('keep a server path and port, without its trailing slash', () => {
    const parsed = parseMintLink(`#mint=${KEY}&at=${encodeURIComponent('https://x.example:8443/worgl/')}`);
    assert.deepEqual(parsed, { ok: true, link: { mint: KEY, server: 'https://x.example:8443/worgl' } });
  });

  test('drop an empty query or fragment from the server', () => {
    for (const [at, server] of [
      ['https://x.example/?', 'https://x.example'],
      ['https://x.example/#', 'https://x.example'],
      ['https://x.example/path?', 'https://x.example/path'],
      ['https://x.example/path/?#', 'https://x.example/path'],
    ] as const) {
      assert.deepEqual(parseMintLink(`#mint=${KEY}&at=${encodeURIComponent(at)}`), { ok: true, link: { mint: KEY, server } }, at);
    }
  });

  test('refuse a text that names no mint or no server', () => {
    for (const text of ['', 'hello', 'https://mint.example.org', `#mint=${KEY}`, '#at=https%3A%2F%2Fx.example', `#mint=&at=https%3A%2F%2Fx`]) {
      assert.deepEqual(parseMintLink(text), { ok: false, problem: 'not_a_link' }, text);
    }
  });

  test('refuse a key that is not 64 hex digits', () => {
    for (const key of [KEY.slice(1), `${KEY}0`, `${KEY.slice(1)}g`, `0x${KEY.slice(2)}`]) {
      assert.deepEqual(parseMintLink(`#mint=${key}&at=https%3A%2F%2Fx.example`), { ok: false, problem: 'key' }, key);
    }
  });

  test('refuse a server that is not a plain http or https URL', () => {
    for (const at of ['mint.example.org', 'ftp://x.example', 'javascript:alert(1)', 'https://user:pw@x.example', 'https://x.example/?a=1', 'https://x.example/#top']) {
      assert.deepEqual(
        parseMintLink(`#mint=${KEY}&at=${encodeURIComponent(at)}`),
        { ok: false, problem: 'server' },
        at,
      );
    }
  });

  test('take plain http only from this machine', () => {
    for (const at of ['http://127.0.0.1:8080', 'http://localhost:8080', 'http://mint.localhost', 'http://[::1]:8080']) {
      const parsed = parseMintLink(`#mint=${KEY}&at=${encodeURIComponent(at)}`);
      assert.equal(parsed.ok, true, at);
    }
    for (const at of ['http://mint.example.org', 'http://192.168.1.20:8080', 'http://127.0.0.1.example']) {
      assert.deepEqual(
        parseMintLink(`#mint=${KEY}&at=${encodeURIComponent(at)}`),
        { ok: false, problem: 'insecure' },
        at,
      );
    }
  });

  test('are written back as they read', () => {
    const link = { mint: KEY, server: 'http://127.0.0.1:8080' };
    assert.equal(mintHash(link), `#mint=${KEY}&at=http%3A%2F%2F127.0.0.1%3A8080`);
    assert.deepEqual(parseMintLink(mintHash(link)), { ok: true, link });
  });

  test('are told from other fragments', () => {
    assert.equal(isMintHash(`#mint=${KEY}&at=x`), true);
    assert.equal(isMintHash('#mint=bad'), true);
    assert.equal(isMintHash('#home'), false);
    assert.equal(isMintHash(''), false);
  });

  test('name their server by its host and their mint by eight digits', () => {
    assert.equal(hostOf('https://mint.example.org/worgl'), 'mint.example.org');
    assert.equal(hostOf('http://127.0.0.1:8080'), '127.0.0.1:8080');
    assert.equal(shortKey(KEY), '0d4e4453');
  });
});
