/*
 * The site follows Worgl's own rule: what moves stays bright, what sits still
 * fades. This module tracks two things every moving figure listens to: whether
 * the visitor prefers reduced motion, and whether they have gone idle.
 */
const root = document.documentElement;
const rmq = matchMedia('(prefers-reduced-motion: reduce)');
let reduced = rmq.matches;
const motionSubs: Array<() => void> = [];

export const IDLE_MS = 5000;
let lastActive = performance.now();
let idle = false;
let pointer: { x: number; y: number } | null = null;
const idleSubs: Array<(idle: boolean) => void> = [];

/** True when the visitor asked for reduced motion: figures draw a still frame and change only when clicked. */
export const isReduced = () => reduced;
/** True after IDLE_MS without a pointer, key, wheel, touch or scroll. */
export const isIdle = () => idle;
/** Where the pointer last was, in viewport coordinates. */
export const lastPointer = () => pointer;

export function onMotionChange(f: () => void) { motionSubs.push(f); }
export function onIdleChange(f: (idle: boolean) => void) { idleSubs.push(f); }

function setIdle(v: boolean) {
  if (v === idle) return;
  idle = v;
  root.classList.toggle('is-idle', v);
  idleSubs.forEach((f) => f(v));
}

rmq.addEventListener('change', (e) => {
  reduced = e.matches;
  if (reduced) setIdle(false);
  motionSubs.forEach((f) => f());
});

function wake(e: Event) {
  if ('clientX' in e && typeof e.clientX === 'number') pointer = { x: e.clientX, y: (e as PointerEvent).clientY };
  lastActive = performance.now();
  setIdle(false);
}
for (const t of ['pointermove', 'pointerdown', 'wheel', 'keydown', 'touchstart', 'scroll']) {
  addEventListener(t, wake, { passive: true, capture: true });
}
setInterval(() => {
  if (!reduced && !idle && performance.now() - lastActive > IDLE_MS) setIdle(true);
}, 250);

/**
 * Runs `set(true)` while `el` is on screen and the tab is visible, and
 * `set(false)` otherwise, so nothing animates where nobody can see it.
 */
export function whileVisible(el: Element, set: (on: boolean) => void, rootMargin = '80px') {
  let inView = false;
  const update = () => set(inView && !document.hidden);
  new IntersectionObserver((es) => {
    inView = es.some((e) => e.isIntersecting);
    update();
  }, { rootMargin }).observe(el);
  document.addEventListener('visibilitychange', update);
  onMotionChange(update);
}

export const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));
export const randInt = (n: number) => Math.floor(Math.random() * n);
export const fmt = (n: number) => Math.round(n).toLocaleString('en-US');
export const PAPER = '243,243,239';
export const PINK = '255,79,163';
