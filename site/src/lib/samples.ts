/**
 * Code samples taken from files CI compiles, so the docs cannot drift from
 * the code. A sample that disappears fails the build instead of going stale.
 */

/**
 * The first fenced example in a Rust file's `//!` documentation after the
 * line `heading` (or the first one in the file), as rustdoc shows it: lines
 * that rustdoc hides, `# …`, are dropped.
 */
export function rustDocExample(source: string, heading?: string): string {
  const docs = source
    .split('\n')
    .filter((line) => line.startsWith('//!'))
    .map((line) => line.replace(/^\/\/! ?/, ''));
  const from = heading === undefined ? 0 : docs.findIndex((line) => line.trim() === heading);
  if (from < 0) throw new Error(`no "${heading}" in the crate documentation`);
  const open = docs.findIndex((line, i) => i > from && line.startsWith('```'));
  const close = docs.findIndex((line, i) => i > open && line.startsWith('```'));
  if (open < 0 || close < 0) throw new Error(`no example after "${heading ?? 'the start'}"`);
  return docs
    .slice(open + 1, close)
    .filter((line) => !/^#( |$)/.test(line))
    .join('\n');
}
