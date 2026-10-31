// History: the shown mint's register, by day, newest first. Each row is a
// button that opens what was behind it, and a send's row whose hand-back or
// send can still be shown opens it on Send from there. The words and the
// numbers are register.ts's; this builds them into elements.

import { type Child, h } from './dom.ts';
import type { AskRecord, LogEntry, MintRecord } from './entries.ts';
import { change, hm } from './format.ts';
import { newDot, PINK_MS } from './home.ts';
import { KEPT_WORDS, type Row, footOf, footWords, register, rowWords } from './register.ts';
import { deskbar, tbSub } from './shell.ts';

export interface HistoryInput {
  record: MintRecord;
  /** The purse's log, newest first: History shows the shown mint's. */
  log: readonly LogEntry[];
  /** What the purse holds of the mint now, once read: the Held column is worked back from it. */
  total: number | undefined;
  /** The requests this purse made, for the rows of what came in for them. */
  asks: readonly AskRecord[];
  /** The requests whose hand-back the purse still keeps, by id. */
  kept: ReadonlySet<string>;
  /** Whether the purse has read what it keeps at the mint's server: until then a row says nothing of a hand-back. */
  keptRead: boolean;
  /** The server's host, as the rows name it. */
  host: string;
  /** The row that is open, by {@link rowKey}. */
  open: string | undefined;
  now: number;
  toggle: (key: string) => void;
  /** A send's row: shows the send, its hand-back or what was lost, on Send. */
  openRow: (entry: LogEntry) => void;
}

/** A row's key: its entry's place in the log. */
export const rowKey = (entry: LogEntry): string => String(entry.id ?? entry.t);

/** History. */
export function historyView(input: HistoryInput): HTMLElement {
  const { record, now } = input;
  const days = register(input.log, record.mint, input.total, now);
  const foot = footWords(footOf(input.log, record.mint), now);
  const body: Child[] =
    days.length === 0
      ? [h('p', { class: 'note-line muted reg-none' }, 'Nothing yet.')]
      : [
          h(
            'div',
            { class: 'reg-cols lab', 'aria-hidden': 'true' },
            h('span', {}, 'Time'),
            h('span', {}, 'Entry'),
            h('span', {}, 'Tokens'),
            h('span', {}, 'Held'),
          ),
          ...days.flatMap((day) => [
            h('h2', { class: 'day' }, h('span', { class: 'lab' }, day.name), h('span', { class: 'rule', 'aria-hidden': 'true' })),
            h('ul', { class: 'hlist', role: 'list', 'aria-label': `${day.name}, newest first` }, ...day.rows.map((row) => rowView(input, row))),
          ]),
        ];
  return h(
    'div',
    { class: 'view v-hist v-sub flow' },
    deskbar(true, 'history'),
    h(
      'main',
      { class: 'vmain' },
      h(
        'section',
        { class: 'band-p' },
        h(
          'div',
          { class: 'wrap' },
          tbSub('History', record),
          h('h1', { class: 'h1 hist-h1', tabindex: -1, 'data-fk': 'h1' }, 'What came and ', h('em', {}, 'went'), '.'),
          h('p', { class: 'lab muted hist-where' }, `${record.name} · kept in this browser only`),
          ...body,
        ),
      ),
      h(
        'section',
        { class: 'band-c' },
        h(
          'div',
          { class: 'wrap' },
          h(
            'div',
            { class: 'c-mid hist-foot' },
            h('p', { class: 'lab muted' }, foot.lab),
            h('p', { class: 'body' }, foot.body),
            h('p', { class: 'body' }, KEPT_WORDS),
          ),
        ),
      ),
    ),
  );
}

/** One row: its button, and what it opens to when it is open. */
function rowView(input: HistoryInput, row: Row): HTMLElement {
  const { entry, held } = row;
  const key = rowKey(entry);
  const open = input.open === key;
  const words = rowWords(entry, {
    kept: entry.ask !== undefined && input.kept.has(entry.ask) ? true : input.keptRead ? false : undefined,
    ask: entry.kind === 'received' ? input.asks.find((each) => each.id === entry.ask) : undefined,
    host: input.host,
    now: input.now,
  });
  const age = input.now - entry.t;
  const detailId = `d-${key}`;
  // what it changed: a dash for nothing, a question mark for a part of a send whose value no answer gave
  const amount =
    words.delta === undefined
      ? h('b', { class: 'n' }, h('span', { 'aria-hidden': 'true' }, '?'), h('span', { class: 'sr' }, ' change not known'))
      : words.delta === 0
        ? h('b', { class: 'n' }, h('span', { 'aria-hidden': 'true' }, '—'), h('span', { class: 'sr' }, ' no change'))
        : h('b', {}, change(words.delta), h('span', { class: 'sr' }, Math.abs(words.delta) === 1 ? ' token' : ' tokens'));
  const button = h(
    'button',
    {
      type: 'button',
      'data-fk': `h-${key}`,
      'aria-expanded': open ? 'true' : 'false',
      'aria-controls': open ? detailId : undefined,
      on: { click: () => input.toggle(key) },
    },
    h('time', { datetime: new Date(entry.t).toISOString() }, hm(entry.t)),
    h(
      'span',
      { class: 't' },
      age >= 0 && age < PINK_MS ? newDot(age) : null,
      words.title,
      words.sub === undefined ? null : h('small', {}, words.sub),
    ),
    amount,
    h('span', { class: 'held' }, held === undefined ? null : h('span', { class: 'sr' }, 'held '), held === undefined ? '' : String(held)),
  );
  const detail = open
    ? h(
        'div',
        { class: 'det', id: detailId },
        ...words.detail.map((line) => h('p', {}, line)),
        words.action === undefined
          ? null
          : h(
              'button',
              { class: 'btn sm', type: 'button', 'data-fk': `go-${key}`, on: { click: () => input.openRow(entry) } },
              words.action,
            ),
      )
    : null;
  return h('li', { class: `hrow${open ? ' open' : ''}` }, button, detail);
}
