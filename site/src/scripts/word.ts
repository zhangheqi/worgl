/*
 * The wordmark as bits: a 5-pixel lowercase on a 9-row grid with a one-pixel
 * gap between letters. BitLogo.astro draws the same glyphs as an SVG.
 */
const GLYPHS: Record<string, string[]> = {
  w: ['.....', '.....', 'X...X', 'X...X', 'X.X.X', 'X.X.X', '.X.X.', '.....', '.....'],
  o: ['.....', '.....', '.XXX.', 'X...X', 'X...X', 'X...X', '.XXX.', '.....', '.....'],
  r: ['.....', '.....', 'X.XX.', 'XX..X', 'X....', 'X....', 'X....', '.....', '.....'],
  g: ['.....', '.....', '.XXXX', 'X...X', 'X...X', 'X...X', '.XXXX', '....X', 'XXXX.'],
  l: ['XX.', '.X.', '.X.', '.X.', '.X.', '.X.', '.XX', '...', '...'],
};

export interface Bitmap { w: number; h: number; at(x: number, y: number): number }

export function wordBitmap(word = 'worgl'): Bitmap {
  const px: Array<[number, number]> = [];
  let x = 0;
  for (const ch of word) {
    const g = GLYPHS[ch];
    if (!g) throw new Error(`no glyph for ${ch}`);
    g.forEach((row, r) => [...row].forEach((cell, c) => { if (cell === 'X') px.push([x + c, r]); }));
    x += (g[0]?.length ?? 0) + 1;
  }
  const w = x - 1, h = 9, grid = new Uint8Array(w * h);
  for (const [a, b] of px) grid[b * w + a] = 1;
  return { w, h, at: (a, b) => (a >= 0 && b >= 0 && a < w && b < h ? grid[b * w + a] ?? 0 : 0) };
}
