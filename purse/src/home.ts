// Holdings (#home): the total and the change, the one sentence, the field
// and its caption, the date to come back by, Recent and the mint. One tree
// for every width; the CSS shows the phone's or the desktop's parts. The
// field itself is drawn into `#field` once the view is in the page (see
// field.ts), since it fills the height the page leaves it.

import type { FaucetInfo } from 'worgl';

import { type Child, h } from './dom.ts';
import { recentRow } from './flow-words.ts';
import { f2, hm, periodWords, shortDay, change } from './format.ts';
import type { Holdings, LaneKey } from './holdings.ts';
import { shortKey } from './link.ts';
import { face } from './marks.ts';
import { postmarkWrap } from './postmark.ts';
import type { LogEntry, MintRecord } from './records.ts';
import { brand, deskbar } from './shell.ts';
import { type Received, KEEPS_ALIVE, caption, comeBackHeading, comeBackRow, fieldLabel, firstToGo, numberDescription, sentence } from './words.ts';

/** What the purse last read of the mint it shows. */
export interface HomeData {
  holdings: Holdings;
  /** The faucet at the mint's server, when it answers for this mint. */
  faucet: FaucetInfo | undefined;
}

/** How long the change chip stays, and how long it and a new entry are pink. */
const CHIP_MS = 60 * 60_000;
const PINK_MS = 6000;

/** Everything Holdings draws from. */
export interface HomeInput {
  record: MintRecord;
  /** What the purse read; `undefined` until the first read comes back. */
  data: HomeData | undefined;
  /** This mint's log entries, newest first. */
  log: readonly LogEntry[];
  /** What went wrong reading the mint, or taking from its faucet, in plain words. */
  error: { words: string; at: 'read' | 'take' } | undefined;
  /** The lane whose tip is open, for its numbers' state. */
  openLane: LaneKey | undefined;
  /** The last change and when it came, for the chip. */
  delta: { v: number; t: number } | undefined;
  /** Whether the purse is taking from the faucet now. */
  taking: boolean;
  /** Tokens just taken in from a hand-back, while Holdings shows them: the postmark and the sentence's clause. */
  received: { amount: number; t: number; lane: LaneKey | undefined } | undefined;
  /** The requests whose hand-back this purse still keeps, by id: their Sent rows open it again. */
  kept: ReadonlySet<string>;
  now: number;
  take: () => void;
  toggleLane: (lane: LaneKey) => void;
  /** A Recent row that opens something on Send. */
  openRow: (entry: LogEntry) => void;
}

/** Holdings. */
export function homeView(input: HomeInput): HTMLElement {
  return h(
    'div',
    { class: 'view v-home' },
    deskbar(true, 'home'),
    h('main', { class: 'vmain', 'aria-busy': input.data === undefined && input.error === undefined ? 'true' : undefined }, paperBand(input), cobaltBand(input)),
  );
}

/** Before the first read: the mint's lanes, empty, so the page keeps its shape. */
export function placeholder(record: MintRecord): Holdings {
  return {
    layers: record.info?.layers ?? 4,
    notes: [],
    total: 0,
    schedule: undefined,
    paused: record.info?.paused ?? false,
    firstExpiry: undefined,
  };
}

function paperBand(input: HomeInput): HTMLElement {
  const { record, data, now } = input;
  const holdings = data?.holdings ?? placeholder(record);
  const known = data !== undefined;
  return h(
    'section',
    { class: 'band-p' },
    h(
      'div',
      { class: 'wrap' },
      h('header', { class: 'tb' }, brand(true)),
      h(
        'div',
        { class: `hero${input.received !== undefined ? ' has-pm' : ''}` },
        h(
          'div',
          { class: 'hero-l' },
          h('p', { class: 'lab muted eyebrow' }, `${record.name} · in this browser`),
          h(
            'h1',
            // a long number shrinks to fit its column (styles.css)
            { class: 'total', tabindex: -1, 'data-fk': 'h1', style: { '--digits': String(Math.max(3, String(holdings.total).length)) } },
            // until the read comes back the number is not known: a blank keeps the line's height
            h('b', {}, known ? String(holdings.total) : ' '),
            known ? h('i', {}, holdings.total === 1 ? 'token' : 'tokens') : null,
            chip(input.delta, now),
          ),
          input.received === undefined ? null : postmarkWrap(input.received.amount, input.received.t, now),
        ),
        h('div', { class: 'hero-r' }, h('p', { class: 'sentence', id: 'sentence' }, ...(known ? words(input, holdings) : []))),
      ),
      h(
        'div',
        { class: 'field-wrap' },
        h('div', {
          class: 'field',
          id: 'field',
          role: 'group',
          'aria-label': fieldLabel(holdings.layers),
          'aria-describedby': 'sentence',
        }),
      ),
      h('div', { class: 'tip-slot phone-only' }),
      input.error?.at === 'read' ? h('p', { class: 'err home-err', role: 'alert' }, input.error.words) : null,
      h(
        'div',
        { class: 'f-cap desk-only' },
        h('span', { class: 'lab' }, caption(holdings.notes.length)),
        h(
          'span',
          { class: 'legend lab' },
          h('span', {}, h('i', { class: 'lg-r' }), 'Ready'),
          h('span', {}, h('i', { class: 'lg-s' }), 'Resting'),
          h('span', {}, h('i', { class: 'lg-f' }), 'Fading'),
        ),
      ),
      h('div', { class: 'back-row phone-only' }, known ? backRow(holdings, now) : h('p', {})),
    ),
  );
}

/** The change chip: for an hour, pink for its first six seconds, continued across redraws. */
function chip(delta: HomeInput['delta'], now: number): HTMLElement | null {
  if (delta === undefined || now - delta.t >= CHIP_MS) {
    return null;
  }
  const age = now - delta.t;
  return h(
    'span',
    { class: `chip${age < PINK_MS ? ' live' : ''}`, style: { animationDelay: `${f2(-age / 1000)}s` } },
    change(delta.v),
  );
}

/** The sentence, its numbers as buttons that open their lane's tip. */
function words(input: HomeInput, holdings: Holdings): Child[] {
  const lane = input.received?.lane;
  const received: Received | undefined = input.received === undefined || lane === undefined ? undefined : { amount: input.received.amount, lane };
  return sentence(holdings, input.now, KEEPS_ALIVE, received).map((part) => {
    if (typeof part === 'string') {
      return part;
    }
    if ('em' in part) {
      return h('em', {}, part.em);
    }
    const open = input.openLane === part.lane;
    return h(
      'button',
      {
        type: 'button',
        class: 'num',
        'data-lane': part.lane,
        'data-fk': `num-${part.lane}`,
        'aria-expanded': open ? 'true' : 'false',
        'aria-controls': open ? 'field-tip' : undefined,
        'aria-label': numberDescription(part.n, part.lane),
        on: { click: () => input.toggleLane(part.lane) },
      },
      String(part.n),
    );
  });
}

/** The phone's come-back row: a heading, as the desktop's column is, when it names a date. */
function backRow(holdings: Holdings, now: number): HTMLElement {
  const row = comeBackRow(holdings, now);
  return 'none' in row ? h('p', { class: 'muted' }, row.none) : h('h2', {}, row.lead, h('b', {}, row.date));
}

function cobaltBand(input: HomeInput): HTMLElement {
  // empty, not between a send and its change: only then is the faucet the way on
  const empty = input.data !== undefined && input.data.holdings.notes.length === 0 && input.data.holdings.total === 0;
  const faucet = empty ? input.data?.faucet : undefined;
  return h(
    'section',
    { class: 'band-c' },
    h(
      'div',
      { class: 'wrap' },
      h(
        'div',
        { class: 'home-cols' },
        h('div', { class: 'col-back desk-only' }, ...backColumn(input, faucet)),
        h(
          'div',
          { class: 'col-recent' },
          h('div', { class: 'recent-h col-h' }, h('h2', { class: 'lab' }, 'Recent')),
          h('div', { class: 'phone-only recent-p' }, ...recent(input, 3, 'p')),
          h('div', { class: 'desk-only' }, ...recent(input, 4, 'd')),
        ),
        mintColumn(input),
      ),
      h(
        'div',
        { class: 'home-acts phone-only' },
        faucet === undefined ? null : takeButton(input, faucet, 'home-take', true),
        faucet === undefined ? null : takeError(input),
        h(
          'div',
          { class: 'btn-row' },
          h('a', { class: 'btn', href: '#receive' }, 'Receive'),
          // on an empty purse the faucet is the one way on: Send would only say the purse is empty
          h('a', { class: `btn${faucet === undefined ? ' solid' : ''}`, href: '#send' }, 'Send'),
        ),
      ),
    ),
  );
}

/** The desktop's come-back column. */
function backColumn(input: HomeInput, faucet: FaucetInfo | undefined): Child[] {
  const head = h('div', { class: 'col-h' }, h('h2', { class: 'lab' }, comeBackHeading()));
  if (input.data === undefined) {
    return [head];
  }
  const holdings = input.data.holdings;
  const row = comeBackRow(holdings, input.now);
  if ('none' in row) {
    return [
      head,
      h('p', { class: 'note-line' }, row.none),
      faucet === undefined ? null : h('div', { class: 'cb-act' }, takeButton(input, faucet, 'home-take-d', false)),
      faucet === undefined ? null : takeError(input),
    ];
  }
  return [head, h('p', { class: 'cb-date' }, row.date), h('p', { class: 'note-line' }, firstToGo(holdings, input.now))];
}

/** The faucet button of an empty purse. */
function takeButton(input: HomeInput, faucet: FaucetInfo, key: string, wide: boolean): HTMLElement {
  return h(
    'button',
    {
      class: `btn solid${wide ? ' wide' : ''}`,
      type: 'button',
      disabled: input.taking,
      'data-fk': key,
      on: { click: input.take },
    },
    input.taking ? `Taking ${faucet.amount}…` : `Take ${faucet.amount} from the faucet`,
  );
}

const takeError = (input: HomeInput): HTMLElement | null =>
  input.error?.at === 'take' ? h('p', { class: 'err home-err', role: 'alert' }, input.error.words) : null;

/**
 * The last `n` entries of the purse's log, or that there are none yet. A
 * send's row whose hand-back or send can still be shown is a button that
 * shows it on Send; `where` keeps the phone's and the desktop's apart.
 */
function recent(input: HomeInput, n: number, where: 'p' | 'd'): HTMLElement[] {
  const { log, now } = input;
  if (log.length === 0) {
    return [h('p', { class: 'note-line muted recent-none' }, 'Nothing yet.')];
  }
  return log.slice(0, n).map((entry) => {
    const age = now - entry.t;
    const row = recentRow(entry, entry.ask !== undefined && input.kept.has(entry.ask), now);
    const parts: Child[] = [
      h('time', { datetime: new Date(entry.t).toISOString() }, shortDay(entry.t, now)),
      h('span', {}, age >= 0 && age < PINK_MS ? newDot(age) : null, row.title, row.small === undefined ? null : h('small', {}, row.small)),
      row.delta === undefined || row.delta === 0 ? h('b', { class: 'n', 'aria-label': 'no change' }, '—') : h('b', {}, change(row.delta)),
    ];
    if (row.opens === undefined) {
      return h('div', { class: 'act' }, ...parts);
    }
    return h(
      'button',
      {
        class: 'act go',
        type: 'button',
        'aria-label': row.label,
        'data-fk': `act-${where}${entry.id ?? entry.t}`,
        on: { click: () => input.openRow(entry) },
      },
      ...parts,
    );
  });
}

/**
 * The pink square of an entry `age` ms old, which decays and collapses. It
 * is taken out once done: a row hidden on a short screen and shown again
 * would start its animation over, pink on an entry that is no longer new.
 */
function newDot(age: number): HTMLElement {
  const dot = h('i', { class: 'new', 'aria-hidden': 'true', style: { animationDelay: `${f2(-age / 1000)}s` } });
  setTimeout(() => dot.remove(), PINK_MS - age);
  return dot;
}

/** A host name that may break after each dot, so a long one wraps in its column rather than running out of it. */
function breakable(name: string): Child[] {
  return name.split('.').flatMap((part, i, parts): Child[] => (i < parts.length - 1 ? [`${part}.`, h('wbr')] : [part]));
}

/** The desktop's mint column: its face, name, rotation and key. */
function mintColumn(input: HomeInput): HTMLElement {
  const { record, data } = input;
  const holdings = data?.holdings;
  const layers = holdings?.layers ?? record.info?.layers;
  const paused = holdings?.paused ?? record.info?.paused ?? false;
  const schedule = holdings?.schedule;
  const period = schedule === undefined ? record.info?.periodSecs : schedule.periodMs / 1000;
  const of = layers === undefined ? '' : ` · ${layers} layers`;
  let rotates: Child[];
  if (paused) {
    rotates = [`Paused${of}`];
  } else if (period === undefined) {
    rotates = [holdings === undefined ? of.replace(/^ · /, '') : `Rotates by hand${of}`];
  } else {
    rotates = [
      h('span', { class: 'm-long' }, 'Rotates every'),
      h('span', { class: 'm-short' }, 'Every'),
      ` ${periodWords(period)}${of}${schedule === undefined ? '' : ` · next ${hm(schedule.nextMs)}`}`,
    ];
  }
  return h(
    'div',
    { class: 'col-mint desk-only' },
    h('div', { class: 'col-h' }, h('h2', { class: 'lab' }, 'Mint')),
    h(
      'div',
      { class: 'mintc' },
      h(
        'div',
        { class: 'mrow' },
        face(record.mint, 7, 5.6),
        h('div', {}, h('h3', {}, ...breakable(record.name)), h('p', { class: 'note-line muted' }, ...rotates)),
      ),
      h('dl', {}, h('dt', {}, 'Key'), h('dd', {}, h('code', {}, shortKey(record.mint)))),
    ),
  );
}
