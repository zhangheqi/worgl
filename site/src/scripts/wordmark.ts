/* The footer's wordmark in bits. It erodes while the visitor is idle and prints itself again under the pointer. */
import { isIdle, isReduced, whileVisible, PAPER, PINK } from './motion';
import { wordBitmap } from './word';

const WORD = wordBitmap();

export function mountWordmark(cv: HTMLCanvasElement) {
  const ctx = cv.getContext('2d');
  const host = cv.parentElement;
  if (!ctx || !host) return;
  let W = 0, H = 0, dpr = 1, p = 12, s = 9, cols = 0, rows = 0;
  let on = new Uint8Array(0), life = new Uint8Array(0), fl = new Float64Array(0);
  let running = false, raf = 0, last = 0;
  let ptr: { x: number; y: number } | null = null;

  function layout() {
    const cs = getComputedStyle(host!);
    const w = Math.floor(host!.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight));
    if (w <= 0 || w === W) return;
    W = w;
    const sub = W < 640 ? 3 : 4;
    cols = WORD.w * sub; rows = WORD.h * sub;
    p = Math.max(3, Math.floor(W / cols)); s = Math.max(2, Math.round(p * .76));
    on = new Uint8Array(cols * rows); life = new Uint8Array(cols * rows); fl = new Float64Array(cols * rows).fill(-1e9);
    for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) {
      const i = r * cols + c;
      on[i] = WORD.at(Math.floor(c / sub), Math.floor(r / sub));
      life[i] = on[i]!;
    }
    H = rows * p; dpr = Math.min(2, window.devicePixelRatio || 1);
    cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr); cv.style.height = `${H}px`;
    draw(performance.now());
  }

  function draw(now: number) {
    if (!on.length) return;
    ctx!.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx!.clearRect(0, 0, W, H);
    ctx!.fillStyle = `rgba(${PAPER},.96)`;
    ctx!.beginPath();
    for (let i = 0; i < on.length; i++) if (on[i] && life[i]) ctx!.rect((i % cols) * p, ((i / cols) | 0) * p, s, s);
    ctx!.fill();
    ctx!.fillStyle = `rgba(${PAPER},.2)`;
    ctx!.beginPath();
    const ds = Math.max(1, Math.round(s * .3)), o = (s - ds) / 2;
    for (let i = 0; i < on.length; i++) if (on[i] && !life[i]) ctx!.rect((i % cols) * p + o, ((i / cols) | 0) * p + o, ds, ds);
    ctx!.fill();
    for (let i = 0; i < on.length; i++) {
      const u = (now - fl[i]!) / 900;
      if (u >= 1 || u < 0) continue;
      ctx!.fillStyle = `rgba(${PINK},${Math.pow(1 - u, 1.5).toFixed(3)})`;
      ctx!.fillRect((i % cols) * p, ((i / cols) | 0) * p, s, s);
    }
  }

  function frame(now: number) {
    raf = 0;
    if (!running) return;
    const dt = Math.min(80, now - last); last = now;
    if (isIdle()) {
      const pk = .07 * dt / 1000;
      for (let i = 0; i < on.length; i++) if (on[i] && life[i] && Math.random() < pk) life[i] = 0;
    } else {
      const pk = .45 * dt / 1000;
      for (let i = 0; i < on.length; i++) if (on[i] && !life[i] && Math.random() < pk) { life[i] = 1; fl[i] = now; }
    }
    if (ptr) {
      const R = 64;
      const c0 = Math.max(0, Math.floor((ptr.x - R) / p)), c1 = Math.min(cols - 1, Math.ceil((ptr.x + R) / p));
      const r0 = Math.max(0, Math.floor((ptr.y - R) / p)), r1 = Math.min(rows - 1, Math.ceil((ptr.y + R) / p));
      for (let r = r0; r <= r1; r++) for (let c = c0; c <= c1; c++) {
        const i = r * cols + c, dx = c * p - ptr.x, dy = r * p - ptr.y;
        if (on[i] && !life[i] && dx * dx + dy * dy < R * R) { life[i] = 1; fl[i] = now; }
      }
    }
    draw(now);
    raf = requestAnimationFrame(frame);
  }

  function setRunning(v: boolean) {
    v = v && !isReduced();
    if (v === running) return;
    running = v;
    if (v) { last = performance.now(); raf = requestAnimationFrame(frame); }
    else {
      if (raf) cancelAnimationFrame(raf);
      raf = 0;
      life.set(on);
      draw(performance.now());
    }
  }

  cv.addEventListener('pointermove', (e) => { const r = cv.getBoundingClientRect(); ptr = { x: e.clientX - r.left, y: e.clientY - r.top }; });
  cv.addEventListener('pointerleave', () => { ptr = null; });
  new ResizeObserver(layout).observe(host);
  whileVisible(cv, setRunning, '0px');
}
