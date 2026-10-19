/* Wörgl's notes took a 1% stamp every month: a strip of months, stamped as it scrolls into view. */
import { isReduced, onMotionChange } from './motion';

const MONTHS = ['Aug', 'Sep', 'Oct', 'Nov', 'Dec', 'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug'];

export function mountStrip(strip: HTMLElement) {
  strip.innerHTML = MONTHS.map((m, i) => `<div class="month" style="--rot:${(i * 37 % 11) - 5}deg"><span class="label">${m} ${i < 5 ? 1932 : 1933}</span><span class="st">1%</span></div>`).join('') +
    '<div class="month void"><span class="label">Sep 1933</span><span class="st">Ended</span></div>';
  const boxes = Array.from(strip.querySelectorAll<HTMLElement>('.month:not(.void)'));
  const stampAll = () => boxes.forEach((b) => b.classList.add('stamped'));
  if (isReduced()) { stampAll(); return; }
  onMotionChange(() => { if (isReduced()) stampAll(); });
  const io = new IntersectionObserver((es) => {
    if (!es.some((e) => e.isIntersecting)) return;
    io.disconnect();
    boxes.forEach((b, i) => setTimeout(() => b.classList.add('stamped'), 180 + i * 150));
  }, { threshold: .45 });
  io.observe(strip);
}
