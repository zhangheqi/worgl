import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { when } from '../src/format.ts';
import { asAtWords, holdsAt, isInfoUrl, skewOf } from '../src/offline.ts';

const MINT = 'ab'.repeat(32);
// Sun 1 Nov 2026, 14:02 local time
const AT = new Date(2026, 10, 1, 14, 2).getTime();

describe('offline', () => {
  test('keeps only a mint’s info answers', () => {
    assert.ok(isInfoUrl(`https://mint.example.org/v1/mints/${MINT}/info`));
    assert.ok(isInfoUrl(`https://mint.example.org/base/v1/mints/${MINT}/info`));
    assert.ok(!isInfoUrl(`https://mint.example.org/v1/mints/${MINT}/transfers`));
    assert.ok(!isInfoUrl(`https://mint.example.org/v1/mints/${MINT}/info?x=1`));
    assert.ok(!isInfoUrl('https://mint.example.org/v1/mints/abc/info'));
    assert.ok(!isInfoUrl(`https://mint.example.org/v1/mints/${MINT.toUpperCase()}/info`));
  });

  test('reads the mint’s clock from a kept answer’s Date, in whole seconds', () => {
    const date = (ms: number): string => new Date(ms).toUTCString();
    // Date counts whole seconds: under two is none
    assert.equal(skewOf(date(AT + 1999), AT), 0);
    assert.equal(skewOf(date(AT - 1000), AT), 0);
    assert.equal(skewOf(date(AT + 90_000), AT), 90_000);
    assert.equal(skewOf(date(AT - 3_600_000), AT), -3_600_000);
    // no Date, or one the purse cannot read, says nothing of the mint's clock
    assert.equal(skewOf(null, AT), 0);
    assert.equal(skewOf('yesterday', AT), 0);
  });

  test('holds what a mint said until its next scheduled rotation, by the mint’s clock', () => {
    const nextAt = BigInt(Math.floor(AT / 1000) + 3600);
    assert.ok(holdsAt({ nextAt }, false, AT));
    assert.ok(holdsAt({ nextAt }, false, AT + 3_599_000));
    assert.ok(!holdsAt({ nextAt }, false, AT + 3_600_000));
    // a mint that rotates by hand, or is paused, rotates when its operator says
    assert.ok(holdsAt(undefined, false, AT + 365 * 86_400_000));
    assert.ok(holdsAt({ nextAt }, true, AT + 86_400_000));
  });

  test('says when the purse last reached the mint, over what it shows', () => {
    assert.equal(asAtWords('Playground', AT, AT + 60_000), `Playground · as at ${when(AT, AT + 60_000)}`);
    assert.match(asAtWords('Playground', AT, AT + 60_000), /^Playground · as at 14:02$/);
  });
});
