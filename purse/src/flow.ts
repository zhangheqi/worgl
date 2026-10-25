// What Receive's and Send's views share: words in italics and bold as
// elements, the QR code drawn as SVG, the viewfinder's corners, the torn
// seam of a send, and the measures the prototype takes once a view is in
// the page (fitPre, fitQR, sizeAmount), set through the CSSOM. And Copy,
// with its fallback of selecting the text, and Share.

import { $, type Child, announce, h, s } from './dom.ts';
import { type Rich, COPY, QR, copiedWords } from './flow-words.ts';
import { f2 } from './format.ts';
import { checkIcon } from './icons.ts';
import { MIN_MODULE, QR_BASE, qrBase, qrCode, qrPath } from './qr.ts';

/** Words with parts in italics or bold, as elements. */
export const rich = (parts: readonly Rich[]): Child[] =>
  parts.map((part) => (typeof part === 'string' ? part : 'em' in part ? h('em', {}, part.em) : h('b', {}, part.b)));

/**
 * A link as a QR code, `target` px across or a little less: whole pixels a
 * module, never scaled by CSS, with four modules of quiet zone. Before the
 * encoder loads, or past what one code holds, the words that say so.
 */
export function qrSvg(text: string, target: number, cls: 'qr' | 'mini', label: string): SVGSVGElement | HTMLElement {
  const code = qrCode(text);
  if (code === undefined) {
    return h('div', { class: `${cls} qr-none` }, QR.failed);
  }
  const { n } = code;
  const u = Math.max(1, Math.floor(target / (n + 8)));
  const px = (n + 8) * u;
  return s(
    'svg',
    { class: cls, width: px, height: px, viewBox: `0 0 ${px} ${px}`, role: 'img', 'aria-label': label, 'data-modules': n },
    s('rect', { class: 'qr-bg', width: px, height: px }),
    s('path', { class: 'qr-fg', d: qrPath(code.isDark, n, u), 'shape-rendering': 'crispEdges' }),
  );
}

/**
 * The holder of a code that fitQR may grow or shrink: it keeps the text and
 * the base size to draw it again. The base is 267 px, or as much as a dense
 * code needs for two pixels a module.
 */
export function qrWrap(text: string, label: string): HTMLElement {
  const base = qrBase(qrCode(text)?.n);
  return h('div', { class: 'qr-wrap', 'data-qt': base, 'data-label': label, 'data-qr': text }, qrSvg(text, base, 'qr', label));
}

/** The viewfinder's four corners. */
export function corners(): SVGSVGElement {
  return s(
    'svg',
    { class: 'corners', viewBox: '0 0 100 100', preserveAspectRatio: 'none', 'aria-hidden': 'true', focusable: 'false' },
    s('path', {
      d: 'M0 14V0h14M86 0h14v14M100 86v14H86M14 100H0V86',
      fill: 'none',
      stroke: 'currentColor',
      'stroke-width': 1.5,
      'vector-effect': 'non-scaling-stroke',
    }),
  );
}

/**
 * The perforated seam between the bands, torn `prog` of the way across, a
 * strip lifting behind the tear, and the pink tear point.
 */
export function tearSvg(W: number, prog: number): SVGSVGElement {
  const top = 14;
  const line = 30;
  const r = 2.7;
  const step = 9;
  const B = 44;
  const tx = f2(W * prog);
  const xs: number[] = [];
  for (let x = 4.5; x < W + step; x += step) {
    xs.push(x);
  }
  const svg = s('svg', { viewBox: `0 0 ${W} ${B}`, width: W, height: B, 'aria-hidden': 'true', focusable: 'false' });
  svg.append(
    s('rect', { class: 't-c', x: tx, y: top, width: f2(W - tx), height: line - top }),
    s('rect', { class: 't-c', x: tx, y: line, width: f2(W - tx), height: B - line }),
    ...xs.filter((x) => x > tx).map((x) => s('circle', { class: 't-hole', cx: x, cy: line, r })),
  );
  if (tx > 0) {
    const low = line + 1.2;
    let d = `M0 ${B}L0 ${low}`;
    for (const x of xs) {
      if (x + r > tx) {
        break;
      }
      d += `L${f2(x - r)} ${low}A${r} ${r} 0 0 0 ${f2(x + r)} ${low}`;
    }
    d += `L${tx} ${low}L${tx} ${B}Z`;
    svg.append(s('path', { class: 't-c', d }));
    let u = `M${tx} ${top}L-60 ${top}L-60 ${line}`;
    for (const x of xs) {
      if (x + r > tx) {
        break;
      }
      u += `L${f2(x - r)} ${line}A${r} ${r} 0 0 1 ${f2(x + r)} ${line}`;
    }
    u += `L${tx} ${line}Z`;
    // the strip lifts at most about 14 px at its far end, whatever the width
    const angle = f2(Math.min(3.2, 0.8 + prog * 3, (Math.atan(14 / Math.max(1, tx)) * 180) / Math.PI));
    svg.append(s('g', { transform: `rotate(${angle} ${tx} ${line})` }, s('path', { class: 't-c', d: u })));
  }
  if (prog > 0) {
    svg.append(
      s('line', { class: 't-pl', x1: tx, y1: top - 5, x2: tx, y2: line + 7 }),
      s('rect', { class: 't-pt', x: f2(tx - 3.5), y: line - 3.5, width: 7, height: 7 }),
    );
  }
  return svg;
}

/**
 * Confirm's large time shows only when the screen holds it without
 * scrolling: as large as the free cobalt holds, up to 150 px (13% of the
 * height) on phones and 280 px (21%) from 560 px wide; gone under 52 px,
 * when the line under the buttons says it instead.
 */
export function fitPre(): void {
  const pre = $('.pre');
  const low = pre?.parentElement?.querySelector<HTMLElement>('.c-low');
  const count = pre?.querySelector<HTMLElement>('.count');
  const big = count?.querySelector<HTMLElement>(':scope > b');
  if (pre === null || pre === undefined || low === null || low === undefined || count === null || count === undefined || big === null || big === undefined) {
    return;
  }
  pre.classList.add('off');
  const de = document.documentElement;
  const ih = window.innerHeight;
  const wide = window.innerWidth >= 560;
  if (de.scrollHeight > ih + 0.5) {
    return;
  }
  pre.classList.remove('off');
  let f = Math.min(wide ? 280 : 150, ih * (wide ? 0.21 : 0.13));
  for (let i = 0; i < 24 && f >= 52; i++) {
    pre.style.setProperty('--pre-f', `${f2(f)}px`);
    const over = de.scrollHeight - ih;
    const ratio = big.getBoundingClientRect().width / count.clientWidth;
    if (over <= 0.5 && ratio <= 1) {
      return;
    }
    f = Math.min(f - 1, ratio > 1 ? f / ratio : f, over > 0.5 ? f - over / 0.8 : f);
  }
  pre.classList.add('off');
}

/**
 * The code on Request and Hand-back grows into the free cobalt, in whole
 * pixels a module: up to 42% of the screen's height and the column's
 * width, keeping 24 px of cobalt above and below the stamp. Where the view
 * scrolls, it shrinks instead, a pixel a module at a time down to two,
 * when that keeps the view's last buttons on the screen.
 */
export function fitQR(): void {
  const wrap = $('.band-c .qr-wrap[data-qr]');
  const svg = wrap?.querySelector('svg.qr');
  const mid = wrap?.closest<HTMLElement>('.c-mid');
  const column = mid?.parentElement;
  const low = column?.querySelector<HTMLElement>('.c-low');
  if (wrap === null || wrap === undefined || svg === null || svg === undefined || mid === null || mid === undefined || column === null || column === undefined || low === null || low === undefined) {
    return;
  }
  const text = wrap.dataset['qr'] ?? '';
  const label = wrap.dataset['label'] ?? '';
  const base = Number(wrap.dataset['qt'] ?? QR_BASE);
  const side = Number(svg.getAttribute('data-modules') ?? 0) + 8;
  const draw = (target: number): number => {
    const code = qrSvg(text, target, 'qr', label);
    wrap.replaceChildren(code);
    return Number(code.getAttribute('width') ?? 0);
  };
  let px = Number(svg.getAttribute('width') ?? 0);
  if (wrap.dataset['fit'] !== undefined) {
    px = draw(base);
    delete wrap.dataset['fit'];
  }
  const de = document.documentElement;
  const ih = window.innerHeight;
  if (de.scrollHeight > ih + 0.5) {
    // smaller, a module's pixel at a time, only if that brings the whole view on screen
    for (let u = Math.floor(px / side) - 1; u >= MIN_MODULE && side > 8; u--) {
      draw(u * side);
      if (de.scrollHeight <= ih + 0.5) {
        wrap.dataset['fit'] = '1';
        return;
      }
    }
    draw(base);
    return;
  }
  const style = getComputedStyle(column);
  const free = low.getBoundingClientRect().top - column.getBoundingClientRect().top - mid.getBoundingClientRect().height - 48;
  const room = Math.min(
    px + free,
    ih * 0.42,
    column.clientWidth - Number.parseFloat(style.paddingLeft) - Number.parseFloat(style.paddingRight) - 38,
  );
  if (room < px + 1) {
    return;
  }
  if (draw(room) <= px || de.scrollHeight > ih + 0.5) {
    draw(base);
  } else {
    wrap.dataset['fit'] = '1';
  }
}

let measure: CanvasRenderingContext2D | null | undefined;

/** The amount's field as wide as its text, so "tokens" sits right after the number. */
export function sizeAmount(input: HTMLInputElement): void {
  try {
    measure ??= document.createElement('canvas').getContext('2d');
    if (measure === null) {
      throw new Error('no canvas');
    }
    const style = getComputedStyle(input);
    const size = Number.parseFloat(style.fontSize);
    const spacing = Number.parseFloat(style.letterSpacing) || 0;
    measure.font = `${style.fontStyle} ${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
    const text = input.value === '' ? '0' : input.value;
    input.style.width = `${Math.ceil(measure.measureText(text).width + spacing * text.length + size * 0.06)}px`;
  } catch {
    input.style.setProperty('--len', String(Math.max(1, input.value.length)));
  }
}

/** Buttons showing "Copied" now, so a second tap does not stack the change. */
const showing = new WeakSet<HTMLElement>();

/**
 * Copies `text` and says so on the button for 2.2 s: in its `span` when it
 * has one, otherwise as a tick and a word in place of its icon. Where the
 * clipboard refuses, the text in `select` is selected for the person to
 * copy, and the button says that.
 */
export function doCopy(button: HTMLElement, text: string, select: HTMLElement | null): void {
  const label = button.querySelector<HTMLElement>(':scope > span');
  const say = (copied: boolean): void => {
    const words = copiedWords(copied);
    announce(words);
    if (label !== null) {
      const was = label.textContent;
      label.textContent = copied ? COPY.copied : COPY.selected;
      setTimeout(() => {
        if (label.isConnected) {
          label.textContent = was;
        }
      }, 2200);
      return;
    }
    if (showing.has(button)) {
      return;
    }
    showing.add(button);
    const was = [...button.childNodes];
    const wasLabel = button.getAttribute('aria-label');
    button.replaceChildren(checkIcon(), h('span', { class: 'lab', 'aria-hidden': 'true' }, copied ? COPY.copied : COPY.selected));
    button.classList.add('did');
    button.setAttribute('aria-label', words);
    setTimeout(() => {
      showing.delete(button);
      if (!button.isConnected) {
        return;
      }
      button.replaceChildren(...was);
      button.classList.remove('did');
      if (wasLabel !== null) {
        button.setAttribute('aria-label', wasLabel);
      }
    }, 2200);
  };
  const fallback = (): void => {
    if (select !== null) {
      try {
        const range = document.createRange();
        range.selectNodeContents(select);
        const selection = getSelection();
        selection?.removeAllRanges();
        selection?.addRange(range);
      } catch {
        // nothing to select
      }
    }
    say(false);
  };
  try {
    if (navigator.clipboard?.writeText === undefined) {
      fallback();
      return;
    }
    navigator.clipboard.writeText(text).then(() => say(true), fallback);
  } catch {
    fallback();
  }
}

/** Whether this browser offers its share sheet. */
export const canShare = (): boolean => typeof navigator.share === 'function';

/** Opens the share sheet for a link; closing it unshared is no error. */
export function shareLink(title: string, url: string): void {
  try {
    void navigator.share({ title, url }).catch(() => undefined);
  } catch {
    // no share sheet after all: Copy is beside it
  }
}
