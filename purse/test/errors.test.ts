import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { linkWords, say } from '../src/errors.ts';

const context = { host: 'mint.example.org', perHour: 10 };

/** An error as the SDK makes one: a WorglError with a code, or a MintError with a status and code. */
const worgl = (code: string): Error => Object.assign(new Error(`raw ${code}`), { name: 'WorglError', code, details: {} });
const mint = (status: number, code: string, retryAfterMs?: number): Error =>
  Object.assign(new Error(`raw ${code} (HTTP ${status})`), { name: 'MintError', status, code, retryAfterMs });

describe('errors', () => {
  test('from reading a mint name the server, never the raw message', () => {
    assert.equal(say(mint(404, 'unknown_mint'), 'read', context).words, 'The server at mint.example.org does not list the mint this link names.');
    assert.equal(say(mint(404, 'http'), 'read', context).words, 'The server at mint.example.org does not list the mint this link names.');
    assert.equal(
      say(new TypeError('Failed to fetch'), 'read', context).words,
      'The mint at mint.example.org did not answer. Check the link, or try again later.',
    );
    assert.equal(
      say(new DOMException('timed out', 'TimeoutError'), 'read', context).words,
      'The mint at mint.example.org did not answer. Check the link, or try again later.',
    );
    assert.equal(
      say(mint(503, 'unavailable'), 'read', context).words,
      'The mint at mint.example.org did not answer. Check the link, or try again later.',
    );
  });

  test('from Holdings say that the purse asks again', () => {
    assert.equal(
      say(new TypeError('Failed to fetch'), 'open', context).words,
      'The mint at mint.example.org did not answer. The purse asks again in a minute.',
    );
    assert.equal(say(mint(404, 'unknown_mint'), 'open', context).words, 'The server at mint.example.org no longer lists this mint.');
  });

  test('from the faucet say which limit it reached', () => {
    assert.equal(say(mint(429, 'rate_limited'), 'take', context).words, 'The faucet has given its 10 taps this hour.');
    assert.equal(say(mint(429, 'rate_limited'), 'take', { host: 'x', perHour: 1 }).words, 'The faucet has given its 1 tap this hour.');
    assert.equal(say(mint(429, 'exhausted'), 'take', context).words, 'The faucet has given out its notes for today.');
    assert.equal(say(mint(500, 'internal'), 'take', context).words, 'The faucet did not answer. Try again in a moment.');
    assert.equal(say(new TypeError('Failed to fetch'), 'take', context).words, 'The faucet did not answer. Try again in a moment.');
    assert.deepEqual(say(mint(403, 'unknown_origin'), 'take', context), {
      words: 'This faucet does not hand out notes to this purse.',
      noFaucet: true,
    });
    assert.equal(say(worgl('faucet_amount'), 'take', context).noFaucet, true);
    assert.deepEqual(say(Object.assign(new Error('raw'), { name: 'FaucetChanged' }), 'take', context), {
      words: 'This faucet hands out another mint’s notes now. Nothing was taken.',
      noFaucet: true,
    });
    assert.equal(say(worgl('bad_signature'), 'take', context).words, 'That server’s answer did not check out. Nothing was kept.');
  });

  test('from the faucet say when to try again, by its Retry-After', () => {
    const now = new Date(2026, 9, 21, 14, 32).getTime();
    assert.equal(
      say(mint(429, 'rate_limited', 23 * 60_000), 'take', { ...context, now }).words,
      'The faucet has given its 10 taps this hour. Try again at 14:55.',
    );
    assert.equal(
      say(mint(429, 'exhausted', 9.5 * 3_600_000), 'take', { ...context, now }).words,
      'The faucet has given out its notes for today. Try again at Thu 00:02.',
    );
    assert.equal(say(mint(429, 'rate_limited'), 'take', { ...context, now }).words, 'The faucet has given its 10 taps this hour.');
  });

  test('from the store say what to do', () => {
    assert.match(say(worgl('store_blocked'), 'open', context).words, /Close its other tabs/);
    assert.deepEqual(say(worgl('store_changed'), 'take', context), {
      words: 'Another tab changed the purse meanwhile. Nothing was kept; try again.',
      reopen: true,
    });
    assert.match(say(worgl('unknown_format'), 'open', context).words, /Reload the page/);
    assert.match(say(worgl('malformed'), 'open', context).words, /cannot be read/);
    assert.equal(
      say(new DOMException('full', 'QuotaExceededError'), 'take', context).words,
      'This browser refused to save. Nothing changed.',
    );
    assert.match(say(new DOMException('no', 'NotSupportedError'), 'open', context).words, /cannot keep tokens here/);
  });

  test('that nothing names still read plainly', () => {
    for (const error of [new Error('boom'), 'boom', undefined, worgl('policy'), { name: 'Mystery' }]) {
      const { words } = say(error, 'open', context);
      assert.equal(words, 'Something went wrong. Try again in a moment.');
    }
  });

  test('for a link say what to paste', () => {
    assert.equal(linkWords('not_a_link'), 'Paste the whole mint link, starting with https://');
    assert.equal(linkWords('key'), 'Paste the whole mint link, starting with https://');
    assert.match(linkWords('insecure'), /only over https/);
  });
});
