/* Tabs that follow the ARIA tabs pattern: arrow keys move between them. */
export function mountTabs(list: HTMLElement) {
  const tabs = Array.from(list.querySelectorAll<HTMLElement>('[role="tab"]'));
  function pick(t: HTMLElement, focus = false) {
    for (const x of tabs) {
      const on = x === t;
      x.setAttribute('aria-selected', String(on));
      x.tabIndex = on ? 0 : -1;
      const panel = document.getElementById(x.getAttribute('aria-controls') ?? '');
      if (panel) panel.hidden = !on;
    }
    if (focus) t.focus();
  }
  tabs.forEach((t, i) => {
    t.addEventListener('click', () => pick(t));
    t.addEventListener('keydown', (e) => {
      if (e.key === 'ArrowRight') pick(tabs[(i + 1) % tabs.length]!, true);
      if (e.key === 'ArrowLeft') pick(tabs[(i - 1 + tabs.length) % tabs.length]!, true);
    });
  });
}

/* A copy button: copies its target's data-text, or selects the text if the clipboard refuses. */
export function mountCopy(btn: HTMLButtonElement) {
  const src = document.querySelector<HTMLElement>(btn.dataset.copy ?? '');
  if (!src) return;
  const select = () => {
    const r = document.createRange();
    r.selectNodeContents(src);
    const s = getSelection();
    s?.removeAllRanges();
    s?.addRange(r);
  };
  btn.addEventListener('click', () => {
    const done = (ok: boolean) => {
      btn.textContent = ok ? 'Copied' : 'Selected';
      setTimeout(() => { btn.textContent = 'Copy'; }, 1600);
    };
    const text = src.dataset.text ?? src.innerText;
    if (!navigator.clipboard) { select(); done(false); return; }
    navigator.clipboard.writeText(text).then(() => done(true), () => { select(); done(false); });
  });
}
