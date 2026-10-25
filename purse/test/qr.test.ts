import assert from 'node:assert/strict';
import { before, describe, test } from 'node:test';

import { DENSE_UP_TO, MIN_MODULE, QR_BASE, loadQr, moduleSize, qrBase, qrCode, qrPath, qrPlan, qrReady } from '../src/qr.ts';

const ABC = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';

/** A hand-back link of `length` characters, as random as a real one. */
function link(length: number): string {
  let seed = 7;
  let text = 'https://purse.worgl.1y.io/#take=';
  while (text.length < length) {
    seed = (seed * 1103515245 + 12345) >>> 0;
    text += ABC[seed >>> 26];
  }
  return text;
}

describe('QR codes', () => {
  before(async () => {
    assert.equal(qrPlan('x').kind, 'none', 'nothing is drawn before the encoder loads');
    assert.equal(await loadQr(), true);
    assert.equal(qrReady(), true);
  });

  test('are plain to v20, dense to v30, and none past that', () => {
    assert.deepEqual(qrPlan(link(261)), { version: 10, kind: 'plain' });
    assert.deepEqual(qrPlan(link(845)), { version: 20, kind: 'plain' });
    assert.deepEqual(qrPlan(link(1278)), { version: 26, kind: 'dense' });
    assert.deepEqual(qrPlan(link(1972)), { kind: 'none' });
    assert.equal(qrCode(link(1972))?.version, 33);
    assert.deepEqual(qrPlan(link(3300)), { kind: 'none' });
    assert.equal(qrCode(link(3300)), undefined);
  });

  test('draw whole pixels a module, inside a quiet zone of four', () => {
    assert.equal(moduleSize(57, 267), 4);
    assert.equal(moduleSize(145, 290), 1);
    assert.equal(moduleSize(145, 10), 1);
  });

  test('start dense codes large enough for two pixels a module, up to the densest drawn', () => {
    const modules = (version: number): number => 17 + 4 * version;
    assert.equal(qrBase(undefined), QR_BASE);
    assert.equal(qrBase(modules(10)), 267);
    assert.equal(moduleSize(modules(27), qrBase(modules(27))), 2);
    for (const version of [28, 29, DENSE_UP_TO]) {
      assert.equal(moduleSize(modules(version), qrBase(modules(version))), MIN_MODULE, `v${version}`);
    }
    // the densest still fits a 360 px phone's stamp: 328 px of column less the stamp's 14 px each side
    assert.equal(qrBase(modules(DENSE_UP_TO)), 290);
    assert.ok(qrBase(modules(DENSE_UP_TO)) <= 328 - 28);
  });

  test('draw one rectangle a run of dark modules on a row', () => {
    const dark = [
      [true, true, false],
      [false, false, false],
      [true, false, true],
    ];
    assert.equal(qrPath((row, col) => dark[row]?.[col] === true, 3, 2), 'M8 8h4v2h-4zM8 12h2v2h-2zM12 12h2v2h-2z');
    assert.equal(qrPath(() => false, 3, 2), '');
  });
});
