/* The header takes the colour of the band beneath it, and on a phone its links fold into a menu. */
const root = document.documentElement;
const header = document.querySelector<HTMLElement>('header.site');

if (header) {
  const toned = Array.from(document.querySelectorAll<HTMLElement>('[data-tone]'));
  let raf = 0;
  const tone = () => {
    raf = 0;
    const y = header.getBoundingClientRect().bottom - 2;
    let t = toned[0]?.dataset.tone ?? 'cobalt';
    for (const s of toned) {
      const r = s.getBoundingClientRect();
      if (r.top <= y && r.bottom > y) { t = s.dataset.tone ?? t; break; }
    }
    root.classList.toggle('hdr-paper', t === 'paper');
    root.classList.toggle('hdr-night', t === 'night');
  };
  addEventListener('scroll', () => { if (!raf) raf = requestAnimationFrame(tone); }, { passive: true });
  addEventListener('resize', tone);
  tone();

  const btn = header.querySelector<HTMLButtonElement>('.menu-btn');
  const nav = header.querySelector<HTMLElement>('.nav');
  if (btn && nav) {
    const set = (open: boolean) => {
      nav.classList.toggle('open', open);
      btn.setAttribute('aria-expanded', String(open));
      btn.textContent = open ? 'Close' : 'Menu';
    };
    btn.addEventListener('click', () => set(!nav.classList.contains('open')));
    nav.addEventListener('click', (e) => { if ((e.target as Element).closest('a')) set(false); });
    addEventListener('keydown', (e) => { if (e.key === 'Escape' && nav.classList.contains('open')) { set(false); btn.focus(); } });
  }
}
