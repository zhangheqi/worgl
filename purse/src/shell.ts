// The shell's own parts, shared by the views: the brand, the desktop bar
// (with Receive and Send once the purse holds a mint) and the flows' top
// bar on the phone. The bands themselves are each view's.

import { h } from './dom.ts';
import { backIcon, downIcon, regIcon, upIcon } from './icons.ts';
import { face, wordmark } from './marks.ts';

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

/** Where the desktop bar is drawn: the first run (the brand alone), or one of the views it leads to. */
export type Bar = 'first' | 'home' | 'receive' | 'send' | 'history';

/**
 * The desktop bar, from 1100 px: the brand, and past the first run the
 * purse's sections after it and Receive and Send at its right, the one
 * shown marked as the current page.
 */
export function deskbar(link: boolean, current: Bar = 'first'): HTMLElement {
  if (current === 'first') {
    return h('header', { class: 'db' }, h('div', { class: 'db-in' }, brand(link, 'brand-d')));
  }
  const page = (bar: Bar): string | undefined => (current === bar ? 'page' : undefined);
  const nav = h(
    'nav',
    { 'aria-label': 'Purse sections' },
    h('a', { href: '#home', 'aria-current': page('home'), 'data-fk': 'db-home' }, 'Purse'),
    h('a', { href: '#history', 'aria-current': page('history'), 'data-fk': 'db-history' }, 'History'),
  );
  const acts = h(
    'div',
    { class: 'acts' },
    h('a', { class: 'btn', href: '#receive', 'aria-current': page('receive'), 'data-fk': 'db-receive' }, downIcon(), 'Receive'),
    h('a', { class: 'btn solid', href: '#send', 'aria-current': page('send'), 'data-fk': 'db-send' }, upIcon(), 'Send'),
  );
  return h('header', { class: 'db' }, h('div', { class: 'db-in' }, brand(link, 'brand-d'), nav, acts));
}

/**
 * A flow's top bar on the phone: back to the purse, the flow's name on a
 * rule, and the mint's face, a mark until the Mints screen comes.
 */
export function tbSub(title: string, mint: string, name: string): HTMLElement {
  return h(
    'header',
    { class: 'tb' },
    h('a', { class: 'ib back', href: '#home', 'aria-label': 'Back to the purse', 'data-fk': 'tb-back' }, backIcon()),
    h('div', { class: 'subhead' }, h('span', { class: 'lab' }, title), h('span', { class: 'rule' }), regIcon()),
    h('span', { class: 'ib face', role: 'img', 'aria-label': `Mint: ${name}` }, face(mint, 4.4, 3.4)),
  );
}
