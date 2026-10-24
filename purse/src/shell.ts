// The shell's own parts, shared by both views: the brand and the desktop
// bar. The bands themselves are each view's.

import { h } from './dom.ts';
import { wordmark } from './marks.ts';

/**
 * The brand: the wordmark in bits and "purse" in the serif italic. A link
 * home once the purse holds a mint; before that there is no home to go to,
 * so it is an image with a name. `key` keeps its focus when the view is
 * drawn again: one for the phone's top bar, one for the desktop's.
 */
export function brand(link: boolean, key = 'brand'): HTMLElement {
  const inside = [wordmark(3), h('span', { class: 'purse', 'aria-hidden': 'true' }, 'purse')];
  return link
    ? h('a', { class: 'brand', href: '#home', 'aria-label': 'Worgl Purse, home', 'data-fk': key }, ...inside)
    : h('span', { class: 'brand', role: 'img', 'aria-label': 'Worgl Purse' }, ...inside);
}

/** The desktop bar, from 1100 px: the brand alone. */
export const deskbar = (link: boolean): HTMLElement =>
  h('header', { class: 'db' }, h('div', { class: 'db-in' }, brand(link, 'brand-d')));
