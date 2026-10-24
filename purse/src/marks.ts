// The purse's marks: the wordmark in the site's bit glyphs, a mint's face
// drawn from its key, the legend's swatches and the close cross. All SVG,
// built with DOM calls.

import { s } from './dom.ts';
import { f2 } from './format.ts';
import { rng } from './rng.ts';

/** The site's bit glyphs for `worgl` (site/src/scripts/word.ts). */
const GLYPHS: Record<string, string[]> = {
  w: ['.....', '.....', 'X...X', 'X...X', 'X.X.X', 'X.X.X', '.X.X.', '.....', '.....'],
  o: ['.....', '.....', '.XXX.', 'X...X', 'X...X', 'X...X', '.XXX.', '.....', '.....'],
  r: ['.....', '.....', 'X.XX.', 'XX..X', 'X....', 'X....', 'X....', '.....', '.....'],
  g: ['.....', '.....', '.XXXX', 'X...X', 'X...X', 'X...X', '.XXXX', '....X', 'XXXX.'],
  l: ['XX.', '.X.', '.X.', '.X.', '.X.', '.X.', '.XX', '...', '...'],
};

/** The lit cells of `worgl`, and its size in cells. */
const WORD = (() => {
  const px: [number, number][] = [];
  let x = 0;
  for (const ch of 'worgl') {
    const glyph = GLYPHS[ch] ?? [];
    glyph.forEach((row, r) => {
      [...row].forEach((c, i) => {
        if (c === 'X') {
          px.push([x + i, r]);
        }
      });
    });
    x += (glyph[0]?.length ?? 0) + 1;
  }
  return { w: x - 1, h: 9, px };
})();

/** A square at (x, y) of side `side`, as path data. */
export const square = (x: number, y: number, side: number): string =>
  `M${f2(x)} ${f2(y)}h${f2(side)}v${f2(side)}h${f2(-side)}z`;

/** The wordmark, `cell` px a cell, in the current colour. */
export function wordmark(cell = 3, side = cell * 0.75): SVGSVGElement {
  let d = '';
  for (const [x, y] of WORD.px) {
    d += square(x * cell, y * cell, side);
  }
  const w = f2((WORD.w - 1) * cell + side);
  const h = f2((WORD.h - 1) * cell + side);
  return s(
    'svg',
    { viewBox: `0 0 ${w} ${h}`, width: w, height: h, 'aria-hidden': 'true', focusable: 'false' },
    s('path', { fill: 'currentColor', d }),
  );
}

/**
 * A mint's face: a 5 x 5 glyph, mirrored left to right, from the first
 * eight hex digits of its issuer key. It tells mints apart but proves
 * nothing: the purse compares whole keys.
 */
export function face(key: string, cell = 4, side = cell - 1): SVGSVGElement {
  const v = Number.parseInt(key.slice(0, 8), 16) || 0;
  let d = '';
  for (let r = 0; r < 5; r++) {
    for (let c = 0; c < 5; c++) {
      const cc = c < 3 ? c : 4 - c;
      if ((v >>> (r * 3 + cc)) & 1) {
        d += square(c * cell, r * cell, side);
      }
    }
  }
  const size = f2(4 * cell + side);
  return s(
    'svg',
    { class: 'face', viewBox: `0 0 ${size} ${size}`, width: size, height: size, 'aria-hidden': 'true', focusable: 'false' },
    s('path', { fill: 'currentColor', d }),
  );
}

/** The close cross. */
export function closeIcon(): SVGSVGElement {
  return s(
    'svg',
    {
      width: 18,
      height: 18,
      viewBox: '0 0 18 18',
      fill: 'none',
      stroke: 'currentColor',
      'stroke-width': 1.6,
      'aria-hidden': 'true',
      focusable: 'false',
    },
    s('path', { d: 'M3.5 3.5l11 11M14.5 3.5l-11 11' }),
  );
}

/** A legend swatch, 48 x 14: ready squares, resting outlines over a hatch, or fading bits. */
export function swatch(kind: 'ready' | 'rest' | 'fade'): SVGSVGElement {
  const pitch = 7;
  const side = 5.4;
  let d = '';
  let loose: SVGRectElement[] = [];
  const random = rng(kind === 'fade' ? 3 : 1);
  for (let i = 0; i < 8; i++) {
    const x = Math.floor(i / 2) * pitch;
    const y = (i % 2) * pitch;
    if (kind === 'ready') {
      d += square(x, y, side);
    } else if (kind === 'rest') {
      d += square(x + 0.5, y + 0.5, side - 1);
    } else {
      for (let a = 0; a < 2; a++) {
        for (let b = 0; b < 2; b++) {
          const q = random();
          const bx = x + a * 2.7;
          const by = y + b * 2.7;
          if (q < 0.5) {
            d += square(bx, by, 2.6);
          } else if (q < 0.85) {
            loose = [
              ...loose,
              s('rect', {
                x: f2(bx + 6 + random() * 14),
                y: f2(by - 3 + random() * 6),
                width: 2.6,
                height: 2.6,
                style: { opacity: String(f2(0.35 + random() * 0.5)) },
              }),
            ];
          }
        }
      }
    }
  }
  const svg = s('svg', { width: 48, height: 14, viewBox: '0 0 48 14', 'aria-hidden': 'true', focusable: 'false' });
  if (kind === 'rest') {
    let hatch = '';
    for (let x = -16; x < 52; x += 5) {
      hatch += `M${x} 16L${x + 16} 0`;
    }
    svg.append(
      s('clipPath', { id: 'legend-hatch' }, s('rect', { x: -4, y: -3, width: 56, height: 20 })),
      s('path', { class: 'lg-hatch', d: hatch, 'clip-path': 'url(#legend-hatch)' }),
    );
  }
  svg.append(
    s('path', { class: kind === 'ready' ? 'lg-rd' : kind === 'rest' ? 'lg-rs' : 'lg-fd', d }),
    s('g', { class: 'lg-fd' }, ...loose),
  );
  return svg;
}

/** The faucet's tap as squares: `min(amount, 64)` of them in four rows, as the stamp shows 64. */
export function bits(amount: number): SVGSVGElement {
  const n = Math.max(1, Math.min(64, amount));
  const columns = Math.ceil(n / 4);
  let d = '';
  for (let i = 0; i < n; i++) {
    d += square(Math.floor(i / 4) * 5, (i % 4) * 5, 3.8);
  }
  // 16 columns, as for 64, are 79 px wide
  const w = (columns - 1) * 5 + 4;
  return s(
    'svg',
    { width: w, height: 19, viewBox: `0 0 ${w} 19`, 'aria-hidden': 'true', focusable: 'false' },
    s('path', { fill: 'currentColor', d }),
  );
}
