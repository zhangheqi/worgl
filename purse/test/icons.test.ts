import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, test } from 'node:test';
import { inflateSync } from 'node:zlib';

import { ICONS, iconPng } from '../scripts/icons.ts';

/** The pixel at (x, y) of an RGB PNG that iconPng drew, as [r, g, b]. */
function pixel(png: Buffer, x: number, y: number): number[] {
  const side = png.readUInt32BE(16);
  const at = png.indexOf('IDAT');
  const pixels = inflateSync(png.subarray(at + 4, at + 4 + png.readUInt32BE(at - 4)));
  const start = y * (1 + side * 3) + 1 + x * 3;
  return [...pixels.subarray(start, start + 3)];
}

const COBALT = [0x1d, 0x35, 0xd6];
const PAPER = [0xf3, 0xf3, 0xef];

describe('the app icons', () => {
  test('in public/ are what scripts/icons.ts draws', () => {
    for (const [name, { side, stamp }] of Object.entries(ICONS)) {
      const saved = readFileSync(new URL(`../public/${name}`, import.meta.url));
      assert.ok(saved.equals(iconPng(side, stamp)), `${name}: run npm run icons`);
    }
  });

  test('are the sizes the manifest and the Home Screen ask for', () => {
    const manifest = JSON.parse(readFileSync(new URL('../public/manifest.webmanifest', import.meta.url), 'utf8')) as {
      icons: { src: string; sizes: string; purpose?: string }[];
    };
    for (const { src, sizes } of manifest.icons) {
      const icon = ICONS[src.slice(1) as keyof typeof ICONS];
      assert.ok(icon !== undefined, `${src} is drawn by scripts/icons.ts`);
      assert.equal(sizes, `${icon.side}x${icon.side}`);
    }
    assert.equal(ICONS['apple-touch-icon.png'].side, 180);
  });

  test('are a paper stamp on the cobalt, with the W in cobalt at its middle', () => {
    const png = iconPng(192, 0.7);
    assert.deepEqual(pixel(png, 4, 4), COBALT, 'the ground');
    // the stamp's paper, between the W's squares and the perforated edge
    assert.deepEqual(pixel(png, 96, 50), PAPER, 'the stamp');
    // the W's middle stroke, a little under the centre
    assert.deepEqual(pixel(png, 96, 100), COBALT, 'the W');
  });

  test('keep a maskable stamp inside the circle a launcher may cut it to', () => {
    const { side, stamp } = ICONS['icon-maskable-512.png'];
    // the stamp's corner, from the centre, as a share of the side: inside the 80% circle's radius
    assert.ok((stamp / 2) * Math.SQRT2 < 0.4);
    const png = iconPng(side, stamp);
    // on the circle's edge along the diagonal there is only the ground
    const edge = Math.round(side / 2 + side * 0.4 * Math.SQRT1_2);
    assert.deepEqual(pixel(png, edge, edge), COBALT);
  });
});
