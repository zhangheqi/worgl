// Building the page from DOM calls alone. Text goes in as text nodes and
// styles through the CSSOM, never through markup or a `style` attribute, so
// that nothing a mint or a link says can become markup, and the page runs
// under `require-trusted-types-for 'script'` and `style-src 'self'`.

/** What an element may hold: nodes, text, and nothing for what is left out. */
export type Child = Node | string | number | null | undefined | false;

/** An element's attributes, its CSS custom properties and styles, and its listeners. */
export interface Props {
  [attribute: string]: string | number | boolean | undefined | Record<string, string> | Listeners;
  /** Styles, set one by one: `{ transform: 'translateX(4px)', '--pd': '-1s' }`. */
  style?: Record<string, string>;
  /** Event listeners. */
  on?: Listeners;
}

/** Event listeners by event name. */
export type Listeners = { [K in keyof HTMLElementEventMap]?: (event: HTMLElementEventMap[K]) => void };

const SVG = 'http://www.w3.org/2000/svg';

/** The children that are drawn: nodes and text, numbers as text, and nothing for what is left out. */
const drawn = (children: readonly Child[]): (Node | string)[] =>
  children.filter((child): child is Node | string | number => child !== null && child !== undefined && child !== false).map((child) => (typeof child === 'number' ? String(child) : child));

/** Puts `children` in place of what `el` holds: for the parts of a view redrawn in place. */
export function put(el: Element, ...children: Child[]): void {
  el.replaceChildren(...drawn(children));
}

function apply(el: Element & ElementCSSInlineStyle, props: Props | undefined, children: Child[]): void {
  for (const [name, value] of Object.entries(props ?? {})) {
    if (name === 'style') {
      for (const [property, style] of Object.entries(value as Record<string, string>)) {
        el.style.setProperty(property.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`), style);
      }
    } else if (name === 'on') {
      for (const [event, listener] of Object.entries(value as Listeners)) {
        el.addEventListener(event, listener as EventListener);
      }
    } else if (value === true) {
      el.setAttribute(name, '');
    } else if (value !== false && value !== undefined) {
      el.setAttribute(name, String(value));
    }
  }
  el.append(...drawn(children));
}

/** An HTML element. */
export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props?: Props,
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  apply(el, props, children);
  return el;
}

/** An SVG element. */
export function s<K extends keyof SVGElementTagNameMap>(
  tag: K,
  props?: Props,
  ...children: Child[]
): SVGElementTagNameMap[K] {
  const el = document.createElementNS(SVG, tag);
  apply(el, props, children);
  return el;
}

/** The first element matching `selector`. */
export const $ = <E extends Element = HTMLElement>(selector: string, root: ParentNode = document): E | null =>
  root.querySelector<E>(selector);

/** Every element matching `selector`. */
export const $$ = <E extends Element = HTMLElement>(selector: string, root: ParentNode = document): E[] => [
  ...root.querySelectorAll<E>(selector),
];

/** Whether the person asks for less motion. */
export function reduced(): boolean {
  try {
    return matchMedia('(prefers-reduced-motion: reduce)').matches;
  } catch {
    return false;
  }
}

/** Says `message` in the live region, cleared first so that the same words are said again. */
export function announce(message: string): void {
  const el = document.getElementById('live');
  if (el === null) {
    return;
  }
  el.textContent = '';
  setTimeout(() => {
    el.textContent = message;
  }, 30);
}
