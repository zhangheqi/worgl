import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, test } from 'node:test';
import { inflateSync } from 'node:zlib';

import { TILES, grainPng } from '../scripts/grain.ts';

describe('the grain', () => {
  test('in public/ is what scripts/grain.ts draws', () => {
    for (const [name, { dark, light, density }] of Object.entries(TILES)) {
      const saved = readFileSync(new URL(`../public/${name}`, import.meta.url));
      assert.ok(saved.equals(grainPng(dark, light, density)), `${name}: run npm run grain`);
    }
  });

  test('is a 180 px tile with about as many specks as it asks for', () => {
    const png = grainPng(22, 18, 0.45);
    assert.equal(png.readUInt32BE(16), 180);
    assert.equal(png.readUInt32BE(20), 180);
    const at = png.indexOf('IDAT');
    const pixels = inflateSync(png.subarray(at + 4, at + 4 + png.readUInt32BE(at - 4)));
    let specks = 0;
    for (let row = 0; row < 180; row++) {
      for (let byte = 1; byte <= 45; byte++) {
        const packed = pixels[row * 46 + byte] ?? 0;
        for (let shift = 0; shift < 8; shift += 2) {
          specks += (packed >> shift) & 3 ? 1 : 0;
        }
      }
    }
    assert.ok(Math.abs(specks / (180 * 180) - 0.45) < 0.02, `${specks} specks`);
  });
});
