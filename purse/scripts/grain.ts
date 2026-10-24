// Draws the print grain the paper and cobalt bands wear: a 180 x 180 tile of
// light and dark specks from a seeded generator, so every build draws the
// same pattern. Drawn here once, to public/grain-light.png and
// public/grain-dark.png, so the page needs no `data:` images and no canvas.
//
//   node scripts/grain.ts
//
// test/grain.test.ts checks that the files in public/ are what this draws.

import { writeFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { crc32, deflateSync } from 'node:zlib';

import { rng } from '../src/rng.ts';

/** The tile's side, in pixels. */
const SIDE = 180;

/** The two tiles: the alpha of a dark and a light speck, and how many pixels hold one. */
export const TILES = {
  'grain-light.png': { dark: 22, light: 18, density: 0.45 },
  'grain-dark.png': { dark: 30, light: 9, density: 0.38 },
} as const;

/**
 * The tile as a PNG: each pixel clear, or a speck of ink (#111111) or of
 * paper (#FFFFFF) at its alpha. Three colours, so a two-bit palette holds it.
 */
export function grainPng(dark: number, light: number, density: number): Buffer {
  const random = rng(42);
  // Two bits a pixel, four pixels a byte, and a filter byte (0, none) before each row.
  const stride = 1 + SIDE / 4;
  const pixels = Buffer.alloc(stride * SIDE);
  for (let i = 0; i < SIDE * SIDE; i++) {
    // The order of the draws fixes the pattern: first which ink, then whether there is a speck.
    const isDark = random() < 0.5;
    const index = random() < density ? (isDark ? 1 : 2) : 0;
    const row = Math.floor(i / SIDE);
    const column = i % SIDE;
    const at = row * stride + 1 + Math.floor(column / 4);
    pixels[at] = (pixels[at] ?? 0) | (index << (6 - 2 * (column % 4)));
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(SIDE, 0);
  header.writeUInt32BE(SIDE, 4);
  header.set([2, 3, 0, 0, 0], 8); // bit depth 2, palette, deflate, adaptive filters, no interlace
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', header),
    chunk('PLTE', Buffer.from([0, 0, 0, 0x11, 0x11, 0x11, 0xff, 0xff, 0xff])),
    chunk('tRNS', Buffer.from([0, dark, light])),
    chunk('IDAT', deflateSync(pixels, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

/** One PNG chunk: its length, type, data and checksum. */
function chunk(type: string, data: Buffer): Buffer {
  const body = Buffer.concat([Buffer.from(type, 'latin1'), data]);
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const check = Buffer.alloc(4);
  check.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, check]);
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  for (const [name, { dark, light, density }] of Object.entries(TILES)) {
    const file = fileURLToPath(new URL(`../public/${name}`, import.meta.url));
    writeFileSync(file, grainPng(dark, light, density));
    console.log(`wrote ${file}`);
  }
}
