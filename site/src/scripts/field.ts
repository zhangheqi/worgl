/*
 * The hero's live field: a simulated mint running in the page. Four layers of
 * bits, oldest at the top, follow the ledger's rules: new notes land in the
 * second-to-last layer, a transfer issues its outputs one layer on, the last
 * layer is frozen, and every rotation drops the oldest layer with every note
 * still in it. When the visitor goes idle, transfers stop and rotations go on.
 */
import { clamp, fmt, isIdle, isReduced, lastPointer, onIdleChange, onMotionChange, randInt, whileVisible, PAPER, PINK } from './motion';

const N = 4, ROT = 12000, PULSE = 1600, SLIDE = 760, FLASH = 950, THREAD = 1700, RIPPLE = 1900;
// The visitor's own spend plays out longer, and the rest of the field steps back while it does.
const FOCUS = 3200, FLASH_MINE = 1800, THREAD_MINE = 2800, INK = '17,20,74';
const ROLE = ['3 hops left · dies next rotation', '2 hops left · dies in 2 rotations', '1 hop left · new notes land here', 'frozen until next rotation'];
const ROLE_SHORT = ['dies next rotation', 'dies in 2', 'new notes land here', 'frozen'];
const ease = (t: number) => (t < .5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
const bez = (p0: number, p1: number, p2: number, p3: number, t: number) => {
  const u = 1 - t;
  return u * u * u * p0 + 3 * u * u * t * p1 + 3 * u * t * t * p2 + t * t * t * p3;
};

interface Layer { gen: number; used: number; live: number; bits: Uint8Array; amt: Uint8Array }
interface Flash { L: Layer; i: number; t: number; kind: 1 | 2 | 3; mine?: boolean }
interface Thread { L: Layer; i: number; T: Layer; js: number[]; t: number; mine?: boolean }
interface Hit { k: number; L: Layer; i: number }
type Phase = 'run' | 'pulse' | 'slide';

export function mountField(root: HTMLElement) {
  const part = <T extends Element>(name: string) => root.querySelector<T>(`[data-part="${name}"]`)!;
  const wrap = part<HTMLElement>('wrap'), cv = part<HTMLCanvasElement>('canvas'), tip = part<HTMLElement>('tip');
  const align = part<HTMLElement>('hud'), msg = part<HTMLElement>('msg'), live = part<HTMLElement>('live');
  const out = {
    t: part<HTMLElement>('transfers'), l: part<HTMLElement>('live-notes'), e: part<HTMLElement>('expired'),
    r: part<HTMLElement>('next-rotation'), chip: part<HTMLElement>('chip'),
  };
  const ctx = cv.getContext('2d');
  if (!ctx) return;

  let W = 0, H = 0, dpr = 1, pitch = 12, size = 9, rows = 8, cols = 0, cap = 0, x0 = 0;
  let labelH = 30, perfH = 14, cellsH = 0, bandH = 0, lx = 16, compact = false;
  let layers: Layer[] = [], pending: Layer | null = null, gen = 1284, rate = 0;
  let stats = { transfers: 0, expired: 0, minted: 0 };
  let flashes: Flash[] = [], threads: Thread[] = [], ripple: { x: number; y: number; t: number } | null = null;
  let vital = 1, activity = 1, acc = 0, phase: Phase = 'run', phaseT = 0, nextRot = 0, shift = 0, slideE = 0;
  let running = false, raf = 0, last = 0, pausedAt = 0, hover: Hit | null = null, hudT = 0;
  // The note the visitor last spent into. Background traffic never touches it.
  let mine: { L: Layer; i: number } | null = null, focusT = -1e9;
  const B = 18, buckets: number[][] = Array.from({ length: B }, () => []);
  const L = (k: number) => layers[k]!;
  const isMine = (layer: Layer, i: number) => !!mine && mine.L === layer && mine.i === i;
  // Whether a note in layer k can move: the last layer is frozen, and a full layer takes no more.
  const room = (k: number) => k < N - 1 && L(k + 1).used < cap;
  const spendBtn = part<HTMLButtonElement>('spend');
  function focus(now: number) {
    if (isReduced()) return 0;
    const u = now - focusT;
    return u < 0 || u >= FOCUS ? 0 : Math.min(1, u / 180, (FOCUS - u) / 700);
  }

  const newLayer = (): Layer => ({ gen: gen++, used: 0, live: 0, bits: new Uint8Array(cap), amt: new Uint8Array(cap) });
  // Serials look scrambled, the way the ledger's keyed permutation makes them.
  const serial = (layer: Layer, i: number) => `${layer.gen}:${(i * 7919 + layer.gen * 131) % cap}`;

  function issue(layer: Layer, a: number, now: number, vis: boolean, own = false) {
    if (layer.used >= cap) return -1;
    const i = layer.used++;
    layer.bits[i] = 1; layer.amt[i] = a; layer.live++;
    if (vis) flashes.push({ L: layer, i, t: now, kind: 1, mine: own });
    return i;
  }
  function kill(layer: Layer, i: number, now: number, vis: boolean, own = false) {
    if (i < 0 || !layer.bits[i]) return false;
    layer.bits[i] = 0; layer.live--;
    if (vis) flashes.push({ L: layer, i, t: now, kind: 2, mine: own });
    return true;
  }
  function pickLive(layer: Layer) {
    if (!layer.live) return -1;
    for (let t = 0; t < 30; t++) { const i = randInt(layer.used); if (layer.bits[i] && !isMine(layer, i)) return i; }
    const s = randInt(layer.used);
    for (let j = 0; j < layer.used; j++) { const i = (s + j) % layer.used; if (layer.bits[i] && !isMine(layer, i)) return i; }
    return -1;
  }
  function transfer(k: number, i: number, now: number, o: { vis: boolean; split?: boolean; thread?: boolean; own?: boolean }) {
    const from = L(k);
    if (k >= N - 1) return { err: 'frozen' as const };
    if (i < 0 || !from.bits[i]) return { err: 'spent' as const };
    const to = L(k + 1), a = from.amt[i]!;
    // A split halves a power-of-two note into two notes of the next size down.
    const outs = o.split && a > 0 ? [a - 1, a - 1] : [a];
    if (to.used + outs.length > cap) return { err: 'full' as const };
    kill(from, i, now, o.vis, o.own);
    const js = outs.map((x) => issue(to, x, now, o.vis, o.own));
    stats.transfers++;
    if (o.thread) threads.push({ L: from, i, T: to, js, t: now, mine: o.own });
    return { k: k + 1, js };
  }
  function auto(now: number, vis: boolean) {
    const mov = L(0).live + L(1).live + L(2).live, r = Math.random();
    if (mov < cap * .3 || r < .09) { if (issue(L(N - 2), 1 + randInt(5), now, vis) >= 0) stats.minted++; return; }
    if (r < .23 && L(0).live) { transfer(0, pickLive(L(0)), now, { vis, thread: vis && Math.random() < .2 }); return; }
    if (r < .245) { const x = L(randInt(N)); kill(x, pickLive(x), now, vis); return; }
    let t = Math.random() * mov, k = 0;
    while (k < N - 2 && t >= L(k).live) { t -= L(k).live; k++; }
    transfer(k, pickLive(L(k)), now, { vis, split: Math.random() < .3, thread: vis && Math.random() < .16 });
  }
  function rotateNow(vis: boolean) {
    const d = layers.shift()!;
    if (vis) stats.expired += d.live;
    layers.push(pending || newLayer());
    pending = null;
    if (mine?.L === d) mine = null;
    flashes = flashes.filter((f) => f.L !== d);
    threads = threads.filter((t) => t.L !== d && t.T !== d);
    return d;
  }
  function warm() {
    gen = 1284; layers = []; pending = null; flashes = []; threads = []; mine = null;
    for (let k = 0; k < N; k++) layers.push(newLayer());
    const per = Math.round(rate * ROT / 1000);
    for (let r = 0; r < 5; r++) { for (let e = 0; e < per; e++) auto(0, false); rotateNow(false); }
    for (let e = 0, n = Math.round(per * .4); e < n; e++) auto(0, false);
    stats = { transfers: 0, expired: 0, minted: 0 };
  }

  function layout() {
    const w = wrap.clientWidth;
    if (!w) return;
    compact = w < 640;
    pitch = compact ? 8 : w < 1100 ? 10 : 12;
    size = compact ? 6 : pitch - 3;
    rows = compact ? 7 : 8;
    labelH = compact ? 26 : 32; perfH = compact ? 12 : 16;
    cols = Math.min(170, Math.floor(w / pitch));
    x0 = Math.round((w - cols * pitch) / 2 + (pitch - size) / 2);
    cellsH = rows * pitch; bandH = labelH + cellsH + perfH;
    W = w; H = bandH * N;
    const ar = align.getBoundingClientRect(), wr = wrap.getBoundingClientRect();
    lx = Math.max(16, Math.round(ar.left - wr.left + parseFloat(getComputedStyle(align).paddingLeft)));
    dpr = Math.min(2, window.devicePixelRatio || 1);
    cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr); cv.style.height = `${H}px`;
    const c = cols * rows;
    if (c !== cap) {
      cap = c;
      rate = cap * .7 / (ROT / 1000 * 1.3);
      warm();
      nextRot = performance.now() + ROT * .6;
      phase = 'run';
    }
  }
  function cellPos(layer: Layer, i: number) {
    let k = layers.indexOf(layer);
    if (k < 0) { if (layer === pending) k = N; else return null; }
    const c = (i / rows) | 0, r = i - c * rows;
    return { x: x0 + c * pitch, y: k * bandH - shift + labelH + r * pitch };
  }

  // Cells are batched by alpha so each frame is a handful of fills.
  function push(a: number, x: number, y: number, s: number) {
    if (a <= .02) return;
    buckets[Math.min(B - 1, Math.round(a * (B - 1)))]!.push(x, y, s);
  }
  function flush() {
    for (let b = 1; b < B; b++) {
      const arr = buckets[b]!;
      if (!arr.length) continue;
      ctx!.fillStyle = `rgba(${PAPER},${(b / (B - 1)).toFixed(3)})`;
      ctx!.beginPath();
      for (let j = 0; j < arr.length; j += 3) ctx!.rect(arr[j]!, arr[j + 1]!, arr[j + 2]!, arr[j + 2]!);
      ctx!.fill();
      arr.length = 0;
    }
    buckets[0]!.length = 0;
  }
  function bandCells(layer: Layer, yTop: number, alpha: number, now: number, dying: boolean, pinkList: number[]) {
    const cy = yTop + labelH;
    if (cy + cellsH < 0 || cy > H) return;
    let rr = 0, rf = 0;
    if (ripple) { const age = now - ripple.t; rr = age * 1.15; rf = 1 - age / RIPPLE; }
    const sm = Math.max(2, Math.round(size * .45)), dim = 1 - .62 * focus(now);
    for (let i = 0; i < cap; i++) {
      const c = (i / rows) | 0, r = i - c * rows, x = x0 + c * pitch, y = cy + r * pitch;
      let a: number, s: number;
      if (i >= layer.used) { s = 2; a = .16; }
      else if (layer.bits[i]) {
        // drawn on its own, above everything else
        if (isMine(layer, i)) continue;
        if (dying) { pinkList.push(x, y); continue; }
        // Brighter notes are worth more.
        s = size; a = .4 + .12 * layer.amt[i]!;
      } else { s = sm; a = .2; }
      a *= vital * alpha * dim;
      if (ripple && rf > 0) {
        const dx = x - ripple.x, dy = y - ripple.y, dd = Math.sqrt(dx * dx + dy * dy) - rr;
        if (dd > -70 && dd < 70) {
          const bst = Math.exp(-(dd * dd) / 1200) * rf * alpha;
          if (bst > .12) { s = size; a = Math.max(a, bst); }
        }
      }
      const o = (size - s) / 2;
      push(a, x + o, y + o, s);
    }
  }
  function frontier(layer: Layer, yTop: number, a: number) {
    if (!layer.used || layer.used >= cap) return;
    const c = (layer.used / rows) | 0, r = layer.used % rows, g = (pitch - size) / 2;
    const xa = x0 + c * pitch - g - .5, xb = xa + pitch;
    const y0 = yTop + labelH - 4, y1 = yTop + labelH + cellsH - 2 * g + 4;
    ctx!.strokeStyle = `rgba(${PAPER},${(.85 * vital * a).toFixed(3)})`;
    ctx!.lineWidth = 1;
    ctx!.beginPath();
    if (r === 0) { ctx!.moveTo(xa, y0); ctx!.lineTo(xa, y1); }
    else {
      const ym = yTop + labelH + r * pitch - g - .5;
      ctx!.moveTo(xb, y0); ctx!.lineTo(xb, ym); ctx!.lineTo(xa, ym); ctx!.lineTo(xa, y1);
    }
    ctx!.stroke();
    ctx!.fillStyle = ctx!.strokeStyle;
    ctx!.fillRect((r === 0 ? xa : xb) - 2.5, y0 - 1, 5, 2);
  }
  function labels(now: number) {
    ctx!.font = `500 ${compact ? 9 : 10.5}px "Martian Mono Variable", ui-monospace, monospace`;
    ctx!.letterSpacing = compact ? '0.03em' : '0.08em';
    ctx!.textBaseline = 'middle';
    const list: Array<[Layer, number]> = layers.map((x, k) => [x, k]);
    if (pending) list.push([pending, N]);
    for (const [layer, k] of list) {
      const yTop = k * bandH - shift, a = k === 0 && phase === 'slide' ? 1 - slideE : 1;
      if (yTop + bandH < 0 || yTop > H) continue;
      const y = yTop + labelH * .5 - 1;
      const dropping = k === 0 && phase !== 'run';
      const role = k >= N ? 'new last layer' : dropping ? `dropping · ${fmt(layer.live)} idle notes expire` : (compact ? ROLE_SHORT[k] : ROLE[k]);
      const left = (compact ? `gen ${layer.gen} · ` : `layer ${Math.min(k, N - 1)} · gen ${layer.gen} · `) + role;
      const right = compact ? `${fmt(layer.live)} live` : `${fmt(layer.used)} / ${fmt(cap)} bits used · ${fmt(layer.live)} live`;
      ctx!.fillStyle = `rgba(${PAPER},${(.9 * a * (.55 + .45 * vital)).toFixed(3)})`;
      let tx = lx;
      if (dropping) {
        const pa = .55 + .45 * Math.sin((now - phaseT) / 1000 * Math.PI * 4.4);
        ctx!.save();
        ctx!.fillStyle = `rgba(${PINK},${(pa * a).toFixed(3)})`;
        ctx!.fillRect(lx, y - 4, 8, 8);
        ctx!.restore();
        tx += 16;
      }
      ctx!.textAlign = 'left'; ctx!.fillText(left.toUpperCase(), tx, y);
      ctx!.textAlign = 'right'; ctx!.fillText(right.toUpperCase(), W - lx, y);
      // a row of perforations between layers
      const py = yTop + labelH + cellsH + perfH / 2 - 2;
      ctx!.fillStyle = `rgba(${PAPER},${(.24 * a).toFixed(3)})`;
      ctx!.beginPath();
      for (let x = 3; x < W; x += 7) ctx!.rect(x, py, 1.6, 1.6);
      ctx!.fill();
    }
  }
  // The visitor's note: full strength whatever the field is doing, with corner
  // marks, a slow ring and a tag, so it can be followed from layer to layer
  // until it is spent again or its layer drops.
  function drawMine(now: number) {
    if (!mine) return;
    const p = cellPos(mine.L, mine.i);
    if (!p) return;
    const a = layers.indexOf(mine.L) === 0 && phase === 'slide' ? 1 - slideE : 1;
    if (a <= .02) return;
    const c = ctx!, g = 4, x = p.x - g, y = p.y - g, s = size + 2 * g, arm = Math.max(3, Math.round(size * .5));
    c.fillStyle = `rgba(${PAPER},${a.toFixed(3)})`;
    c.fillRect(p.x, p.y, size, size);
    c.strokeStyle = c.fillStyle;
    c.lineWidth = 1.5;
    c.beginPath();
    for (const [cx, cy, dx, dy] of [[x, y, 1, 1], [x + s, y, -1, 1], [x, y + s, 1, -1], [x + s, y + s, -1, -1]] as const) {
      c.moveTo(cx + dx * arm, cy); c.lineTo(cx, cy); c.lineTo(cx, cy + dy * arm);
    }
    c.stroke();
    if (!isReduced()) {
      const u = ((now - focusT) % 1600) / 1600, e = g + 2 + u * 14;
      c.strokeStyle = `rgba(${PINK},${(a * (1 - u) * .9).toFixed(3)})`;
      c.strokeRect(p.x - e + .5, p.y - e + .5, size + 2 * e - 1, size + 2 * e - 1);
    }
    const text = `YOUR NOTE · ${serial(mine.L, mine.i)}`;
    c.font = `600 ${compact ? 9 : 10.5}px "Martian Mono Variable", ui-monospace, monospace`;
    c.letterSpacing = compact ? '0.03em' : '0.08em';
    c.textBaseline = 'middle'; c.textAlign = 'left';
    const bw = Math.ceil(c.measureText(text).width) + 16, bh = compact ? 18 : 22;
    let bx = x + s + 10, by = y - bh - 6;
    if (bx + bw > W - 4) bx = x - bw - 10;
    if (by < 2) by = y + s + 6;
    c.fillStyle = `rgba(${PAPER},${a.toFixed(3)})`;
    c.fillRect(bx, by, bw, bh);
    c.strokeStyle = c.fillStyle;
    c.lineWidth = 1;
    c.beginPath();
    const lx0 = bx > x ? bx : bx + bw, ly0 = by > y ? by : by + bh;
    c.moveTo(lx0, ly0); c.lineTo(bx > x ? x + s : x, by > y ? y + s : y);
    c.stroke();
    c.fillStyle = `rgba(${INK},${a.toFixed(3)})`;
    c.fillText(text, bx + 8, by + bh / 2 + .5);
  }
  function draw(now: number) {
    const reduced = isReduced();
    ctx!.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx!.clearRect(0, 0, W, H);
    slideE = phase === 'slide' ? ease(clamp((now - phaseT) / SLIDE, 0, 1)) : 0;
    shift = slideE * bandH;
    if (ripple && now - ripple.t > RIPPLE) ripple = null;
    const dim = 1 - .62 * focus(now), pinkList: number[] = [];
    for (let k = 0; k < N; k++) bandCells(L(k), k * bandH - shift, k === 0 && phase === 'slide' ? 1 - slideE : 1, now, k === 0 && phase !== 'run', pinkList);
    if (pending) bandCells(pending, N * bandH - shift, 1, now, false, pinkList);
    flush();
    if (pinkList.length) {
      const pa = (phase === 'slide' ? (1 - slideE) * .95 : .5 + .45 * Math.sin((now - phaseT) / 1000 * Math.PI * 4.4)) * dim;
      ctx!.fillStyle = `rgba(${PINK},${pa.toFixed(3)})`;
      ctx!.beginPath();
      for (let j = 0; j < pinkList.length; j += 2) ctx!.rect(pinkList[j]!, pinkList[j + 1]!, size, size);
      ctx!.fill();
    }
    for (let k = 0; k < N; k++) frontier(L(k), k * bandH - shift, (k === 0 && phase === 'slide' ? 1 - slideE : 1) * dim);
    if (pending) frontier(pending, N * bandH - shift, dim);
    if (!reduced) flashes = flashes.filter((f) => now - f.t < (f.mine ? FLASH_MINE : FLASH));
    for (const f of flashes) {
      const p = cellPos(f.L, f.i);
      if (!p) continue;
      const u = clamp((now - f.t) / (f.mine ? FLASH_MINE : FLASH), 0, 1), a = Math.pow(1 - u, 1.6) * (f.mine ? 1 : dim);
      if (f.kind === 1) {
        ctx!.fillStyle = `rgba(${PINK},${a.toFixed(3)})`;
        ctx!.fillRect(p.x - 1, p.y - 1, size + 2, size + 2);
      } else {
        const e = f.kind === 2 ? 2 + u * 6 : 3 + Math.abs(Math.sin(u * Math.PI * 5)) * 4;
        ctx!.strokeStyle = `rgba(${PINK},${a.toFixed(3)})`;
        ctx!.lineWidth = f.kind === 2 ? 1.5 : 2;
        ctx!.strokeRect(p.x - e + .5, p.y - e + .5, size + 2 * e - 1, size + 2 * e - 1);
      }
    }
    if (!reduced) threads = threads.filter((t) => now - t.t < (t.mine ? THREAD_MINE : THREAD));
    for (const th of threads) {
      const p = cellPos(th.L, th.i);
      if (!p) continue;
      const u = clamp((now - th.t) / (th.mine ? THREAD_MINE : THREAD), 0, 1), grow = reduced ? 1 : Math.min(1, u / .22);
      const a = (u < .2 ? 1 : Math.pow(1 - (u - .2) / .8, 1.3)) * (th.mine ? 1 : dim);
      ctx!.lineWidth = th.mine ? 2.5 : 1.5;
      ctx!.strokeStyle = `rgba(${PINK},${a.toFixed(3)})`;
      for (const j of th.js) {
        const q = cellPos(th.T, j);
        if (!q) continue;
        const x1 = p.x + size / 2, y1 = p.y + size / 2, x2 = q.x + size / 2, y2 = q.y + size / 2, my = (y1 + y2) / 2;
        ctx!.beginPath();
        ctx!.moveTo(x1, y1);
        const steps = 28;
        for (let s = 1; s <= steps; s++) { const t = grow * s / steps; ctx!.lineTo(bez(x1, x1, x2, x2, t), bez(y1, my, my, y2, t)); }
        ctx!.stroke();
      }
    }
    labels(now);
    drawMine(now);
    if (hover) {
      const p = cellPos(hover.L, hover.i);
      if (p) { ctx!.strokeStyle = `rgba(${PAPER},1)`; ctx!.lineWidth = 1.5; ctx!.strokeRect(p.x - 2.75, p.y - 2.75, size + 5.5, size + 5.5); }
    }
  }
  function hud(now: number) {
    const reduced = isReduced();
    out.t.textContent = fmt(stats.transfers);
    out.l.textContent = fmt(layers.reduce((s, x) => s + x.live, 0));
    out.e.textContent = fmt(stats.expired);
    out.r.textContent = reduced ? 'paused' : phase === 'run' ? `${Math.max(0, Math.ceil((nextRot - now) / 1000))} s` : 'now';
    const k = mine ? layers.indexOf(mine.L) : -1, label = k >= 0 && room(k) ? 'Spend it again' : 'Spend a note';
    if (spendBtn.textContent !== label) spendBtn.textContent = label;
    const st = reduced ? 'paused' : isIdle() ? 'idle' : 'moving';
    if (out.chip.dataset.state !== st || !out.chip.textContent) {
      out.chip.dataset.state = st;
      out.chip.textContent = st === 'paused' ? 'Reduced motion: it moves when you click' : st === 'idle' ? 'Idle: nothing moves, rotations go on' : 'Moving';
    }
  }
  function say(text: string, aloud = false) { msg.textContent = text; if (aloud) live.textContent = text; }

  function frame(now: number) {
    raf = 0;
    if (!running) return;
    const dt = Math.min(80, now - last); last = now;
    const idle = isIdle();
    vital += ((idle ? .45 : 1) - vital) * (1 - Math.exp(-dt / 900));
    activity += ((idle ? 0 : 1) - activity) * (1 - Math.exp(-dt / 350));
    if (phase === 'run' && now >= nextRot - PULSE) { phase = 'pulse'; phaseT = now; }
    if (phase === 'pulse' && now >= nextRot) { phase = 'slide'; phaseT = now; pending = newLayer(); }
    if (phase === 'slide' && now - phaseT >= SLIDE) {
      const own = mine?.L === L(0) ? serial(mine.L, mine.i) : null;
      const d = rotateNow(true);
      phase = 'run';
      nextRot += ROT;
      if (nextRot < now + 1000) nextRot = now + ROT;
      if (own) say(`Rotation: generation ${d.gen} dropped, and your note ${own} expired with it${d.live > 1 ? `, along with ${fmt(d.live - 1)} others nobody moved` : ''}.`, true);
      else say(`Rotation: generation ${d.gen} dropped${d.live ? `, and the ${fmt(d.live)} notes nobody moved out of it expired.` : '. Nothing was left in it.'}`);
    }
    if (phase !== 'slide') {
      acc += dt / 1000 * rate * activity;
      const vis = focus(now) === 0;
      let n = 0;
      while (acc >= 1 && n++ < 40) { acc -= 1; auto(now, vis); }
    }
    draw(now);
    if (now - hudT > 200) { hudT = now; hud(now); }
    raf = requestAnimationFrame(frame);
  }
  function setRunning(v: boolean) {
    v = v && !isReduced();
    if (v === running) return;
    const now = performance.now();
    running = v;
    if (!v) { pausedAt = now; if (raf) cancelAnimationFrame(raf); raf = 0; return; }
    // Time spent off screen doesn't count towards the next rotation.
    const gap = pausedAt ? now - pausedAt : 0;
    nextRot += gap; phaseT += gap;
    for (const f of flashes) f.t += gap;
    for (const t of threads) t.t += gap;
    if (ripple) ripple.t += gap;
    last = now;
    raf = requestAnimationFrame(frame);
  }

  function cellAt(e: PointerEvent | MouseEvent): Hit | null {
    if (phase === 'slide') return null;
    const r = cv.getBoundingClientRect(), px = e.clientX - r.left, py = e.clientY - r.top;
    const k = Math.floor(py / bandH);
    if (k < 0 || k >= N) return null;
    const yy = py - k * bandH - labelH;
    if (yy < -1 || yy >= cellsH) return null;
    const c = Math.floor((px - x0 + (pitch - size) / 2) / pitch), rr = clamp(Math.floor(yy / pitch), 0, rows - 1);
    if (c < 0 || c >= cols) return null;
    return { k, L: L(k), i: c * rows + rr };
  }
  function describe(h: Hit) {
    const { L: layer, k, i } = h, s = serial(layer, i);
    if (i >= layer.used) return `<b>Bit not issued yet</b><span>Layer ${k} hands out its bits in order.</span>`;
    if (!layer.bits[i]) return `<b>Note ${s}</b><span>Spent. Its bit is 0.</span>`;
    const amt = 1 << layer.amt[i]!, hops = N - 1 - k, dies = k + 1;
    return `<b>${isMine(layer, i) ? 'Your note' : 'Note'} ${s} · ${amt} ${amt === 1 ? 'unit' : 'units'}</b><span>` +
      (k === N - 1 ? 'Frozen until the next rotation' : `${hops} ${hops === 1 ? 'hop' : 'hops'} left · click to spend`) +
      `</span><span>Dies ${dies === 1 ? 'at the next rotation' : `in ${dies} rotations`} if left alone</span>`;
  }
  function showTip(h: Hit | null) {
    if (!h) { tip.hidden = true; return; }
    tip.innerHTML = describe(h);
    tip.hidden = false;
    const p = cellPos(h.L, h.i);
    if (!p) { tip.hidden = true; return; }
    const tw = tip.offsetWidth, th = tip.offsetHeight;
    let x = p.x + size + 14, y = p.y - th - 10;
    if (x + tw > W - 8) x = p.x - tw - 14;
    if (x < 8) x = 8;
    if (y < 4) y = p.y + size + 12;
    tip.style.transform = `translate(${Math.round(x)}px,${Math.round(y)}px)`;
  }
  function act(h: Hit) {
    const now = performance.now(), { L: layer, k, i } = h, s = serial(layer, i);
    if (i >= layer.used) say('That bit has not been issued yet. Each layer hands out its bits in order.', true);
    else if (!layer.bits[i]) say(`Note ${s} is already spent. The mint would answer “spent”.`, true);
    else if (k === N - 1) {
      flashes.push({ L: layer, i, t: now, kind: 3 });
      say(`Note ${s} is frozen. The last layer cannot move until the next rotation.`, true);
    } else {
      const r = transfer(k, i, now, { vis: true, thread: true, own: true });
      if ('err' in r) say('The next layer is full and stays full until a rotation drops it. This note’s layer is dropped first, so the note cannot move on.', true);
      else {
        mine = { L: L(r.k), i: r.js[0]! };
        focusT = now;
        say(`You spent ${s}. The mint cleared its bit and issued ${serial(mine.L, mine.i)} one layer on, to a fresh key. That note is yours, marked in the field` +
          (r.k === N - 1 ? ', and frozen until the next rotation.' : '.'), true);
      }
    }
    if (!running) { draw(now); hud(now); }
  }

  cv.addEventListener('pointermove', (e) => {
    const h = cellAt(e);
    hover = h;
    showTip(h);
    cv.style.cursor = h && h.i < h.L.used && h.L.bits[h.i] ? 'pointer' : 'default';
    if (!running) draw(performance.now());
  });
  cv.addEventListener('pointerleave', () => { hover = null; tip.hidden = true; if (!running) draw(performance.now()); });
  cv.addEventListener('click', (e) => { const h = cellAt(e); if (h) { act(h); showTip(h); } });
  spendBtn.addEventListener('click', () => {
    // Spend the visitor's own note while it can still move, else pick one. A layer whose
    // next layer has room comes first; if none has room, the refusal says why.
    const own = mine ? layers.indexOf(mine.L) : -1;
    if (mine && own >= 0 && room(own)) { act({ k: own, L: mine.L, i: mine.i }); return; }
    const live = [0, 1, 2].filter((k) => L(k).live);
    if (!live.length) {
      if (issue(L(N - 2), 3, performance.now(), true) < 0) { say('Nothing can move: every live note is frozen in the last layer, and the layer new notes land in is full. The next rotation thaws them.', true); return; }
      stats.minted++; live.push(N - 2);
    }
    const cand = live.some(room) ? live.filter(room) : live;
    let best = cand[0]!;
    for (const k of cand) if (L(k).live > L(best).live) best = k;
    // pickLive passes over the visitor's note, which can be the only live one left there.
    const i = pickLive(L(best));
    act({ k: best, L: L(best), i: i < 0 && mine?.L === L(best) ? mine.i : i });
  });

  onIdleChange((idle) => {
    if (idle) { say('Nobody is moving, so nothing is transferred. Rotations go on, and each one drops a layer with every note in it.'); return; }
    if (!running) return;
    const r = cv.getBoundingClientRect(), ptr = lastPointer();
    let x = W / 2, y = H * .3;
    if (ptr) { x = clamp(ptr.x - r.left, 0, W); y = clamp(ptr.y - r.top, -300, H + 300); }
    ripple = { x, y, t: performance.now() };
    say('Moving again. Transfers resume, and new notes are minted where old ones expired.');
  });
  onMotionChange(() => {
    if (isReduced()) { flashes = []; threads = []; vital = 1; draw(performance.now()); hud(performance.now()); }
  });

  layout();
  draw(performance.now());
  hud(performance.now());
  new ResizeObserver(() => { if (wrap.clientWidth !== W) { layout(); draw(performance.now()); } }).observe(wrap);
  document.fonts?.ready.then(() => draw(performance.now()));
  whileVisible(cv, setRunning);
}
