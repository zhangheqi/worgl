/*
 * Four layers of a tiny mint as stamp sheets, to mint into, transfer,
 * refresh and rotate by hand. A rotation tears off the top sheet.
 *
 * By default clicking a note transfers it. With data-select on the root,
 * clicking selects a note instead and the buttons act on it. Hovering or
 * focusing a note reads it in a tip. Other parts of the page can press the
 * buttons by dispatching a "lab-do" event with the action's name as detail.
 */
import { isReduced } from './motion';

const N = 4, CAP = 48;
interface Cell { a: number; live: boolean }
interface Sheet { gen: number; cells: Cell[] }

export function mountStampLab(root: HTMLElement) {
  const part = <T extends Element>(name: string) => root.querySelector<T>(`[data-part="${name}"]`)!;
  const host = part<HTMLElement>('sheets'), out = part<HTMLElement>('readout');
  const cRot = part<HTMLElement>('rotations'), cExp = part<HTMLElement>('expired');
  const tip = part<HTMLElement>('tip');
  const selectMode = root.hasAttribute('data-select');
  let L: Sheet[] = [], gen = 1, rot = 0, expired = 0, busy = false;
  let sel: { gen: number; i: number } | null = null, tipFor: { gen: number; i: number } | null = null, rendering = false;
  const fx = new Map<string, string>();
  const sheet = (k: number) => L[k]!;
  const nl = (): Sheet => ({ gen: gen++, cells: [] });
  const hopsTxt = (k: number) => { const h = N - 1 - k; return h === 0 ? 'frozen until the next rotation' : `${h} ${h === 1 ? 'hop' : 'hops'} left`; };
  const diesTxt = (k: number) => (k === 0 ? 'dies at the next rotation' : `dies in ${k + 1} rotations`);
  const say = (t: string) => { out.textContent = t; };
  const seed = (layer: Sheet, list: Array<[number, number]>) => { for (const [a, live] of list) layer.cells.push({ a, live: !!live }); };
  const selected = () => {
    const k = sel ? L.findIndex((x) => x.gen === sel!.gen) : -1;
    return k < 0 || !sel ? null : { k, i: sel.i, c: sheet(k).cells[sel.i]! };
  };
  // The tip reads a live note the way a holder's SDK would, and follows it
  // across re-renders until the note is spent or the pointer leaves.
  function showTip() {
    const k = tipFor ? L.findIndex((x) => x.gen === tipFor!.gen) : -1;
    const el = k < 0 ? null : host.querySelector<HTMLElement>(`.c.l[data-k="${k}"][data-i="${tipFor!.i}"]`);
    if (!el) { tip.hidden = true; return; }
    const c = sheet(k).cells[tipFor!.i]!, h = N - 1 - k;
    tip.innerHTML = `<b>Note ${sheet(k).gen}:${tipFor!.i} · worth ${c.a}</b>` +
      `<span>Layer ${k} of ${N} · ${h === 0 ? 'frozen until the next rotation' : `${h} ${h === 1 ? 'hop' : 'hops'} left now`}</span>` +
      `<span>${k === 0 ? 'Dies at the next rotation' : `Dies in ${k + 1} rotations`} if left alone</span>`;
    tip.hidden = false;
    const r = el.getBoundingClientRect(), tw = tip.offsetWidth, th = tip.offsetHeight;
    let x = r.left + r.width / 2 - tw / 2, y = r.top - th - 10;
    x = Math.max(8, Math.min(x, window.innerWidth - tw - 8));
    if (y < 72) y = r.bottom + 10;
    tip.style.transform = `translate(${Math.round(x)}px,${Math.round(y)}px)`;
  }
  function tipOn(el: HTMLElement | null) {
    tipFor = el ? { gen: sheet(Number(el.dataset.k)).gen, i: Number(el.dataset.i) } : null;
    showTip();
  }

  function reset() {
    gen = 1; rot = 0; expired = 0;
    L = [nl(), nl(), nl(), nl()];
    seed(sheet(0), [[8, 0], [4, 1], [2, 0], [8, 1], [1, 0], [4, 0]]);
    seed(sheet(1), [[8, 0], [2, 1], [4, 0], [8, 1], [1, 1], [2, 0], [4, 0]]);
    seed(sheet(2), [[8, 1], [8, 0], [4, 1], [2, 1], [8, 1]]);
    seed(sheet(3), [[4, 1], [8, 1]]);
    sel = selectMode ? { gen: sheet(2).gen, i: 2 } : null;
    fx.clear();
    render();
    say(selectMode ? 'Four layers, oldest at the top. Click a note to follow it, then use the buttons.' : 'Four layers, oldest at the top. Click a note to transfer it, or use the buttons.');
  }
  function render(arrive = false) {
    rendering = true;
    host.innerHTML = L.map((layer, k) => {
      const cls = `sheet${k === N - 1 ? ' frozen' : ''}${k === 0 ? ' dying' : ''}${arrive && k === N - 1 ? ' arrive' : ''}`;
      let cells = '';
      for (let i = 0; i < CAP; i++) {
        const c = layer.cells[i], f = fx.get(`${layer.gen}:${i}`) ?? '';
        if (!c) cells += '<span class="c u"></span>';
        else if (c.live) {
          const on = sel && sel.gen === layer.gen && sel.i === i ? ' sel' : '';
          cells += `<button type="button" class="c l${on} ${f}" data-k="${k}" data-i="${i}" aria-label="Note worth ${c.a} in layer ${k}"${selectMode ? ` aria-pressed="${!!on}"` : ''}>${c.a}</button>`;
        }
        else cells += `<span class="c s ${f}"></span>`;
      }
      return `<div class="${cls}"><div class="sheet-head"><span class="label">Layer ${k} · gen ${layer.gen}</span><span class="label muted">${hopsTxt(k)} · ${diesTxt(k)}</span></div><div class="cells">${cells}</div></div>`;
    }).join('');
    rendering = false;
    fx.clear();
    cRot.textContent = String(rot);
    cExp.textContent = String(expired);
    showTip();
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
    // The selection follows the value to its new note.
    if (sel && sel.gen === sheet(k).gen && sel.i === i) sel = { gen: next.gen, i: next.cells.length - 1 };
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
      if (selectMode) sel = { gen: layer.gen, i: layer.cells.length - 1 };
      render();
      say(`Minted 8 into layer ${k}, the second-to-last: one hop right away, and it dies in ${k + 1} rotations if left alone.`);
    },
    transfer() {
      const s = selected();
      if (selectMode && s && s.c.live) { move(s.k, s.i); render(); return; }
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
        const lost = sel?.gen === d.gen && d.cells[sel.i]?.live;
        if (sel?.gen === d.gen) sel = null;
        expired += v; rot++;
        L.push(nl());
        render(true);
        busy = false;
        say(`Rotation ${rot}: generation ${d.gen} dropped. ` +
          (dead.length ? `${dead.length} ${dead.length === 1 ? 'idle note' : 'idle notes'} worth ${v} expired with it.` : 'Nothing was left in it, so nothing expired.') +
          ' The frozen layer can move again.' + (lost ? ' The note you were following expired with it.' : ''));
      };
      const first = host.firstElementChild;
      if (isReduced() || !first) return done();
      first.classList.add('tear');
      setTimeout(done, 680);
    },
    reset,
  };
  root.querySelectorAll<HTMLElement>('[data-do]').forEach((b) => b.addEventListener('click', () => act[b.dataset.do ?? '']?.()));
  root.addEventListener('lab-do', (e) => act[(e as CustomEvent<string>).detail]?.());
  const cellOf = (t: EventTarget | null) => (t instanceof Element ? t.closest<HTMLElement>('.c.l') : null);
  host.addEventListener('pointerover', (e) => { if (e.pointerType === 'mouse') tipOn(cellOf(e.target)); });
  host.addEventListener('pointerout', (e) => { if (e.pointerType === 'mouse' && !cellOf(e.relatedTarget)) tipOn(null); });
  host.addEventListener('focusin', (e) => tipOn(cellOf(e.target)));
  // A re-render removes the focused note, which is not the reader leaving it.
  host.addEventListener('focusout', (e) => { if (!rendering && !cellOf(e.relatedTarget)) tipOn(null); });
  // On touch, a tap shows the tip for the note tapped; scrolling or tapping elsewhere hides it.
  host.addEventListener('pointerdown', (e) => { if (e.pointerType !== 'mouse') tipOn(cellOf(e.target)); });
  document.addEventListener('pointerdown', (e) => { if (!host.contains(e.target as Node)) tipOn(null); });
  window.addEventListener('scroll', () => { if (tipFor) tipOn(null); }, { passive: true });
  host.addEventListener('click', (e) => {
    const c = (e.target as Element).closest<HTMLElement>('.c.l');
    if (!c) return;
    const k = Number(c.dataset.k), i = Number(c.dataset.i);
    if (selectMode) {
      sel = { gen: sheet(k).gen, i };
      render();
      say(k === N - 1 ? 'Selected a frozen note. Rotate to thaw it, then transfer it.' : 'Selected. Transfer moves it one layer on; Rotate shows what happens if it sits still.');
      return;
    }
    if (move(k, i)) render();
  });
  reset();
}
