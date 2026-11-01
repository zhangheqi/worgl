// Draws the purse's app icons: a paper stamp, perforated, on the cobalt,
// with the favicon's W in bits at its middle. The stamp the purse's mints
// wear (Mints), the colours of its bands (styles.css), and the favicon's W
// (public/favicon.svg), so the Home Screen icon and the tab's are one mark.
// Drawn here once, to public/, as the grain is, so the build needs no
// rasteriser:
//
//   node scripts/icons.ts
//
// test/icons.test.ts checks that the files in public/ are what this draws.

import { writeFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { crc32, deflateSync } from 'node:zlib';

/** The cobalt and the paper of styles.css, as RGB. */
const COBALT = [0x1d, 0x35, 0xd6] as const;
const PAPER = [0xf3, 0xf3, 0xef] as const;

/**
 * The favicon's W: its 3-unit squares on a 27-unit grid, by their top-left
 * corners (public/favicon.svg). It spans 19 units each way, from 4 to 23.
 */
const W: readonly (readonly [number, number])[] = [
  [4, 4], [20, 4],
  [4, 8], [20, 8],
  [4, 12], [12, 12], [20, 12],
  [4, 16], [12, 16], [20, 16],
  [8, 20], [16, 20],
];

/**
 * The icons: their file, side in pixels, and how much of the side the
 * stamp takes. A maskable icon may be cut to a circle 80% of its side
 * across, so its stamp stays inside that; the others may show it larger.
 */
export const ICONS = {
  'icon-192.png': { side: 192, stamp: 0.7 },
  'icon-512.png': { side: 512, stamp: 0.7 },
  'icon-maskable-512.png': { side: 512, stamp: 0.56 },
  'apple-touch-icon.png': { side: 180, stamp: 0.7 },
} as const;

/** Bites along each edge of the stamp, and their radius as a share of the spacing between them. */
const BITES = 9;
const BITE = 0.32;
/** The W's width as a share of the stamp's side. */
const W_SHARE = 0.54;
/** Samples a pixel is split into, each way, for its edges' coverage. */
const SAMPLES = 4;

/** Whether the point (x, y), in units of the icon's side, lies on paper: in the stamp, not bitten out of its edge, and not in the W. */
function onPaper(x: number, y: number, stamp: number): boolean {
  const lo = (1 - stamp) / 2;
  const hi = lo + stamp;
  if (x < lo || x > hi || y < lo || y > hi) {
    return false;
  }
  // the perforation: a row of bites centred on each edge, a bite at each corner
  const step = stamp / (BITES - 1);
  const r = step * BITE;
  for (const edge of [lo, hi]) {
    for (const [along, across] of [
      [x, y],
      [y, x],
    ] as const) {
      if (Math.abs(across - edge) < r) {
        const nearest = lo + Math.round((along - lo) / step) * step;
        if ((along - nearest) ** 2 + (across - edge) ** 2 < r * r) {
          return false;
        }
      }
    }
  }
  // the W, centred: 19 grid units across
  const unit = (stamp * W_SHARE) / 19;
  const gx = (x - 0.5) / unit + 13.5;
  const gy = (y - 0.5) / unit + 13.5;
  return !W.some(([wx, wy]) => gx >= wx && gx < wx + 3 && gy >= wy && gy < wy + 3);
}

/** An icon as a PNG: RGB, eight bits a channel, each pixel the paper's share of its samples over the cobalt. */
export function iconPng(side: number, stamp: number): Buffer {
  const stride = 1 + side * 3;
  const pixels = Buffer.alloc(stride * side);
  for (let row = 0; row < side; row++) {
    for (let column = 0; column < side; column++) {
      let paper = 0;
      for (let sy = 0; sy < SAMPLES; sy++) {
        for (let sx = 0; sx < SAMPLES; sx++) {
          if (onPaper((column + (sx + 0.5) / SAMPLES) / side, (row + (sy + 0.5) / SAMPLES) / side, stamp)) {
            paper++;
          }
        }
      }
      const share = paper / (SAMPLES * SAMPLES);
      const at = row * stride + 1 + column * 3;
      for (let c = 0; c < 3; c++) {
        pixels[at + c] = Math.round((COBALT[c] ?? 0) + ((PAPER[c] ?? 0) - (COBALT[c] ?? 0)) * share);
      }
    }
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(side, 0);
  header.writeUInt32BE(side, 4);
  header.set([8, 2, 0, 0, 0], 8); // bit depth 8, RGB, deflate, adaptive filters, no interlace
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', header),
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
  for (const [name, { side, stamp }] of Object.entries(ICONS)) {
    const file = fileURLToPath(new URL(`../public/${name}`, import.meta.url));
    writeFileSync(file, iconPng(side, stamp));
    console.log(`wrote ${file}`);
  }
}
