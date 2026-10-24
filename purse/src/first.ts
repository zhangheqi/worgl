// The first run (#first): what the purse is, the three states a token can
// be in, and the way in: the build's playground, a mint link, or a mint's
// stamp read from its server, with its face and key, before it is added.

import { type Child, h } from './dom.ts';
import { periodWords } from './format.ts';
import { hostOf, shortKey } from './link.ts';
import { bits, face, swatch } from './marks.ts';
import type { About } from './purses.ts';
import type { MintRecord } from './records.ts';
import { brand, deskbar } from './shell.ts';
import { legend } from './words.ts';

/** What the first run is showing in its cobalt band. */
export interface FirstState {
  /** The mint a link named, if one did. */
  link: { mint: string; server: string } | undefined;
  /** What the purse read of that mint, or of the playground. */
  about: About | undefined;
  /** Whether it is reading a mint now. */
  reading: boolean;
  /** Whether the paste row is open (it always is without a playground). */
  pasting: boolean;
  /** What is in the paste row. */
  paste: string;
  /** What the purse is doing for the person now. */
  busy: 'take' | 'add' | undefined;
  /** What went wrong, in plain words. */
  error: string | undefined;
}

/** What the first run calls when the person acts. */
export interface FirstActions {
  take: () => void;
  add: () => void;
  openPaste: () => void;
  pasteChanged: (text: string) => void;
  submitPaste: () => void;
}

/** Everything the first run draws from. */
export interface FirstInput {
  state: FirstState;
  records: readonly MintRecord[];
  /** The build's playground, when it names one and the purse holds no mint yet. */
  playground: { mint: string; server: string } | undefined;
  /** Whether the purse runs as a Home Screen app, so the hint about it is moot. */
  standalone: boolean;
  actions: FirstActions;
}

/** The first run. */
export function firstView(input: FirstInput): HTMLElement {
  return h(
    'div',
    { class: 'view v-first v-sub flow' },
    deskbar(input.records.length > 0),
    h('main', { class: 'vmain' }, paperBand(input), cobaltBand(input)),
  );
}

function paperBand(input: FirstInput): HTMLElement {
  const rows = legend().map(({ kind, name, words }) =>
    h('li', {}, swatch(kind), h('p', {}, h('b', {}, name), ' ', h('span', {}, words))),
  );
  return h(
    'section',
    { class: 'band-p' },
    h(
      'div',
      { class: 'wrap' },
      h('header', { class: 'tb' }, brand(input.records.length > 0)),
      h('p', { class: 'lab muted intro-lab' }, 'No sign-up'),
      h('h1', { class: 'h1', tabindex: -1, 'data-fk': 'h1' }, 'A purse that lives in ', h('em', {}, 'this'), ' browser.'),
      h(
        'p',
        { class: 'body' },
        'Tokens come in notes of 1, 2, 4 … 64. The purse draws one square per token, in three states:',
      ),
      h(
        'ul',
        { class: 'legend3', 'aria-label': 'How the field reads' },
        ...rows,
      ),
    ),
  );
}

function cobaltBand(input: FirstInput): HTMLElement {
  const { state, records, playground } = input;
  const link = state.link;
  let parts: Child[];
  if (link !== undefined) {
    const held = records.find((record) => record.mint === link.mint);
    if (state.about !== undefined) {
      parts = linkStamp(input, link, state.about, held);
    } else if (state.reading && state.paste === '') {
      // a link opened, not pasted: there is no paste row to show "Adding…"
      parts = [waiting(link.server), low([], input)];
    } else {
      parts = [pasteBlock(input)];
    }
  } else if (playground !== undefined && records.length === 0 && (state.about !== undefined || state.reading)) {
    parts =
      state.about === undefined
        ? [waiting(playground.server), low([], input)]
        : playgroundStamp(input, playground, state.about);
  } else {
    parts = [pasteBlock(input)];
  }
  return h('section', { class: 'band-c' }, h('div', { class: 'wrap' }, ...parts));
}

/** While the purse reads a mint from its server. */
const waiting = (server: string): HTMLElement =>
  h('p', { class: 'note-line muted stamp-wait c-mid', role: 'status' }, `Reading the mint at ${hostOf(server)}…`);

/** The way in without a stamp: the paste row, always open, in the middle of the band. */
function pasteBlock(input: FirstInput): HTMLElement {
  return h(
    'div',
    { class: 'c-mid first-acts flush' },
    h('p', { class: 'lab muted' }, 'Add a mint'),
    pasteRow(input),
    errorLine(input.state.error),
    h(
      'p',
      { class: 'note-line muted' },
      'The key is the mint’s real name. Its face is drawn from the key, so you can compare it with theirs.',
    ),
    notes(input.standalone),
  );
}

/** The paste row: a mint link, and Add. */
function pasteRow(input: FirstInput): HTMLElement {
  const { state, actions } = input;
  const busy = state.reading;
  return h(
    'form',
    {
      class: 'paste-row',
      'data-form': 'first-link',
      on: {
        submit: (event) => {
          event.preventDefault();
          actions.submitPaste();
        },
      },
    },
    h('label', { class: 'sr', for: 'first-link' }, 'Mint link'),
    h('input', {
      class: 'inp mono',
      id: 'first-link',
      'data-fk': 'first-link',
      placeholder: 'Paste the mint’s link',
      autocomplete: 'off',
      autocapitalize: 'off',
      spellcheck: 'false',
      value: state.paste,
      on: { input: (event) => actions.pasteChanged((event.target as HTMLInputElement).value) },
    }),
    h('button', { class: 'btn', type: 'submit', disabled: busy, 'data-fk': 'first-add' }, busy ? 'Adding…' : 'Add'),
  );
}

/** A mint's stamp, as a link named it, and what the person can do with it. */
function linkStamp(
  input: FirstInput,
  link: { mint: string; server: string },
  about: About,
  held: MintRecord | undefined,
): Child[] {
  const { state, actions } = input;
  const name = held?.name ?? (input.playground?.mint === link.mint ? 'Playground' : hostOf(link.server));
  const faucet = about.faucet;
  const buttons: Child[] = [];
  const home = h('a', { class: 'btn solid wide', href: '#home', 'data-fk': 'first-home' }, 'Open the purse');
  if (held !== undefined && held.server !== link.server) {
    // the purse never moves a mint it holds to another server on a link's word
    buttons.push(
      h(
        'p',
        { class: 'note-line' },
        `This purse holds this mint at ${hostOf(held.server)} already, and keeps it there. The link names ${hostOf(link.server)}.`,
      ),
      home,
    );
  } else if (held !== undefined && faucet === undefined) {
    buttons.push(home);
  } else if (faucet !== undefined) {
    buttons.push(takeButton(state, Number(faucet.amount), actions.take));
    if (held === undefined) {
      buttons.push(
        h(
          'button',
          { class: 'btn wide', type: 'button', disabled: state.busy !== undefined, 'data-fk': 'first-addmint', on: { click: actions.add } },
          state.busy === 'add' ? 'Adding…' : 'Add mint',
        ),
      );
    }
  } else {
    buttons.push(
      h(
        'button',
        { class: 'btn solid wide', type: 'button', disabled: state.busy !== undefined, 'data-fk': 'first-addmint', on: { click: actions.add } },
        state.busy === 'add' ? 'Adding…' : 'Add mint',
      ),
    );
  }
  return [stamp(link.mint, name, about, false), low(buttons, input)];
}

/** The build's playground as a stamp: Start with the Playground, or add another mint by link. */
function playgroundStamp(input: FirstInput, playground: { mint: string; server: string }, about: About): Child[] {
  const { state, actions } = input;
  const faucet = about.faucet;
  const start = h(
    'button',
    {
      class: 'btn solid wide',
      type: 'button',
      'data-fk': 'first-start',
      disabled: state.busy !== undefined,
      on: { click: faucet === undefined ? actions.add : actions.take },
    },
    state.busy === undefined
      ? 'Start with the Playground'
      : faucet === undefined
        ? 'Adding…'
        : `Taking ${faucet.amount}…`,
  );
  const other = state.pasting
    ? pasteRow(input)
    : h('button', { class: 'btn wide', type: 'button', 'data-fk': 'first-addlink', on: { click: actions.openPaste } }, 'Add a mint by link');
  return [stamp(playground.mint, 'Playground', about, true), low([start, other], input)];
}

/** The solid faucet button. */
function takeButton(state: FirstState, amount: number, take: () => void): HTMLElement {
  return h(
    'button',
    { class: 'btn solid wide', type: 'button', disabled: state.busy !== undefined, 'data-fk': 'first-take', on: { click: take } },
    state.busy === 'take' ? `Taking ${amount}…` : `Take ${amount} from the faucet`,
  );
}

/** The actions, the error under them and the two notes, pinned low in the band. */
function low(buttons: Child[], input: FirstInput): HTMLElement {
  return h('div', { class: 'c-low first-acts' }, ...buttons, errorLine(input.state.error), notes(input.standalone));
}

const errorLine = (error: string | undefined): HTMLElement | null =>
  error === undefined ? null : h('p', { class: 'err', role: 'alert' }, error);

/** The Home Screen hint, unless the purse is one already, and what clears the purse. */
function notes(standalone: boolean): HTMLElement {
  return h(
    'div',
    { class: 'first-notes' },
    standalone
      ? null
      : h('p', { class: 'note-line muted' }, 'On iPhone or iPad, add this purse to your Home Screen so Safari keeps it.'),
    h(
      'p',
      { class: 'note-line muted' },
      'Clearing this site’s data empties the purse, and a private window forgets it when it closes.',
    ),
  );
}

/**
 * A mint's stamp. With a faucet: its name and face, the tap as squares and
 * as a denomination, and the faucet's limit. Without: its face large, and
 * its rotation.
 */
function stamp(key: string, name: string, about: About, playground: boolean): HTMLElement {
  const faucet = about.faucet;
  const rotation = about.info.paused ? undefined : about.info.rotation;
  const schedule = about.info.paused
    ? 'Paused'
    : rotation === undefined
      ? 'By hand'
      : `Every ${periodWords(Number(rotation.periodSecs))}`;
  if (faucet === undefined) {
    return h(
      'div',
      { class: 'stamp mstamp c-mid', role: 'group', 'aria-label': `The ${name} mint, key ${shortKey(key)}` },
      h('div', { class: 'who' }, face(key, 3.6, 2.8), h('span', { class: 'lab' }, name)),
      h('div', { class: 'bits' }, face(key, 7, 5.6)),
      h(
        'div',
        { class: 'foot' },
        keyLabel(key),
        h('span', { class: 'lab' }, `${schedule} · ${about.info.layers} layers`),
      ),
    );
  }
  const amount = Number(faucet.amount);
  const perHour = faucet.perAddressPerHour;
  const taps = `${perHour} ${perHour === 1 ? 'tap' : 'taps'} an hour`;
  return h(
    'div',
    {
      class: 'stamp mstamp c-mid',
      role: 'group',
      'aria-label': `The ${name} mint: ${amount} ${amount === 1 ? 'token' : 'tokens'} a tap, ${perHour} ${perHour === 1 ? 'tap' : 'taps'} an hour`,
    },
    h('div', { class: 'who' }, face(key, 3.6, 2.8), h('span', { class: 'lab' }, name)),
    h('div', { class: 'bits' }, bits(amount)),
    h('p', { class: 'denom' }, String(amount), h('i', {}, 'a tap')),
    h(
      'div',
      { class: 'foot' },
      playground ? h('span', { class: 'lab' }, 'A mint to practise on') : keyLabel(key),
      h('span', { class: 'lab' }, taps),
    ),
  );
}

/** `KEY c5a519ee`: the label in capitals, the key's digits as Holdings writes them, so the two can be compared. */
const keyLabel = (key: string): HTMLElement => h('span', { class: 'lab' }, 'Key ', h('span', { class: 'hex' }, shortKey(key)));
