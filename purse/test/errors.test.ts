import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { linkWords, notRecorded, say } from '../src/errors.ts';

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

  test('from Receive and Send say what each step needs', () => {
    const offline = new TypeError('Failed to fetch');
    const down = mint(503, 'unavailable');
    // the mint is what did not answer, not the person's connection
    const ask1 = 'The mint at mint.example.org did not answer. A request needs the mint’s answer; try again in a moment.';
    const accept4 = 'The mint at mint.example.org did not answer. Taking in needs its keys. The hand-back keeps until its tokens fade, so try again later.';
    const send7 = 'The mint at mint.example.org did not answer. Nothing was sent; try again in a moment.';
    const settle12 = 'The mint at mint.example.org did not answer. This purse tries again the next time it opens; you can also try now.';
    assert.equal(say(offline, 'ask', context).words, ask1);
    assert.equal(say(down, 'ask', context).words, ask1);
    assert.equal(say(offline, 'accept', context).words, accept4);
    assert.equal(say(down, 'accept', context).words, accept4);
    assert.equal(say(offline, 'send', context).words, send7);
    assert.equal(say(down, 'send', context).words, send7);
    assert.equal(say(offline, 'settle', context).words, settle12);
    assert.equal(say(down, 'settle', context).words, settle12);
  });

  test('name what a request, a take or a send refuses', () => {
    const coded = (code: string, details: Record<string, unknown>): Error =>
      Object.assign(new Error(`raw ${code}`), { name: 'WorglError', code, details });
    assert.equal(say(coded('unsplittable', { amount: 7n }), 'ask', context).words, 'This mint cannot make 7 from its notes. Try another amount.');
    assert.equal(
      say(coded('too_many_notes', { max: 256 }), 'ask', context).words,
      'That would take more than 256 notes. Ask for less, or in larger notes.',
    );
    for (const code of ['bad_signature', 'wrong_issuer', 'unsigned']) {
      assert.equal(
        say(worgl(code), 'accept', context).words,
        'This hand-back did not check out against the mint’s keys. Nothing was taken in.',
        code,
      );
    }
    assert.equal(
      say(worgl('unknown_generation'), 'accept', context).words,
      'Some of these tokens have faded: the mint has dropped their layer. Nothing was taken in.',
    );
    assert.equal(
      say(coded('denomination', { amount: 3n }), 'send', context).words,
      'This request asks for a note of 3, which this mint does not make. Ask them for a new request.',
    );
    assert.equal(say(worgl('policy'), 'send', context).words, 'This mint’s rules refuse this send as it stands. Ask them for a new request.');
    assert.equal(notRecorded, 'This browser refused to save the request, so it was not made.');
  });

  test('keep the faucet’s words for a take only', () => {
    assert.deepEqual(say(worgl('unsplittable'), 'take', context), {
      words: 'This faucet hands out notes its mint cannot make. Nothing was kept.',
      noFaucet: true,
    });
    assert.match(say(worgl('unsplittable'), 'ask', context).words, /^This mint cannot make .* from its notes\. Try another amount\.$/);
    assert.equal(say(worgl('zero_amount'), 'ask', context).words, 'Something went wrong. Try again in a moment.');
    assert.equal(say(worgl('faucet_amount'), 'send', context).words, 'Something went wrong. Try again in a moment.');
  });

  test('keep step 1’s words for a paused mint, a full layer and the store, whatever the step', () => {
    for (const doing of ['ask', 'send', 'accept', 'settle'] as const) {
      assert.equal(say(mint(503, 'paused'), doing, context).words, 'The mint at mint.example.org is paused. Try again later.', doing);
      assert.equal(say(mint(503, 'layer_full'), doing, context).words, 'The mint’s newest layer is full. Try again after its next rotation.', doing);
      assert.deepEqual(say(worgl('store_changed'), doing, context), {
        words: 'Another tab changed the purse meanwhile. Nothing was kept; try again.',
        reopen: true,
      });
    }
  });
});
