// The received postmark on Holdings (after the site's Postmark.astro, as
// the prototype draws it, L884-902): a ring of bits, the wordmark, the
// amount large and the day and time small, with cancellation waves. A pure
// ornament, hidden from assistive tech: every fact on it is also in the
// chip, the sentence, Recent and the live region.

import { h, reduced, s } from './dom.ts';
import { f2, hm } from './format.ts';
import { WORD } from './marks.ts';

let drawings = 0;

const DAYS = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'];

/** The postmark: waves to the right on the desktop, to the left on the phone (the ring sits at the screen's edge). */
export function postmark(big: string, small: string, waves: 'r' | 'l'): SVGSVGElement {
  const C = 105;
  const R = 100;
  const INNER = 74;
  const id = `pm${++drawings}f`;
  const bits = s('g', { class: 'bits' });
  const NB = 44;
  for (let i = 0; i < NB; i++) {
    if (i % 11 === 5) {
      continue;
    }
    const a = (i / NB) * Math.PI * 2 - Math.PI / 2;
    const x = C + 87 * Math.cos(a);
    const y = C + 87 * Math.sin(a);
    bits.append(
      s('rect', {
        x: f2(x - 3.3),
        y: f2(y - 3.3),
        width: 6.6,
        height: 6.6,
        transform: `rotate(${f2((a * 180) / Math.PI + 90)} ${f2(x)} ${f2(y)})`,
      }),
    );
  }
  const cell = 3.5;
  const side = cell - 0.8;
  const ww = (WORD.w - 1) * cell + side;
  let word = '';
  for (const [x, y] of WORD.px) {
    word += `M${f2(C - ww / 2 + x * cell)} ${f2(C - 56 + y * cell)}h${f2(side)}v${f2(side)}h-${f2(side)}z`;
  }
  const barH = Math.sqrt(INNER * INNER - 15 * 15) - 6;
  const bars = `M${f2(C - barH)} ${C - 15}H${f2(C + barH)}`;
  const rows = [-44, -22, 0, 22, 44];
  const wave =
    waves === 'r'
      ? rows
          .map((dy) => {
            const start = C + Math.sqrt(R * R - dy * dy) + 9;
            let d = `M${f2(start)} ${C + dy}`;
            for (let x = start; x + 18 <= 346; x += 18) {
              d += 'q4.5 -5.5 9 0t9 0';
            }
            return d;
          })
          .join('')
      : rows
          .map((dy) => {
            const end = C - Math.sqrt(R * R - dy * dy) - 9;
            let d = `M-136 ${C + dy}`;
            for (let x = -136; x + 18 <= end; x += 18) {
              d += 'q4.5 -5.5 9 0t9 0';
            }
            return d;
          })
          .join('');
  return s(
    'svg',
    { class: 'postmark', viewBox: waves === 'r' ? '0 0 350 210' : '-140 0 350 210', 'aria-hidden': 'true', focusable: 'false' },
    s(
      'defs',
      {},
      s(
        'filter',
        { id, x: '-4%', y: '-4%', width: '108%', height: '108%' },
        s('feTurbulence', { type: 'fractalNoise', baseFrequency: '.9', numOctaves: '2', seed: '7', result: 'speck' }),
        s('feColorMatrix', { in: 'speck', type: 'matrix', values: '0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 -8 6.1', result: 'holes' }),
        s('feComposite', { in: 'SourceGraphic', in2: 'holes', operator: 'in', result: 'inked' }),
        s('feTurbulence', { type: 'fractalNoise', baseFrequency: '.035', numOctaves: '2', seed: '3', result: 'warp' }),
        s('feDisplacementMap', { in: 'inked', in2: 'warp', scale: '1.6', xChannelSelector: 'R', yChannelSelector: 'G' }),
      ),
    ),
    s(
      'g',
      { filter: `url(#${id})` },
      s('path', { class: 'waves', d: wave }),
      s('circle', { cx: C, cy: C, r: R, class: 'ring' }),
      s('circle', { cx: C, cy: C, r: INNER, class: 'ring thin' }),
      bits,
      s('path', { class: 'word', d: word }),
      s('path', { class: 'bars', d: bars }),
      s('text', { class: 'big', x: C, y: C + 25, 'text-anchor': 'middle' }, big),
      s(
        'text',
        {
          class: 'small',
          x: C,
          y: C + 44,
          'text-anchor': 'middle',
          textLength: f2(2 * Math.sqrt(INNER * INNER - 50 * 50) - 12),
          lengthAdjust: 'spacingAndGlyphs',
        },
        small,
      ),
    ),
  );
}

/** Whether Holdings is drawn at the desktop's width, where the postmark sits right of the total. */
export const wide = (): boolean => window.innerWidth >= 1100;

/** The postmark for a take of `amount` at `t`: pink for six seconds, pressed for its first half second. */
export function postmarkWrap(amount: number, t: number, now: number): HTMLElement {
  const age = Math.max(0, now - t) / 1000;
  const right = wide();
  const wrap = h(
    'div',
    {
      class: `pm-wrap${right ? ' r' : ''}${age < 6 ? ' live' : ''}${age < 0.5 && !reduced() ? ' press' : ''}`,
      'aria-hidden': 'true',
      style: { '--pd': `${f2(-age)}s` },
    },
    postmark(`+${amount}`, `${DAYS[new Date(t).getDay()] ?? ''} ${hm(t)}`, right ? 'r' : 'l'),
  );
  if (wrap.classList.contains('press')) {
    setTimeout(() => wrap.classList.remove('press'), 700);
  }
  return wrap;
}

/**
 * On the desktop, the postmark sits right of the total, its ring's left rim
 * just past the chip and the whole ring before the sentence; only the
 * waves are cut where that gap is narrow, and go when less than a third of
 * them would show. On the phone it keeps its place in the CSS.
 */
export function placePostmark(wrap: HTMLElement): void {
  if (!wide()) {
    for (const property of ['--pm-x', '--pm-y', '--pm-w', 'clip-path']) {
      wrap.style.removeProperty(property);
    }
    wrap.classList.remove('bare');
    return;
  }
  const heroL = wrap.parentElement;
  const total = heroL?.querySelector('.total');
  const big = total?.querySelector('b');
  const heroR = heroL?.parentElement?.querySelector('.hero-r');
  if (heroL === null || heroL === undefined || total === null || total === undefined || big === null || big === undefined || heroR === null || heroR === undefined) {
    return;
  }
  const hl = heroL.getBoundingClientRect();
  const last = (total.querySelector('.chip') ?? total.querySelector('i') ?? big).getBoundingClientRect();
  const numerals = big.getBoundingClientRect();
  const hr = heroR.getBoundingClientRect();
  // the ring is D px across; the svg is 1.75 D wide (its viewBox 350 x 210, the ring 200 of it, 5 in)
  const chipR = last.right - hl.left + 10;
  const edge = hr.left - hl.left - 16;
  const D = Math.max(120, Math.min(190, numerals.height + 40, (edge - chipR) / 0.76));
  const w = D * 1.75;
  const hh = w * 0.6;
  const x = chipR - 0.24 * D - 0.025 * w;
  // centred on the numerals (the -9° turn lowers the ring by about 5% of D)
  const cy = (numerals.top + numerals.bottom) / 2 - hl.top - 0.05 * D;
  wrap.style.setProperty('--pm-x', `${f2(x)}px`);
  wrap.style.setProperty('--pm-w', `${f2(w)}px`);
  wrap.style.setProperty('--pm-y', `${f2(cy - hh / 2)}px`);
  const over = x + w - edge;
  const waves = edge - (x + 0.025 * w + D);
  const bare = waves < (0.377 * w) / 3;
  wrap.classList.toggle('bare', bare);
  if (over > 0 && !bare) {
    wrap.style.setProperty('clip-path', `inset(-20px ${f2(over)}px -20px -20px)`);
  } else {
    wrap.style.removeProperty('clip-path');
  }
}
