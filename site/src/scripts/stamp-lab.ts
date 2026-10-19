/*
 * Four layers of a tiny mint as stamp sheets, to mint into, transfer,
 * refresh and rotate by hand. A rotation tears off the top sheet.
 */
import { isReduced } from './motion';

const N = 4, CAP = 48;
interface Cell { a: number; live: boolean }
interface Sheet { gen: number; cells: Cell[] }

export function mountStampLab(root: HTMLElement) {
  const part = <T extends Element>(name: string) => root.querySelector<T>(`[data-part="${name}"]`)!;
  const host = part<HTMLElement>('sheets'), out = part<HTMLElement>('readout');
  const cRot = part<HTMLElement>('rotations'), cExp = part<HTMLElement>('expired');
  let L: Sheet[] = [], gen = 1, rot = 0, expired = 0, busy = false;
  const fx = new Map<string, string>();
  const sheet = (k: number) => L[k]!;
  const nl = (): Sheet => ({ gen: gen++, cells: [] });
  const hopsTxt = (k: number) => { const h = N - 1 - k; return h === 0 ? 'frozen until the next rotation' : `${h} ${h === 1 ? 'hop' : 'hops'} left`; };
  const diesTxt = (k: number) => (k === 0 ? 'dies at the next rotation' : `dies in ${k + 1} rotations`);
  const say = (t: string) => { out.textContent = t; };
  const seed = (layer: Sheet, list: Array<[number, number]>) => { for (const [a, live] of list) layer.cells.push({ a, live: !!live }); };

  function reset() {
    gen = 1; rot = 0; expired = 0;
    L = [nl(), nl(), nl(), nl()];
    seed(sheet(0), [[8, 0], [4, 1], [2, 0], [8, 1], [1, 0], [4, 0]]);
    seed(sheet(1), [[8, 0], [2, 1], [4, 0], [8, 1], [1, 1], [2, 0], [4, 0]]);
    seed(sheet(2), [[8, 1], [8, 0], [4, 1], [2, 1], [8, 1]]);
    seed(sheet(3), [[4, 1], [8, 1]]);
    fx.clear();
    render();
    say('Four layers, oldest at the top. Click a note to transfer it, or use the buttons.');
  }
  function render(arrive = false) {
    host.innerHTML = L.map((layer, k) => {
      const cls = `sheet${k === N - 1 ? ' frozen' : ''}${k === 0 ? ' dying' : ''}${arrive && k === N - 1 ? ' arrive' : ''}`;
      let cells = '';
      for (let i = 0; i < CAP; i++) {
        const c = layer.cells[i], f = fx.get(`${layer.gen}:${i}`) ?? '';
        if (!c) cells += '<span class="c u"></span>';
        else if (c.live) cells += `<button type="button" class="c l ${f}" data-k="${k}" data-i="${i}" aria-label="Note worth ${c.a} in layer ${k}">${c.a}</button>`;
        else cells += `<span class="c s ${f}"></span>`;
      }
      return `<div class="${cls}"><div class="sheet-head"><span class="label">Layer ${k} · gen ${layer.gen}</span><span class="label muted">${hopsTxt(k)} · ${diesTxt(k)}</span></div><div class="cells">${cells}</div></div>`;
    }).join('');
    fx.clear();
    cRot.textContent = String(rot);
    cExp.textContent = String(expired);
  }
  function move(k: number, i: number, quiet = false) {
    const c = sheet(k).cells[i];
    if (!c) return false;
    if (k === N - 1) {
      fx.set(`${sheet(k).gen}:${i}`, 'shake');
      render();
      say('That note is frozen. The last layer cannot move until the next rotation, which thaws it.');
      return false;
    }
    const next = sheet(k + 1);
    if (next.cells.length >= CAP) { say(`Layer ${k + 1} is full until the next rotation.`); return false; }
    c.live = false;
    fx.set(`${sheet(k).gen}:${i}`, 'fs');
    next.cells.push({ a: c.a, live: true });
    fx.set(`${next.gen}:${next.cells.length - 1}`, 'fl');
    if (!quiet) {
      const n = k + 1;
      say(`Transferred ${c.a} from layer ${k} to layer ${n}, under a fresh key. ` +
        (n === N - 1 ? `It is frozen now, and dies in ${N} rotations if left alone.` : `It has ${hopsTxt(n)} and ${diesTxt(n)} if left alone.`));
    }
    return true;
  }
  const act: Record<string, () => void> = {
    mint() {
      const k = N - 2, layer = sheet(k);
      if (layer.cells.length >= CAP) return say(`Layer ${k} is full until the next rotation.`);
      layer.cells.push({ a: 8, live: true });
      fx.set(`${layer.gen}:${layer.cells.length - 1}`, 'fl');
      render();
      say(`Minted 8 into layer ${k}, the second-to-last: one hop right away, and it dies in ${k + 1} rotations if left alone.`);
    },
    transfer() {
      for (let k = 0; k < N - 1; k++) {
        const i = sheet(k).cells.findIndex((c) => c.live);
        if (i >= 0) { move(k, i); render(); return; }
      }
      say('Nothing can move: every live note is frozen in the last layer. Rotate to thaw them.');
    },
    refresh() {
      const idx = sheet(0).cells.map((c, i) => (c.live ? i : -1)).filter((i) => i >= 0);
      if (!idx.length) return say('There is nothing in the oldest layer to save.');
      let n = 0;
      for (const i of idx) if (move(0, i, true)) n++;
      render();
      say(`Refreshed ${n} ${n === 1 ? 'note' : 'notes'}: each paid to a fresh key of your own, one layer on. ${n === 1 ? 'It now dies' : 'They now die'} in 2 rotations instead of 1.`);
    },
    rotate() {
      if (busy) return;
      busy = true;
      const done = () => {
        const d = L.shift()!, dead = d.cells.filter((c) => c.live), v = dead.reduce((s, c) => s + c.a, 0);
        expired += v; rot++;
        L.push(nl());
        render(true);
        busy = false;
        say(`Rotation ${rot}: generation ${d.gen} dropped. ` +
          (dead.length ? `${dead.length} ${dead.length === 1 ? 'idle note' : 'idle notes'} worth ${v} expired with it.` : 'Nothing was left in it, so nothing expired.') +
          ' The frozen layer can move again.');
      };
      const first = host.firstElementChild;
      if (isReduced() || !first) return done();
      first.classList.add('tear');
      setTimeout(done, 680);
    },
    reset,
  };
  root.querySelectorAll<HTMLElement>('[data-do]').forEach((b) => b.addEventListener('click', () => act[b.dataset.do ?? '']?.()));
  host.addEventListener('click', (e) => {
    const c = (e.target as Element).closest<HTMLElement>('.c.l');
    if (!c) return;
    if (move(Number(c.dataset.k), Number(c.dataset.i))) render();
  });
  reset();
}
