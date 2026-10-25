// The QR codes of a request and a hand-back: which version a link needs,
// whether a phone camera can read it across a counter, and the code's dark
// modules as one path of horizontal runs. The encoder, qrcode-generator,
// is loaded only once a code is to be drawn. No DOM here, so it is tested
// in Node; flow.ts draws the path into an SVG.

type Factory = typeof import('qrcode-generator');

let factory: Factory | undefined;
let loading: Promise<boolean> | undefined;

/** Loads the encoder once; `false` when it did not load (the views then say so, and show the link). */
export function loadQr(): Promise<boolean> {
  if (factory !== undefined) {
    return Promise.resolve(true);
  }
  loading ??= import('qrcode-generator').then(
    (loaded) => {
      factory = (loaded as unknown as { default?: Factory }).default ?? (loaded as unknown as Factory);
      return true;
    },
    () => {
      loading = undefined;
      return false;
    },
  );
  return loading;
}

/** Whether the encoder is loaded. */
export const qrReady = (): boolean => factory !== undefined;

/** A code made: its size in modules, its version, and which modules are dark. */
export interface QrCode {
  n: number;
  version: number;
  isDark: (row: number, col: number) => boolean;
}

/** The code for `text`, at error correction L in byte mode; `undefined` before the encoder loads or past v40. */
export function qrCode(text: string): QrCode | undefined {
  if (factory === undefined) {
    return undefined;
  }
  try {
    const code = factory(0, 'L');
    code.addData(text, 'Byte');
    code.make();
    const n = code.getModuleCount();
    return { n, version: (n - 17) / 4, isDark: (row, col) => code.isDark(row, col) };
  } catch {
    // more than v40 holds
    return undefined;
  }
}

/**
 * The last version drawn as it is, and the last drawn at all (with a word
 * that it is dense): v30 is 137 modules, two pixels each in 290 px, which
 * still fits a 360 px phone's stamp.
 */
export const PLAIN_UP_TO = 20;
export const DENSE_UP_TO = 30;

/** How a link shows: a code a phone reads easily, a dense one, or none (too much for one code). */
export type QrPlan = { version: number; kind: 'plain' | 'dense' } | { kind: 'none' };

/** How `text` shows as a code, once the encoder is loaded. */
export function qrPlan(text: string): QrPlan {
  const code = qrCode(text);
  if (code === undefined || code.version > DENSE_UP_TO) {
    return { kind: 'none' };
  }
  return { version: code.version, kind: code.version > PLAIN_UP_TO ? 'dense' : 'plain' };
}

/** The quiet zone round a code, in modules. */
export const QUIET = 4;

/** Whole CSS pixels a module, for a code of `n` modules in about `target` px. */
export const moduleSize = (n: number, target: number): number => Math.max(1, Math.floor(target / (n + 2 * QUIET)));

/** The size a Request's or a Hand-back's code is drawn at first, in CSS px (L1683). */
export const QR_BASE = 267;

/** The fewest CSS pixels a module of those codes gets: a phone across a counter reads two, not one. */
export const MIN_MODULE = 2;

/**
 * The size to draw a code of `n` modules at first: the base, or more for a
 * code too dense for two pixels a module in it (past v27, up to 290 px for
 * v30, the densest drawn).
 */
export const qrBase = (n: number | undefined): number => (n === undefined ? QR_BASE : Math.max(QR_BASE, MIN_MODULE * (n + 2 * QUIET)));

/**
 * The dark modules of an `n`-module code as path data, `u` px a module,
 * inside the quiet zone: one rectangle a run of dark modules on a row.
 */
export function qrPath(isDark: (row: number, col: number) => boolean, n: number, u: number): string {
  let d = '';
  for (let y = 0; y < n; y++) {
    let x = 0;
    while (x < n) {
      if (!isDark(y, x)) {
        x++;
        continue;
      }
      let e = x;
      while (e < n && isDark(y, e)) {
        e++;
      }
      const w = (e - x) * u;
      d += `M${(x + QUIET) * u} ${(y + QUIET) * u}h${w}v${u}h${-w}z`;
      x = e;
    }
  }
  return d;
}
