/*
 * What the mint can draw: a lineage tree of notes, each under its own one-time
 * key. One lineage is traced in pink every few seconds. Nothing in the tree
 * says which boxes share a holder.
 */
import { isReduced, onMotionChange, randInt } from './motion';

const NS = 'http://www.w3.org/2000/svg';
interface Node { c: number; x: number; y: number; key: string; parent?: Node; edge?: SVGPathElement; g?: SVGGElement }

export function mountLineage(svg: SVGSVGElement) {
  const host = svg.parentElement;
  if (!host) return;
  let nodes: Node[] = [], lastW = 0, timer = 0, inView = false;
  let steps: number[] = [];
  const hex4 = () => randInt(65536).toString(16).padStart(4, '0');
  const el = <K extends keyof SVGElementTagNameMap>(n: K, a: Record<string, string | number>) => {
    const e = document.createElementNS(NS, n);
    for (const k in a) e.setAttribute(k, String(a[k]));
    return e;
  };

  function build() {
    const w = Math.round(host!.clientWidth - 28);
    if (w <= 0 || w === lastW) return;
    lastW = w;
    const narrow = w < 520, ncol = narrow ? 4 : 6, counts = [2, 3, 4, 5, 6, 7].slice(0, ncol);
    const nw = narrow ? 52 : 64, nh = 22, top = 26, gapY = narrow ? 32 : 36;
    const h = top + Math.max(...counts) * gapY;
    svg.setAttribute('viewBox', `0 0 ${w} ${h}`);
    svg.setAttribute('width', String(w));
    svg.setAttribute('height', String(h));
    svg.textContent = '';
    nodes = [];
    const step = (w - nw) / (ncol - 1);
    counts.forEach((n, c) => {
      const t = el('text', { x: c * step, y: 10, class: 'lab' });
      t.textContent = c === 0 ? 'MINTED' : `HOP ${c}`;
      svg.appendChild(t);
      for (let j = 0; j < n; j++) nodes.push({ c, x: c * step, y: top + (j + .5) * (h - top) / n, key: hex4() });
    });
    const col = (c: number) => nodes.filter((d) => d.c === c);
    for (let c = 1; c < ncol; c++) {
      const prev = col(c - 1), cur = col(c);
      cur.forEach((d, j) => { d.parent = prev[Math.min(prev.length - 1, Math.floor(j * prev.length / cur.length))]; });
    }
    const eg = el('g', {}), ng = el('g', {});
    svg.append(eg, ng);
    for (const d of nodes) {
      if (d.parent) {
        const x1 = d.parent.x + nw, y1 = d.parent.y, x2 = d.x, y2 = d.y, mx = (x1 + x2) / 2;
        d.edge = el('path', { d: `M${x1} ${y1}C${mx} ${y1} ${mx} ${y2} ${x2} ${y2}`, class: 'edge-l' });
        eg.appendChild(d.edge);
      }
      d.g = el('g', { class: 'nd' });
      d.g.appendChild(el('rect', { x: d.x, y: d.y - nh / 2, width: nw, height: nh }));
      const t = el('text', { x: d.x + nw / 2, y: d.y + 3.5, 'text-anchor': 'middle' });
      t.textContent = `k·${d.key}`;
      d.g.appendChild(t);
      ng.appendChild(d.g);
    }
    light(true);
  }
  function light(instant: boolean) {
    steps.forEach(clearTimeout);
    steps = [];
    for (const d of nodes) { d.g?.classList.remove('on'); d.edge?.classList.remove('on'); }
    const maxC = Math.max(...nodes.map((d) => d.c)), leaves = nodes.filter((d) => d.c === maxC);
    const path: Node[] = [];
    let d: Node | undefined = leaves[randInt(leaves.length)];
    while (d) { path.unshift(d); d = d.parent; }
    path.forEach((p, idx) => {
      const on = () => { p.g?.classList.add('on'); p.edge?.classList.add('on'); };
      if (instant || isReduced()) on(); else steps.push(window.setTimeout(on, idx * 180));
    });
  }
  function loop() {
    clearInterval(timer);
    timer = 0;
    if (inView && !isReduced() && !document.hidden) timer = window.setInterval(() => light(false), 2800);
  }
  new ResizeObserver(build).observe(host);
  new IntersectionObserver((es) => { inView = es.some((e) => e.isIntersecting); loop(); }).observe(host);
  document.addEventListener('visibilitychange', loop);
  onMotionChange(loop);
}
