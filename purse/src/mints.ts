// Mints (#mints): every mint the purse holds, each as a stamp: its face,
// its name and key, what is held of it, its next rotations, its faucet and
// its whole key; which one Holdings shows, and a name the person gives it.
// Under them a mint is added by its link, or by its server and key: read
// from its server and shown, face and whole key, before it is kept. The
// words are mint-list.ts's; this builds them into elements.

import type { FaucetInfo } from 'worgl';

import { type Child, h } from './dom.ts';
import { stamp } from './first.ts';
import { hm, weekday } from './format.ts';
import type { Holdings } from './holdings.ts';
import { type MintLink, hostOf, shortKey } from './link.ts';
import { face } from './marks.ts';
import { clockWords, faucetRow, keyPairs, rotatesRow, timetable } from './mint-list.ts';
import type { About } from './purses.ts';
import type { MintRecord } from './records.ts';
import { deskbar, tbSub } from './shell.ts';

/** What Mints shows and what it is doing. */
export interface MintsState {
  /** What the purse holds of each mint, by mint, as Mints last read it. */
  read: Map<string, Holdings>;
  /** Each mint's faucet once its server answered: `null` where it has none for the mint. */
  faucets: Map<string, FaucetInfo | null>;
  /** Why the last read of a mint failed, by mint. */
  errors: Map<string, string>;
  /** The mint whose name is being typed, and what is typed. */
  naming: string | undefined;
  name: string;
  nameError: string | undefined;
  /** Whether the name typed is being kept. */
  renaming: boolean;
  /** The mint taken from now, and what the last take came to, or why it failed. */
  taking: string | undefined;
  took: { mint: string; words: string; error: boolean } | undefined;
  /** How a mint is added, and what was typed for it. */
  tab: 'link' | 'key';
  link: string;
  server: string;
  key: string;
  /** Whether the mint typed is being read from its server. */
  reading: boolean;
  /** The mint read, with the name it would start with: shown before it is kept. */
  found: { link: MintLink; about: About; name: string } | undefined;
  /** A mint typed that the purse holds already, and what to say of it. */
  held: { mint: string; words: string } | undefined;
  /** What is wrong with what was typed, or why the read or the add failed. */
  addError: string | undefined;
  adding: boolean;
  /** What the last add came to. */
  added: string | undefined;
  /** The error words drawn last time: drawn again, they are not an alert again, so a screen reader says each once. */
  alerted: ReadonlySet<string>;
}

export const blankMints = (): MintsState => ({
  read: new Map(),
  faucets: new Map(),
  errors: new Map(),
  naming: undefined,
  name: '',
  nameError: undefined,
  renaming: false,
  taking: undefined,
  took: undefined,
  tab: 'link',
  link: '',
  server: '',
  key: '',
  reading: false,
  found: undefined,
  held: undefined,
  addError: undefined,
  adding: false,
  added: undefined,
  alerted: new Set(),
});

/** What Mints calls when the person acts. */
export interface MintsActions {
  /** Shows the mint on Holdings: it becomes the one used last. */
  show: (mint: string) => void;
  take: (mint: string) => void;
  /** Opens the field for the mint's name. */
  rename: (mint: string) => void;
  nameChanged: (text: string) => void;
  saveName: () => void;
  cancelName: () => void;
  tab: (tab: 'link' | 'key') => void;
  typed: (field: 'link' | 'server' | 'key', text: string) => void;
  /** Reads the mint typed from its server, to show it before it is kept. */
  find: () => void;
  add: () => void;
  cancelAdd: () => void;
}

/** Everything Mints draws from. */
export interface MintsInput {
  state: MintsState;
  /** The mints, in the order they were added. */
  records: readonly MintRecord[];
  /** The mint Holdings shows. */
  shown: string | undefined;
  /** Whether a mint is the build's playground: its stamp says so. */
  playground: (link: MintLink) => boolean;
  actions: MintsActions;
}

/** What went wrong, as an alert when it is new, and as a plain line when it is only drawn again. */
const err = (state: MintsState, words: string, cls = 'err'): HTMLElement =>
  h('p', { class: cls, role: state.alerted.has(words) ? undefined : 'alert' }, words);

/** A key's first eight digits, as Holdings writes them, after its label. */
const keyLab = (key: string): HTMLElement => h('p', { class: 'lab muted' }, 'Key ', h('span', { class: 'hex' }, shortKey(key)));

/** A whole key, in pairs of groups that never break, to compare with the one a mint's people give. */
const wholeKey = (key: string): HTMLElement =>
  h('code', { class: 'mc-key' }, ...keyPairs(key).flatMap((pair, i) => [i === 0 ? '' : ' ', h('span', {}, pair)]));

/** Mints. */
export function mintsView(input: MintsInput): HTMLElement {
  const several = input.records.length > 1;
  return h(
    'div',
    { class: 'view v-mints v-sub flow' },
    deskbar(true, 'mints'),
    h(
      'main',
      { class: 'vmain' },
      h(
        'section',
        { class: 'band-p' },
        h(
          'div',
          { class: 'wrap' },
          tbSub('Mints'),
          h('h1', { class: 'h1 mints-h1', tabindex: -1, 'data-fk': 'h1' }, 'Mints in this ', h('em', {}, 'purse'), '.'),
          h(
            'p',
            { class: 'body mt-12 mints-lede' },
            several
              ? 'Each mint is one kind of token, with a clock of its own. Holdings shows one at a time: the one you used last.'
              : 'A mint is one kind of token, with a clock of its own.',
          ),
          h('div', { class: 'mcards' }, ...input.records.map((record) => card(input, record, several))),
        ),
      ),
      h('section', { class: 'band-c' }, h('div', { class: 'wrap' }, h('div', { class: 'c-mid mints-add' }, ...addPart(input)))),
    ),
  );
}

/** One mint, as a stamp on the page. */
function card(input: MintsInput, record: MintRecord, several: boolean): HTMLElement {
  const { state, actions } = input;
  const { mint } = record;
  const holdings = state.read.get(mint);
  const error = state.errors.get(mint);
  const faucet = state.faucets.get(mint) ?? undefined;
  const layers = holdings?.layers ?? record.info?.layers;
  const paused = holdings?.paused ?? record.info?.paused ?? false;
  const periodSecs = holdings === undefined ? record.info?.periodSecs : holdings.schedule === undefined ? undefined : holdings.schedule.periodMs / 1000;
  const known = holdings !== undefined || record.info !== undefined;
  const times = timetable(holdings?.schedule);
  const isShown = several && input.shown === mint;
  const took = state.took?.mint === mint ? state.took : undefined;
  const id = `mc-${mint}`;
  const total = holdings?.total;
  // with one mint there is nothing to choose for Holdings, so while it is renamed its foot is empty
  const footless = !several && state.naming === mint;
  const rows: Child[] = [h('dt', {}, 'Server'), h('dd', {}, hostOf(record.server))];
  if (faucet !== undefined) {
    const amount = Number(faucet.amount);
    const taking = state.taking === mint;
    rows.push(
      h('dt', { class: 'mc-ft' }, 'Faucet'),
      h(
        'dd',
        { class: 'mc-faucet' },
        h('span', {}, faucetRow(amount, faucet.perAddressPerHour)),
        h(
          'button',
          {
            class: 'btn sm solid',
            type: 'button',
            disabled: state.taking !== undefined,
            'data-fk': `mc-take-${mint}`,
            'aria-label': taking ? undefined : `Take ${amount} from the ${record.name} faucet`,
            on: { click: () => actions.take(mint) },
          },
          taking ? `Taking ${amount}…` : `Take ${amount}`,
        ),
      ),
    );
  }
  return h(
    'article',
    { class: `mcard${isShown ? ' on-home' : ''}${footless ? ' footless' : ''}`, 'aria-labelledby': id },
    h(
      'div',
      { class: 'mc-top' },
      face(mint, 6, 4.8),
      h('div', { class: 'mc-who' }, h('h2', { class: 'mc-name', id, tabindex: -1, 'data-fk': id }, record.name), keyLab(mint)),
      h(
        'p',
        { class: 'mc-held' },
        h('b', {}, total === undefined ? ' ' : String(total)),
        h('small', { class: 'lab' }, total !== undefined ? 'held' : error === undefined ? 'reading' : 'not read'),
      ),
    ),
    state.naming === mint ? nameForm(input) : null,
    layers === undefined
      ? null
      : h(
          'p',
          { class: 'tt-h' },
          h('span', { class: 'lab' }, times.length === 0 ? 'Rotations' : 'Next rotations'),
          ' ',
          h('span', { class: 'lab muted' }, rotatesRow(layers, periodSecs, paused)),
        ),
    times.length === 0
      ? null
      : h(
          'ol',
          { class: 'tt', 'aria-label': 'Next rotations' },
          ...times.map((t, i) =>
            h(
              'li',
              { class: i === 0 ? 'next' : undefined },
              h('time', { datetime: new Date(t).toISOString() }, h('b', {}, hm(t)), ' ', h('span', { class: 'lab' }, weekday(t))),
            ),
          ),
        ),
    known ? h('p', { class: 'note-line muted mc-clock' }, clockWords(periodSecs, paused)) : null,
    h('dl', {}, ...rows),
    h('div', { class: 'mc-whole' }, h('p', { class: 'lab muted' }, 'Whole key'), wholeKey(mint)),
    error === undefined ? null : err(state, error, 'err mc-err'),
    took === undefined ? null : took.error ? err(state, took.words, 'err mc-err') : h('p', { class: 'result mc-took', role: 'status' }, took.words),
    footless
      ? null
      : h(
          'div',
          { class: 'mc-foot' },
          !several
            ? null
            : isShown
              ? h('span', { class: 'mc-tag lab' }, 'On Holdings')
              : h(
                  'button',
                  { class: 'btn sm', type: 'button', 'data-fk': `mc-show-${mint}`, 'aria-label': `Show ${record.name} on Holdings`, on: { click: () => actions.show(mint) } },
                  'Show on Holdings',
                ),
          state.naming === mint
            ? null
            : h(
                'button',
                { class: 'tbtn mc-rn', type: 'button', 'data-fk': `mc-rename-${mint}`, 'aria-label': `Rename ${record.name}`, on: { click: () => actions.rename(mint) } },
                'Rename',
              ),
        ),
  );
}

/** The field for a mint's name, under its name. */
function nameForm(input: MintsInput): HTMLElement {
  const { state, actions } = input;
  return h(
    'form',
    {
      class: 'form mc-rename',
      'data-form': 'rename',
      novalidate: true,
      on: {
        submit: (event) => {
          event.preventDefault();
          actions.saveName();
        },
      },
    },
    h(
      'div',
      { class: 'fld' },
      h('label', { class: 'lab', for: 'mc-name' }, 'Name'),
      h('input', {
        class: 'inp',
        id: 'mc-name',
        'data-fk': 'mc-name',
        value: state.name,
        autocomplete: 'off',
        spellcheck: 'false',
        on: { input: (event) => actions.nameChanged((event.target as HTMLInputElement).value) },
      }),
    ),
    state.nameError === undefined ? null : err(state, state.nameError),
    h(
      'div',
      { class: 'btn-row' },
      h('button', { class: 'btn sm solid', type: 'submit', disabled: state.renaming, 'data-fk': 'mc-save' }, state.renaming ? 'Saving…' : 'Save'),
      h('button', { class: 'btn sm', type: 'button', disabled: state.renaming, 'data-fk': 'mc-cancel', on: { click: actions.cancelName } }, 'Cancel'),
    ),
  );
}

/** Adding a mint: the two ways in, then the mint read, to compare before it is kept. */
function addPart(input: MintsInput): Child[] {
  const { state, actions } = input;
  const lead = h('p', { class: 'lab muted' }, 'Add a mint');
  const found = state.found;
  if (found !== undefined) {
    const { link, about, name } = found;
    return [
      lead,
      h('div', { class: 'mc-found' }, stamp(link.mint, name, about, input.playground(link))),
      h('div', { class: 'mc-whole' }, h('p', { class: 'lab muted' }, 'Its whole key'), wholeKey(link.mint)),
      h(
        'p',
        { class: 'note-line mt-10' },
        `Read from ${hostOf(link.server)}. Compare its face and key with the ones its people give you: another key is another mint.`,
      ),
      state.addError === undefined ? null : err(state, state.addError, 'err mt-10'),
      h(
        'div',
        { class: 'btn-row mt-14' },
        h('button', { class: 'btn solid', type: 'button', disabled: state.adding, 'data-fk': 'm-add', on: { click: actions.add } }, state.adding ? 'Adding…' : 'Add mint'),
        h('button', { class: 'btn', type: 'button', disabled: state.adding, 'data-fk': 'm-cancel', on: { click: actions.cancelAdd } }, 'Cancel'),
      ),
    ];
  }
  const tab = (which: 'link' | 'key', words: string): HTMLElement =>
    h(
      'button',
      { type: 'button', 'data-fk': `m-tab-${which}`, 'aria-pressed': state.tab === which ? 'true' : 'false', on: { click: () => actions.tab(which) } },
      words,
    );
  const field = (id: string, label: string, which: 'link' | 'server' | 'key', value: string, placeholder: string, mono: boolean): HTMLElement =>
    h(
      'div',
      { class: 'fld' },
      h('label', { class: 'lab', for: id }, label),
      h('input', {
        class: `inp${mono ? ' mono' : ''}`,
        id,
        'data-fk': id,
        value,
        placeholder,
        autocomplete: 'off',
        autocapitalize: 'off',
        spellcheck: 'false',
        inputmode: which === 'server' ? 'url' : undefined,
        on: { input: (event) => actions.typed(which, (event.target as HTMLInputElement).value) },
      }),
    );
  return [
    lead,
    h('div', { class: 'tabs', role: 'group', 'aria-label': 'How to add a mint' }, tab('link', 'By link'), tab('key', 'By server and key')),
    h(
      'form',
      {
        class: 'form',
        'data-form': 'mint',
        novalidate: true,
        on: {
          submit: (event) => {
            event.preventDefault();
            actions.find();
          },
        },
      },
      ...(state.tab === 'link'
        ? [field('m-link', 'Mint link', 'link', state.link, 'Paste the mint’s link', true)]
        : [
            field('m-server', 'Server', 'server', state.server, 'https://mint.example.org', true),
            field('m-key', 'Key · 64 hex digits', 'key', state.key, '3b0c9e71…', true),
          ]),
      state.addError === undefined ? null : err(state, state.addError),
      h('button', { class: 'btn solid wide', type: 'submit', disabled: state.reading, 'data-fk': 'm-find' }, state.reading ? 'Reading the mint…' : 'Find the mint'),
    ),
    state.held === undefined
      ? null
      : h(
          'div',
          { class: 'mc-held-note' },
          h('p', { class: 'result', role: 'status' }, state.held.words),
          input.shown === state.held.mint
            ? null
            : h(
                'button',
                { class: 'btn', type: 'button', 'data-fk': 'm-show-held', on: { click: () => actions.show(state.held?.mint ?? '') } },
                'Show it on Holdings',
              ),
        ),
    state.added === undefined ? null : h('p', { class: 'result mt-14', role: 'status' }, state.added),
    h('p', { class: 'note-line muted mt-14' }, 'The key is the mint’s real name. Its face is drawn from the key, so you can compare it with theirs.'),
  ];
}
