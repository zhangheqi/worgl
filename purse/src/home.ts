// Holdings (#home): the total and the change, the one sentence, the field
// and its caption, the date to come back by, Recent and the mint. One tree
// for every width; the CSS shows the phone's or the desktop's parts. The
// field itself is drawn into `#field` once the view is in the page (see
// field.ts), since it fills the height the page leaves it. While a
// keep-alive run goes, the come-back row and column count its transfers;
// `renderBack` redraws only those two as the count moves, so focus, an open
// tip and the field are left alone. The date to come back by is the
// earliest of every mint the purse keeps, and says so when it is another's.

import type { FaucetInfo } from 'worgl';

import { type Child, h } from './dom.ts';
import { recentRow } from './flow-words.ts';
import { f2, hm, periodWords, shortDay, change } from './format.ts';
import { type Cover, coverOf, coverRow, wantsBackup } from './cover.ts';
import { arrowIcon } from './icons.ts';
import type { Holdings, LaneKey } from './holdings.ts';
import { type Progress, boxes } from './keep.ts';
import { shortKey } from './link.ts';
import { face } from './marks.ts';
import { postmarkWrap } from './postmark.ts';
import type { LogEntry, MintRecord } from './records.ts';
import { brand, deskbar } from './shell.ts';
import {
  KEEPING_ELSEWHERE,
  KEEPING_ELSEWHERE_NOTE,
  type Received,
  caption,
  comeBackHeading,
  comeBackRow,
  elsewhereNote,
  elsewhereRow,
  fieldLabel,
  firstToGo,
  keepingCount,
  keepingLead,
  keepingNote,
  numberDescription,
  sentence,
} from './words.ts';

/** What the purse last read of the mint it shows. */
export interface HomeData {
  holdings: Holdings;
  /** The faucet at the mint's server, when it answers for this mint. */
  faucet: FaucetInfo | undefined;
}

/** How long the change chip stays, and how long it and a new entry are pink. */
const CHIP_MS = 60 * 60_000;
/** How long what just changed stays pink: Recent's and History's dot, and the chip. */
export const PINK_MS = 6000;

/** A keep-alive run of the shown mint's tokens, as the come-back row and column count it. */
export interface KeepShown extends Progress {
  /** Whether its hops go a few seconds apart, or a near rotation squeezed them. */
  spaced: boolean;
  /** Whether another tab of the purse runs it, and this one only shows it. */
  watching: boolean;
}

/** Another mint the purse keeps, whose tokens go before the shown mint's: the date to come back by is for them. */
export interface Elsewhere {
  /** What the purse calls it. */
  name: string;
  holdings: Holdings;
  /** Whether the purse can keep its tokens alive now. */
  keeps: boolean;
}

/** At most this many step boxes: eight on the phone (two rows at most), sixteen in the desktop column. */
const PHONE_BOXES = 8;
const DESK_BOXES = 16;

/** Everything Holdings draws from. */
export interface HomeInput {
  record: MintRecord;
  /** What the purse read; `undefined` until the first read comes back. */
  data: HomeData | undefined;
  /** Before any read came back, when the read failed: what fades first as the purse saved it, for the come-back row and column. */
  saved: Holdings | undefined;
  /** This mint's log entries, newest first. */
  log: readonly LogEntry[];
  /** What went wrong reading the mint, taking from its faucet, or keeping its tokens alive, in plain words. */
  error: { words: string; at: 'read' | 'take' | 'keep' } | undefined;
  /** Whether the error is new since the last draw: only then is it an alert, so a screen reader says it once, not at each redraw. */
  alert: boolean;
  /** Whether the purse can keep these tokens alive now: false while the mint is paused or keep-alive is stuck (the words then say what happens without it). */
  keeps: boolean;
  /** The keep-alive run going now, if one moves the shown mint's tokens. */
  keep: KeepShown | undefined;
  /** Another mint whose tokens go first, when there is one: the come-back row and column are about it. */
  elsewhere: Elsewhere | undefined;
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
      h('header', { class: 'tb' }, brand(true), h('span', { class: 'sp' }), backupLink(input)),
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
      input.error?.at === 'read' || input.error?.at === 'keep' ? errorLine(input, input.error.words) : null,
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
      h('div', { class: 'back-row phone-only' }, ...backRow(input)),
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
  return sentence(holdings, input.now, input.keeps, received).map((part) => {
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

/**
 * The phone's come-back row: a heading, as the desktop's column is, when it
 * names a date; while a run goes, its count and step boxes, a paragraph
 * since it reports progress.
 */
function backRow(input: HomeInput): HTMLElement[] {
  const { data, keep, now } = input;
  if (keep !== undefined) {
    return [
      h(
        'p',
        {},
        h('b', {}, keepingLead(keep.amount)),
        ` · ${keepingCount(keep)}`,
        keep.watching ? h('br') : null,
        keep.watching ? h('span', { class: 'muted' }, KEEPING_ELSEWHERE) : null,
      ),
      h('span', { class: 'ka-box' }, steps(keep, PHONE_BOXES)),
    ];
  }
  const own = data?.holdings ?? input.saved;
  if (own === undefined) {
    return [h('p', {})];
  }
  const { elsewhere } = input;
  const row = comeBackRow(elsewhere?.holdings ?? own, now, elsewhere?.keeps ?? input.keeps);
  if ('none' in row) {
    return [h('p', { class: 'muted' }, row.none)];
  }
  return [
    h(
      'h2',
      {},
      row.lead,
      h('b', {}, row.date),
      elsewhere === undefined ? null : h('br'),
      elsewhere === undefined ? null : h('span', { class: 'muted' }, elsewhereRow(elsewhere.name)),
    ),
  ];
}

/** The step boxes: one per transfer, up to `max`; filled once done. Hidden from screen readers, since the words say the count. */
function steps(keep: Progress, max: number): HTMLElement {
  return h('span', { class: 'ka-steps', 'aria-hidden': 'true' }, ...boxes(keep, max).map((on) => h('i', { class: on ? 'on' : undefined })));
}

/**
 * Draws the come-back row and column again from `input`, and nothing else:
 * the run's count moves every few seconds, and the rest of the view, its
 * focus and its open tip stay as they are.
 */
export function renderBack(input: HomeInput, root: ParentNode = document): void {
  const row = root.querySelector('.back-row');
  const column = root.querySelector('.col-back');
  const empty = input.data !== undefined && input.data.holdings.notes.length === 0;
  row?.replaceChildren(...backRow(input));
  column?.replaceChildren(...backColumn(input, empty ? input.data?.faucet : undefined).filter((child): child is HTMLElement => child instanceof HTMLElement));
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
          h(
            'div',
            { class: 'recent-h col-h' },
            h('h2', { class: 'lab' }, 'Recent'),
            h('a', { class: 'lab to-hist', href: '#history', 'data-fk': 'to-history' }, 'History', arrowIcon()),
          ),
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
          h('a', { class: 'btn', href: '#receive', 'data-fk': 'home-receive' }, 'Receive'),
          // on an empty purse the faucet is the one way on: Send would only say the purse is empty
          h('a', { class: `btn${faucet === undefined ? ' solid' : ''}`, href: '#send', 'data-fk': 'home-send' }, 'Send'),
        ),
      ),
    ),
  );
}

/**
 * The desktop's come-back column. Its last slot (`.cb-act`) is there in
 * every state with notes, empty when idle, so the cobalt band, and with it
 * the field, keeps its height when a run starts or ends.
 */
function backColumn(input: HomeInput, faucet: FaucetInfo | undefined): Child[] {
  const { keep } = input;
  if (keep !== undefined) {
    return [
      h('div', { class: 'col-h' }, h('h2', { class: 'lab' }, keepingLead(keep.amount))),
      h('p', { class: 'cb-date' }, keepingCount(keep)),
      // the boxes right under the note, as in the prototype: the two keep the idle note's three lines between them (styles.css)
      h(
        'div',
        { class: 'cb-body' },
        h('p', { class: 'note-line' }, keep.watching ? KEEPING_ELSEWHERE_NOTE : keepingNote(keep.amount, keep.spaced)),
        h('div', { class: 'cb-act' }, steps(keep, DESK_BOXES)),
      ),
    ];
  }
  const { elsewhere } = input;
  const keeps = elsewhere?.keeps ?? input.keeps;
  const head = h('div', { class: 'col-h' }, h('h2', { class: 'lab' }, comeBackHeading(keeps)));
  const own = input.data?.holdings ?? input.saved;
  if (own === undefined) {
    return [head];
  }
  const holdings = elsewhere?.holdings ?? own;
  const row = comeBackRow(holdings, input.now, keeps);
  if ('none' in row) {
    return [
      head,
      h('p', { class: 'note-line' }, row.none),
      faucet === undefined ? null : h('div', { class: 'cb-act' }, takeButton(input, faucet, 'home-take-d', false)),
      faucet === undefined ? null : takeError(input),
    ];
  }
  return [
    head,
    h('p', { class: 'cb-date' }, row.date),
    h(
      'p',
      { class: 'note-line' },
      elsewhere === undefined ? firstToGo(holdings, input.now, keeps) : elsewhereNote(elsewhere.name, holdings, input.now, keeps),
    ),
    h('div', { class: 'cb-act', 'aria-hidden': 'true' }),
  ];
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
  input.error?.at === 'take' ? errorLine(input, input.error.words) : null;

/** What went wrong, as an alert when it is new, and as a plain line when it is only drawn again. */
const errorLine = (input: HomeInput, words: string): HTMLElement =>
  h('p', { class: 'err home-err', role: input.alert ? 'alert' : undefined }, words);

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
      row.delta === undefined
        ? h('b', { class: 'n', 'aria-label': 'change not known' }, '?')
        : row.delta === 0
          ? h('b', { class: 'n', 'aria-label': 'no change' }, '—')
          : h('b', {}, change(row.delta)),
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
export function newDot(age: number): HTMLElement {
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
      h(
        'dl',
        {},
        h('dt', {}, 'Key'),
        h('dd', {}, h('code', {}, shortKey(record.mint))),
        h('dt', {}, 'Backup'),
        h('dd', {}, h('a', { class: 'to-bk', href: '#backup', 'data-fk': 'to-backup-d' }, backupRow(input))),
      ),
    ),
  );
}

/** What the last backup covers of what is held now, once read. */
const coverNow = (input: HomeInput): Cover | undefined => {
  const holdings = input.data?.holdings;
  // a note on its way in a keep-alive hop is spent, and the backup no longer covers it: Backup's own read leaves it out too
  return holdings === undefined || input.record.backup === undefined
    ? undefined
    : coverOf(
        holdings.notes.filter((note) => !note.moving),
        input.record.backup.ids,
        holdings.total,
      );
};

/** The mint column's Backup row: what the last backup covers and when, or that there is none. */
function backupRow(input: HomeInput): string {
  return coverRow(coverNow(input), input.record.backup?.t, input.now);
}

/**
 * Backup's way in on the phone, at the right of the top bar: marked when a
 * backup is wanted, the purse holding tokens of which the last one covers
 * under half (or there is none).
 */
function backupLink(input: HomeInput): HTMLElement {
  const holdings = input.data?.holdings;
  const wanted = holdings !== undefined && wantsBackup(coverNow(input) ?? { covered: 0, total: holdings.total });
  return h(
    'a',
    {
      class: `lab tb-bk phone-only${wanted ? ' wanted' : ''}`,
      href: '#backup',
      'data-fk': 'to-backup',
      'aria-label': !wanted
        ? undefined
        : input.record.backup === undefined
          ? `Backup: there is no backup of ${input.record.name} yet`
          : 'Backup: your last backup covers under half of what the purse holds',
    },
    wanted ? h('i', { class: 'bk-mark', 'aria-hidden': 'true' }) : null,
    'Backup',
  );
}
