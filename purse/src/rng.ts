/**
 * A small seeded random number generator (mulberry32), so that what the
 * purse draws at random, such as the print grain and how a fading note comes
 * apart, comes out the same on every draw. Returns numbers in [0, 1).
 */
export function rng(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
