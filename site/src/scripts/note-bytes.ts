/*
 * A note, byte by byte, beside the mint's share of it: one bit, drawn at one
 * eighth the area of a byte. The layout follows Note in crates/worgl/src/note.rs.
 */
import { randInt } from './motion';

const FIELDS = [
  { k: 'ver', name: 'Version', from: 0, to: 1, d: 'The format version, so the layout can change later.' },
  { k: 'iss', name: 'Mint key', from: 1, to: 33, d: 'The issuer key of the mint that made it. A note names its mint, so it finds its own way back.' },
  { k: 'gen', name: 'Generation', from: 33, to: 41, d: 'Which rotation’s layer it was issued in. A generation is never reused.' },
  { k: 'idx', name: 'Index', from: 41, to: 45, d: 'The note’s bit, run through a keyed permutation, so serials don’t reveal order.' },
  { k: 'amt', name: 'Amount', from: 45, to: 53, d: 'What the note is worth, in the token’s smallest unit.' },
  { k: 'hol', name: 'Holder key', from: 53, to: 85, d: 'A one-time public key of whoever holds it. Spending needs its signature.' },
  { k: 'sig', name: 'Signature', from: 85, to: 149, d: 'Ed25519, by the generation’s own key, which the mint key certifies.' },
] as const;

export function mountNoteBytes(root: HTMLElement) {
  const part = <T extends Element>(name: string) => root.querySelector<T>(`[data-part="${name}"]`)!;
  const grid = part<HTMLElement>('bytes'), legend = part<HTMLElement>('legend'), read = part<HTMLElement>('read');

  // A made-up note, 1291:238066 worth 8, with random keys and signature.
  let seed = 1932;
  const rnd = () => { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return (seed >> 8) & 255; };
  const bytes = Array.from({ length: 149 }, rnd);
  bytes[0] = 1;
  [0, 0, 0, 0, 0, 0, 0x05, 0x0B].forEach((b, j) => { bytes[33 + j] = b; });
  [0x00, 0x03, 0xA1, 0xF2].forEach((b, j) => { bytes[41 + j] = b; });
  [0, 0, 0, 0, 0, 0, 0, 8].forEach((b, j) => { bytes[45 + j] = b; });
  const fieldOf = (i: number) => FIELDS.find((f) => i >= f.from && i < f.to)!;
  const range = (f: (typeof FIELDS)[number]) => (f.to - f.from === 1 ? `byte ${f.from}` : `bytes ${f.from}–${f.to - 1}`);

  grid.innerHTML = bytes.map((b, i) => `<span class="b f-${fieldOf(i).k}" data-f="${fieldOf(i).k}">${b.toString(16).padStart(2, '0')}</span>`).join('');
  legend.innerHTML = FIELDS.map((f) => `<li><button type="button" data-f="${f.k}" aria-pressed="false"><i class="sw f-${f.k}"></i><span>${f.name}</span><em>${f.to - f.from}</em></button></li>`).join('');
  const def = read.innerHTML;
  let pinned: string | null = null;
  function hl(k: string | null) {
    grid.dataset.hl = k ?? '';
    for (const b of grid.children) b.classList.toggle('on', !!k && (b as HTMLElement).dataset.f === k);
    for (const b of legend.querySelectorAll<HTMLElement>('button')) b.setAttribute('aria-pressed', String(b.dataset.f === k));
    const f = FIELDS.find((x) => x.k === k);
    read.innerHTML = f ? `<b>${f.name}</b>, ${range(f)}. ${f.d}` : def;
  }
  const fieldFrom = (e: Event, sel: string) => (e.target as Element).closest<HTMLElement>(sel)?.dataset.f ?? null;
  legend.addEventListener('mouseover', (e) => { const f = fieldFrom(e, 'button'); if (f) hl(f); });
  legend.addEventListener('mouseleave', () => hl(pinned));
  legend.addEventListener('focusin', (e) => { const f = fieldFrom(e, 'button'); if (f) hl(f); });
  legend.addEventListener('focusout', () => hl(pinned));
  legend.addEventListener('click', (e) => { const f = fieldFrom(e, 'button'); if (!f) return; pinned = pinned === f ? null : f; hl(pinned); });
  grid.addEventListener('mouseover', (e) => { const f = fieldFrom(e, '.b'); if (f) hl(f); });
  grid.addEventListener('mouseleave', () => hl(pinned));

  // The bit's side is a byte cell's side over √8, so its area is one eighth.
  new ResizeObserver(() => {
    const cell = grid.firstElementChild?.getBoundingClientRect().width;
    if (cell) root.style.setProperty('--cell', `${cell}px`);
  }).observe(grid);
  const bit = part<HTMLElement>('bit'), btn = part<HTMLButtonElement>('bit-btn'), state = part<HTMLElement>('bit-state'), ser = part<HTMLElement>('bit-serial');
  let isLive = true;
  btn.addEventListener('click', () => {
    if (isLive) { isLive = false; state.textContent = '0, spent'; btn.textContent = 'Issue another note'; }
    else { isLive = true; ser.textContent = `1291:${100000 + randInt(900000)}`; state.textContent = '1, live'; btn.textContent = 'Spend the note'; }
    bit.classList.toggle('spent', !isLive);
    bit.classList.remove('flash');
    void bit.offsetWidth;
    bit.classList.add('flash');
  });
}
