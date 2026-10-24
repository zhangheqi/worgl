// Send (#send): scan or open a request, see what it spends and what comes
// back, send it, and hand the receiver their notes back as a code and a
// link. Its side views say why a request cannot go as it is, and what
// became of a send the mint did not answer. Drawn from SendState in
// app.ts; the words are in flow-words.ts, the planner in plan.ts.

import type { FaucetInfo } from 'worgl';

import { type Camera, attachCamera } from './camera.ts';
import { $, type Child, h, put, s } from './dom.ts';
import type { MintRecord } from './entries.ts';
import { stamp, waiting } from './first.ts';
import {
  type WayOut,
  ALREADY_HEADING,
  CAMERA,
  CHANGED,
  HANDBACK,
  HANDBACK_HEADING,
  LOST_HEADING,
  OWN_BODY,
  OWN_HEADING,
  QR,
  READING_BODY,
  READING_HEADING,
  SCAN_BODY,
  SCAN_HEADING,
  SENDING_LAB,
  SEND_NOW,
  STAGE_CODE,
  TURNED_HEADING,
  TURNED_REQUEST_HEADING,
  UNKNOWN_HEADING,
  WAITING_BODY,
  WAITING_HEADING,
  afterWords,
  alreadyBody,
  atMintWords,
  backLine,
  backTag,
  carriesWords,
  chosenWords,
  confirmHeading,
  durationLine,
  estimateSecs,
  explainWords,
  faucetButton,
  handBackQrLabel,
  handBackShareTitle,
  handBackStamp,
  keptWords,
  lostBody,
  nothingBack,
  notesLabel,
  outTag,
  partialBody,
  partialLine,
  scanCap,
  sendingHeading,
  sendOfLine,
  sentLine,
  shortWords,
  ticketWords,
  turnedBody,
  unknownBody,
} from './flow-words.ts';
import { corners, doCopy, fitPre, fitQR, rich, tearSvg } from './flow.ts';
import { f2, plusJoin } from './format.ts';
import { copyIcon } from './icons.ts';
import { hostOf } from './link.ts';
import type { AskLink } from './links.ts';
import { face } from './marks.ts';
import type { ChangeLands, Part, ShortCase, Shown } from './plan.ts';
import type { About, ReadRequest } from './purses.ts';
import { qrCode, qrPlan } from './qr.ts';
import { codeStamp, linkRow, scanner } from './receive.ts';
import { deskbar, tbSub } from './shell.ts';
import { type SliceNote, type SliceTag, drawSlice } from './slice.ts';

/** The steps and side views of Send. */
export type SendStep =
  | 'scan'
  | 'reading'
  | 'unknown'
  | 'own'
  | 'already'
  | 'short'
  | 'confirm'
  | 'sending'
  | 'handback'
  | 'waiting'
  | 'turned'
  | 'lost';

/** Where one transfer of a send stands. */
export type TransferState = 'waiting' | 'sending' | 'answered' | 'refused' | 'unsent';

/** A send in progress. */
export interface Run {
  /** When submit started (performance.now()), and when the sending view was first drawn. */
  t0: number;
  shownAt: number;
  /** Transfers, their times, and how long the send should take in all, ms. */
  n: number;
  afterSecs: number[];
  D: number;
  states: TransferState[];
  answered: number;
  refused: number;
  /** "Send the rest now" was pressed. */
  nowSent: boolean;
  done: boolean;
  /** Its log entry. */
  entry: number | undefined;
  /** The request's amount; the plan it follows, when the planner had one. */
  amount: number;
  parts: Part[] | undefined;
  /** When each answer came, in order, for the pink of its change. */
  landed: number[];
  sendNow: AbortController;
}

/** A hand-back to show: the receiver's notes as a link, and what came back. */
export interface HandBack {
  id: string;
  server: string;
  mint: string;
  /** The tokens it carries: what went, or the request's amount when that is not known. */
  amount: number;
  asked: number;
  link: string;
  /** When it was sent, and when its answers came: the change's pink runs from then. */
  t: number;
  landed: number;
  /** The change that came back (`undefined` when not known), where it landed, and when it can move. */
  back: number | undefined;
  backLands: ChangeLands | undefined;
  readyAt: number | undefined;
  /** The notes the send spent, for "the note went whole". */
  spent: number;
  /** A transfer was turned away: `turned` of them, and what went (`undefined` when not known). */
  partial: boolean;
  turned: number;
  went: number | undefined;
}

/** How a send ended without a hand-back to show at once, or with part of one. */
export interface SendEnd {
  amount: number;
  /** When it was sent (its log entry's time). */
  t: number;
  /** For turned: whether the mint's newest layer was full (else its rules refused it). */
  layerFull: boolean;
  /** For lost: the value lost, theirs and the change together, and the part of it that was theirs. */
  lost: number;
  lostTheirs: number;
  /** For waiting: words of the last try, and whether one runs now. */
  words: string | undefined;
  trying: boolean;
  /** The log entry it is. */
  entry: number | undefined;
  /** For lost, when other transfers went: their hand-back. */
  hb: HandBack | undefined;
  /** Whether the notes pay spent were the ones the planner chose, for a send settled later: not known for one this tab did not send. */
  asPlanned: boolean | undefined;
}

/** What Send is showing and doing. */
export interface SendState {
  step: SendStep;
  /** How and when the request came. */
  via: 'camera' | 'link';
  at: number;
  /** The scan step's field and its words. */
  paste: string;
  error: string | undefined;
  ask: AskLink | undefined;
  req: ReadRequest | undefined;
  /** The held mint it is for. */
  record: MintRecord | undefined;
  serverNote: string | undefined;
  /** A mint this purse does not hold: what its server says of it. */
  about: About | undefined;
  adding: boolean;
  readError: string | undefined;
  /** The plan and its numbers, as confirm and short draw them. */
  shown: Shown | undefined;
  short: ShortCase | undefined;
  /** What is ready now and resting now, in tokens. */
  ready: number;
  resting: number;
  /** Whether the mint's new notes land ready: its faucet's do then. */
  mintsReady: boolean;
  /** Whether the planner's choice agreed with the SDK's: set on short from pay's refusal, and on the hand-back. */
  plan: 'match' | 'differ' | undefined;
  signing: boolean;
  sendError: string | undefined;
  changed: boolean;
  run: Run | undefined;
  hb: HandBack | undefined;
  again: boolean;
  /** A send that ended while another screen showed, not seen yet. */
  unseen: boolean;
  faucet: { info: FaucetInfo; taking: boolean; error: string | undefined } | undefined;
  /** Sent already: kept with its hand-back, still on its way, turned away by the mint, or no longer kept. */
  already: { kind: 'kept' | 'outgoing' | 'turned' | 'gone'; t: number | undefined; amount: number; id: string } | undefined;
  end: SendEnd | undefined;
}

/** A fresh Send, on the scan step. */
export const blankSend = (): SendState => ({
  step: 'scan',
  via: 'link',
  at: 0,
  paste: '',
  error: undefined,
  ask: undefined,
  req: undefined,
  record: undefined,
  serverNote: undefined,
  about: undefined,
  adding: false,
  readError: undefined,
  shown: undefined,
  short: undefined,
  ready: 0,
  resting: 0,
  mintsReady: false,
  plan: undefined,
  signing: false,
  sendError: undefined,
  changed: false,
  run: undefined,
  hb: undefined,
  again: false,
  unseen: false,
  faucet: undefined,
  already: undefined,
  end: undefined,
});

/** A kept hand-back, as the list on Send shows it. */
export interface KeptView {
  key: string;
  amount: number;
  t: number | undefined;
}

/** What Send calls when the person acts. */
export interface SendActions {
  pasteChanged: (text: string) => void;
  open: () => void;
  camera: () => void;
  cameraOff: () => void;
  showKept: (key: string) => void;
  retryRead: () => void;
  add: () => void;
  send: () => void;
  sendNow: () => void;
  takeFaucet: () => void;
  showHandBack: () => void;
  showSend: () => void;
  showOwn: () => void;
  retrySettle: () => void;
}

/** Everything Send draws from. */
export interface SendInput {
  state: SendState;
  /** The mint the top bar shows: the request's when it is held, else the one Holdings shows. */
  bar: { mint: string; name: string };
  /** The request's mint, as the ticket names it: its name, or its host when it is not held. */
  name: string;
  /** The request's link, for the ticket's code. */
  link: string | undefined;
  qr: boolean | undefined;
  /** The scan step's numbers and slice: what the shown mint holds. */
  scan: { ready: number; resting: number; notes: readonly SliceNote[]; faucet: number | undefined };
  /** The request mint's notes, for the slices of short, confirm and sending. */
  notes: readonly SliceNote[];
  at: string;
  kept: readonly KeptView[];
  camera: Camera;
  scanError: string | undefined;
  now: number;
  actions: SendActions;
}

/** Send. */
export function sendView(input: SendInput): HTMLElement {
  const { state } = input;
  switch (state.step) {
    case 'reading':
      return readingView(input);
    case 'unknown':
      return unknownView(input);
    case 'own':
      return ownView(input);
    case 'already':
      return alreadyView(input);
    case 'short':
      return shortView(input);
    case 'confirm':
      return confirmView(input);
    case 'sending':
      return sendingView(input);
    case 'handback':
      return handBackView(input);
    case 'waiting':
    case 'turned':
    case 'lost':
      return endView(input);
    default:
      return scanView(input);
  }
}

/**
 * The frame of every step: the bars, the paper band with its top bar, and
 * the cobalt band. A flow step with nothing in the middle of its cobalt
 * (only its buttons) is `low`: the paper takes the height, and the cobalt
 * is the strip its buttons need.
 */
function frame(input: SendInput, paper: Child[], cobalt: Child[], cls = ' flow', attrs: Record<string, string> = {}): HTMLElement {
  const tearing = input.state.step === 'sending';
  const low = cls === ' flow' && !tearing && !cobalt.some((child) => child instanceof HTMLElement && child.classList.contains('c-mid'));
  return h(
    'div',
    { class: `view v-send v-sub${cls}${low ? ' low' : ''}${tearing ? ' is-sending' : ''}`, ...attrs },
    deskbar(true, 'send'),
    h(
      'main',
      { class: 'vmain' },
      h(
        'section',
        { class: `band-p${tearing ? ' tearing' : ''}` },
        h('div', { class: 'wrap' }, tbSub('Send', input.bar.mint, input.bar.name), ...paper),
        tearing ? h('div', { class: 'tear', id: 'tear', 'aria-hidden': 'true' }) : null,
      ),
      h('section', { class: `band-c${tearing ? ' tearing' : ''}` }, h('div', { class: 'wrap' }, ...cobalt)),
    ),
  );
}

/** The ticket stub: the request's code, small; how and when it came; its amount; its mint. */
function ticket(input: SendInput): HTMLElement {
  const { state } = input;
  const amount = state.req?.amount ?? 0;
  const words = ticketWords(state.via, state.at, amount);
  // the request's own link, drawn small; until it is written (or if it could not be), an empty stub, never a code of nothing
  const code = input.qr === true && input.link !== undefined && input.link !== '' ? qrCode(input.link) : undefined;
  let mini: Element;
  if (code === undefined) {
    mini = s('svg', { class: 'mini', width: 76, height: 76, viewBox: '0 0 76 76', 'aria-hidden': 'true', focusable: 'false' });
  } else {
    const u = Math.max(1, Math.floor(76 / (code.n + 8)));
    const px = (code.n + 8) * u;
    mini = s('svg', { class: 'mini', width: px, height: px, viewBox: `0 0 ${px} ${px}`, role: 'img', 'aria-label': words.qr });
    let d = '';
    for (let y = 0; y < code.n; y++) {
      for (let x = 0; x < code.n; x++) {
        if (code.isDark(y, x)) {
          d += `M${(x + 4) * u} ${(y + 4) * u}h${u}v${u}h${-u}z`;
        }
      }
    }
    mini.append(s('rect', { class: 'qr-bg', width: px, height: px }), s('path', { class: 'qr-fg', d, 'shape-rendering': 'crispEdges' }));
  }
  return h(
    'div',
    { class: 'ticket' },
    h('div', { class: 'stub' }, mini),
    h(
      'div',
      { class: 'tk' },
      h('span', { class: 'lab' }, words.lab),
      h('b', {}, words.b),
      h('span', { class: 'who' }, face(state.req?.mint ?? '', 2.6, 2), h('span', { class: 'lab' }, input.name)),
    ),
  );
}

const h1 = (cls: string, parts: Parameters<typeof rich>[0]): HTMLElement =>
  h('h1', { class: `h1 ${cls}`, tabindex: -1, 'data-fk': 'h1' }, ...rich(parts));

const back = (solid = false): HTMLElement => h('a', { class: `btn${solid ? ' solid' : ''}`, href: '#home' }, 'Back to the purse');

// ---------- scan ----------

function scanView(input: SendInput): HTMLElement {
  const { state, actions, scan } = input;
  const paper: Child[] = [
    h1('mt-10', SCAN_HEADING),
    h('p', { class: 'body mt-12' }, SCAN_BODY),
    // while the camera runs, its toggle sits under the viewfinder, inside the area, where a tap reaches it
    h('div', { class: input.camera.kind === 'running' ? 'scan-area live' : 'scan-area' }, ...scanner(input, '', CAMERA.orBelow)),
    input.scanError === undefined ? null : h('p', { class: 'err flow-err', role: 'alert' }, input.scanError),
    h(
      'div',
      { class: 'slice-foot' },
      h('div', { class: 'field slice', id: 'slice-scan', 'aria-hidden': 'true' }),
      h('p', { class: 'slice-cap' }, ...rich(scanCap(scan.ready, scan.resting, input.at, scan.faucet))),
    ),
  ];
  const cobalt: Child[] = [
    h(
      'form',
      {
        on: {
          submit: (event) => {
            event.preventDefault();
            actions.open();
          },
        },
      },
      h(
        'div',
        { class: 'fld' },
        h('label', { class: 'lab', for: 'req-link' }, 'Their request link'),
        h(
          'div',
          { class: 'paste-row' },
          h('input', {
            class: 'inp mono',
            id: 'req-link',
            'data-fk': 'req-link',
            placeholder: 'Paste their request link',
            autocomplete: 'off',
            autocapitalize: 'off',
            spellcheck: 'false',
            value: state.paste,
            on: { input: (event) => actions.pasteChanged((event.target as HTMLInputElement).value) },
          }),
          h('button', { class: 'btn solid', type: 'submit', 'data-fk': 'req-go' }, 'Open'),
        ),
      ),
      state.error === undefined ? null : h('p', { class: 'err flow-err', role: 'alert' }, state.error),
    ),
  ];
  if (input.kept.length > 0) {
    cobalt.push(
      h('div', { class: 'or lab' }, 'Or show a hand-back again'),
      h(
        'div',
        { class: 'samples' },
        ...input.kept.map((kept) => {
          const words = keptWords(kept.amount, kept.t, input.now);
          return h(
            'button',
            { class: 'stamp sample', type: 'button', 'aria-label': words.label, 'data-fk': `hb-${kept.key.slice(0, 16)}`, on: { click: () => actions.showKept(kept.key) } },
            h('b', {}, String(kept.amount)),
            h('span', { class: 'lab' }, words.lab),
          );
        }),
      ),
    );
  }
  return frame(input, paper, cobalt, ' v-scan');
}

// ---------- reading, unknown, own, sent already ----------

function readingView(input: SendInput): HTMLElement {
  const { state, actions } = input;
  const failed = state.readError !== undefined;
  return frame(
    input,
    [
      // while its link is still being read there is no request to show yet
      state.req === undefined ? null : ticket(input),
      h1(state.req === undefined ? 'mt-10' : 'mt-16', READING_HEADING),
      h('p', { class: 'body mt-10', role: 'status' }, READING_BODY),
      failed ? h('p', { class: 'err flow-err', role: 'alert' }, state.readError) : null,
    ],
    [
      h(
        'div',
        { class: 'c-low' },
        failed
          ? h(
              'div',
              { class: 'btn-row' },
              h('a', { class: 'btn', href: '#home' }, 'Cancel'),
              h('button', { class: 'btn solid', type: 'button', 'data-fk': 'retry', on: { click: actions.retryRead } }, 'Try again'),
            )
          : null,
      ),
    ],
    ' flow',
    { 'aria-busy': failed ? 'false' : 'true' },
  );
}

function unknownView(input: SendInput): HTMLElement {
  const { state, actions } = input;
  const server = state.ask?.server ?? '';
  const host = hostOf(server);
  let middle: Child;
  if (state.about !== undefined && state.req !== undefined) {
    middle = stamp(state.req.mint, host, state.about, false);
  } else if (state.readError !== undefined) {
    middle = h(
      'div',
      { class: 'c-mid first-acts' },
      h('p', { class: 'err', role: 'alert' }, state.readError),
      h('button', { class: 'btn', type: 'button', 'data-fk': 'retry', on: { click: actions.retryRead } }, 'Try again'),
    );
  } else {
    middle = waiting(server);
  }
  return frame(
    input,
    [ticket(input), h1('mt-16', UNKNOWN_HEADING), h('p', { class: 'body mt-10' }, unknownBody(state.req?.amount ?? 0, host))],
    [
      middle,
      h(
        'div',
        { class: 'c-low first-acts' },
        h(
          'div',
          { class: 'btn-row' },
          h('a', { class: 'btn', href: '#home' }, 'Cancel'),
          h(
            'button',
            {
              class: 'btn solid',
              type: 'button',
              disabled: state.about === undefined || state.adding,
              'data-fk': 'add-mint',
              on: { click: actions.add },
            },
            state.adding ? 'Adding…' : 'Add this mint',
          ),
        ),
      ),
    ],
  );
}

function ownView(input: SendInput): HTMLElement {
  return frame(
    input,
    [ticket(input), h1('mt-16', OWN_HEADING), h('p', { class: 'body mt-10' }, OWN_BODY)],
    [
      h(
        'div',
        { class: 'c-low' },
        h(
          'div',
          { class: 'btn-row' },
          back(),
          h('button', { class: 'btn solid', type: 'button', 'data-fk': 'show-own', on: { click: input.actions.showOwn } }, 'Show it'),
        ),
      ),
    ],
  );
}

function alreadyView(input: SendInput): HTMLElement {
  const { state, actions } = input;
  const before = state.already;
  const kind = before?.kind ?? 'gone';
  const buttons: Child[] = [back(kind === 'gone' || kind === 'turned')];
  if (kind === 'kept') {
    buttons.push(h('button', { class: 'btn solid', type: 'button', 'data-fk': 'show-hb', on: { click: actions.showHandBack } }, 'Show the hand-back'));
  } else if (kind === 'outgoing') {
    buttons.push(h('button', { class: 'btn solid', type: 'button', 'data-fk': 'show-send', on: { click: actions.showSend } }, 'Show the send'));
  }
  return frame(
    input,
    [
      ticket(input),
      h1('mt-16', kind === 'turned' ? TURNED_REQUEST_HEADING : ALREADY_HEADING),
      h('p', { class: 'body mt-10' }, alreadyBody(kind, before?.amount ?? state.req?.amount ?? 0, before?.t, input.now)),
    ],
    [h('div', { class: 'c-low' }, h('div', { class: 'btn-row' }, ...buttons))],
  );
}

// ---------- not enough ready ----------

const ROMAN = ['I', 'II', 'III', 'IV'];

function shortView(input: SendInput): HTMLElement {
  const { state, actions } = input;
  const c = state.short ?? { kind: 'over', has: 0 };
  const req = state.req;
  const faucet = state.faucet;
  const words = shortWords(c, {
    amount: req?.amount ?? 0,
    outputs: req?.outputs ?? [],
    ready: state.ready,
    resting: state.resting,
    at: input.at,
    faucet: faucet === undefined ? undefined : { amount: Number(faucet.info.amount), mint: input.name, ready: state.mintsReady },
  });
  const way = (out: WayOut): Child => {
    const line = h('p', {}, out.text, h('small', {}, out.small));
    if (out.faucet !== true || faucet === undefined) {
      return line;
    }
    return h(
      'div',
      {},
      line,
      h(
        'button',
        { class: 'btn take', type: 'button', disabled: faucet.taking, 'data-fk': 'short-faucet', on: { click: actions.takeFaucet } },
        faucetButton(Number(faucet.info.amount), faucet.taking),
      ),
      faucet.error === undefined ? null : h('p', { class: 'err flow-err', role: 'alert' }, faucet.error),
    );
  };
  const preview = h('p', { class: 'msg-preview', id: 'msg-text' }, `“${words.message}”`);
  const copy = h(
    'div',
    {},
    h(
      'button',
      {
        class: 'btn solid copy',
        type: 'button',
        'data-fk': 'copy-msg',
        on: { click: (event) => doCopy(event.currentTarget as HTMLElement, words.message, preview) },
      },
      copyIcon(),
      h('span', {}, 'Copy a message'),
    ),
    preview,
  );
  const items = [...words.options.map(way), copy];
  return frame(
    input,
    [
      ticket(input),
      h1('mt-16', words.h1),
      h('p', { class: 'body mt-10' }, ...rich(words.body)),
      h('div', { class: 'field slice', id: 'slice-short', 'aria-hidden': 'true' }),
    ],
    [
      h(
        'div',
        { class: 'c-mid' },
        h('p', { class: 'lab muted' }, 'What will work'),
        h('ol', { class: 'options' }, ...items.map((item, i) => h('li', {}, h('i', {}, ROMAN[i] ?? ''), item))),
      ),
      h('div', { class: 'c-low short-low' }, h('a', { class: 'tbtn on-c', href: '#home' }, 'Cancel')),
    ],
    ' flow',
    state.plan === undefined ? {} : { 'data-plan': state.plan },
  );
}

/** The short slice: what is ready and resting, and ghosts where the missing tokens, or the missing note, would be. */
function drawShortSlice(input: SendInput, host: HTMLElement): void {
  const { state } = input;
  const c = state.short;
  const A = state.req?.amount ?? 0;
  const ghosts: SliceNote[] = [];
  let tag = '';
  if (c?.kind === 'notes' || c?.kind === 'outputs') {
    const maxOut = Math.max(0, ...(state.req?.outputs ?? []));
    ghosts.push({ id: 'sg0', v: maxOut, lane: 'ready', order: 1e6 });
    tag = `Needs a note of ${maxOut}`;
  } else if (c !== undefined) {
    const need = c.kind === 'over' ? A - c.has : A - state.ready;
    for (let left = need, i = 0; left > 0 && i < 8; i++) {
      const v = Math.min(64, left);
      ghosts.push({ id: `sg${i}`, v, lane: 'ready', order: 1e6 + i });
      left -= v;
    }
    tag = c.kind === 'over' ? `${need} short` : `${need} short now`;
  }
  drawSlice(host, {
    lanes: ['ready', 'rest'],
    notes: [...input.notes, ...ghosts],
    marks: new Map(ghosts.map((ghost) => [ghost.id, 'ghost' as const])),
    tags: ghosts[0] === undefined ? [] : [{ at: ghosts[0].id, text: tag, kind: 'out' }],
  });
}

// ---------- confirm ----------

function confirmView(input: SendInput): HTMLElement {
  const { state, actions } = input;
  const A = state.req?.amount ?? 0;
  const shown = state.shown;
  const n = shown?.chosen.length ?? 1;
  const one = n === 1;
  const back = shown === undefined ? 0 : shown.back.reduce((a, b) => a + b, 0);
  const paper: Child[] = [ticket(input), h1('mt-18', confirmHeading(A))];
  if (shown !== undefined) {
    paper.push(h('div', { class: 'field slice', id: 'slice-confirm', 'aria-hidden': 'true' }));
    const values = shown.chosen.map((note) => note.v);
    paper.push(
      one
        ? h(
            'div',
            { class: 'split' },
            h('div', {}, h('b', {}, String(values[0] ?? 0)), h('span', { class: 'lab' }, notesLabel(1))),
            h('span', { class: 'op' }, '→'),
            h('div', {}, h('b', {}, String(A)), h('span', { class: 'lab' }, 'To them')),
            back > 0 ? h('span', { class: 'op' }, '+') : null,
            back > 0 ? h('div', {}, h('b', {}, String(back)), h('span', { class: 'lab' }, 'Back to you')) : null,
          )
        : h(
            'div',
            { class: 'split many' },
            h('div', {}, h('b', {}, plusJoin(values)), h('span', { class: 'lab' }, notesLabel(n))),
            h('span', { class: 'op' }, '→'),
            h('div', {}, h('b', {}, String(A)), h('span', { class: 'lab' }, 'To them')),
          ),
      h('p', { class: 'explain' }, explainWords(n, shown.back, shown.changeLands, input.at)),
      h('p', { class: 'after' }, afterWords(shown.readyAfter, shown.laterAfter, input.at)),
    );
  }
  if (state.serverNote !== undefined) {
    paper.push(h('p', { class: 'note-line muted mt-10' }, state.serverNote));
  }
  if (state.changed) {
    paper.push(h('p', { class: 'note-line mt-10', role: 'status' }, CHANGED));
  }
  const dur = estimateSecs(n);
  const pre = one
    ? [
        h('p', { class: 'lab muted' }, 'One transfer'),
        h(
          'div',
          { class: 'count once' },
          h('b', {}, 'At ', h('em', {}, 'once'), '.'),
          h('p', {}, h('b', {}, 'Keep this open.'), h('br'), h('span', { class: 'muted' }, 'Then you give them a hand-back.')),
        ),
      ]
    : [
        h('p', { class: 'lab muted' }, 'Takes about'),
        h(
          'div',
          { class: 'count' },
          h('b', {}, String(dur), h('i', {}, 's')),
          h('p', {}, h('b', {}, 'Keep this open while it goes.'), h('br'), h('span', { class: 'muted' }, 'Then you give them a hand-back.')),
        ),
      ];
  return frame(input, paper, [
    h('div', { class: 'c-mid pre' }, ...pre),
    h(
      'div',
      { class: 'c-low' },
      h(
        'div',
        { class: 'btn-row' },
        h('a', { class: 'btn', href: '#home' }, 'Cancel'),
        h(
          'button',
          { class: 'btn solid', type: 'button', disabled: state.signing, 'data-fk': 'send-go', on: { click: actions.send } },
          state.signing ? 'Sending…' : `Send ${A}`,
        ),
      ),
      h('p', { class: 'note-line muted pre-alt mt-12' }, durationLine(n)),
      state.sendError === undefined ? null : h('p', { class: 'err flow-err', role: 'alert' }, state.sendError),
    ),
  ]);
}

/** Confirm's slice: the ready and resting lanes, the chosen notes outlined and tagged. */
function drawConfirmSlice(input: SendInput, host: HTMLElement): void {
  const chosen = input.state.shown?.chosen ?? [];
  const first = chosen[0];
  drawSlice(host, {
    lanes: ['ready', 'rest'],
    notes: [...input.notes],
    marks: new Map(chosen.map((note) => [note.id, 'outline' as const])),
    tags: first === undefined ? [] : [{ at: first.id, text: chosen.length === 1 ? 'This note' : 'These notes', kind: 'ink' }],
  });
}

// ---------- sending ----------

/** Elapsed ms of a run, by the page's clock. */
export const elapsed = (run: Run): number => performance.now() - run.t0;

/** The countdown: whole seconds left, never above the stated total. */
export const secondsLeft = (run: Run, el: number): number => Math.min(Math.round(run.D / 1000), Math.max(0, Math.ceil((run.D - el) / 1000)));

/** How far along the stated time a run is, 0 to 1. */
export const progressOf = (run: Run, el: number): number => Math.min(1, el / run.D);

/** The transfers answered, and one more than the highest that left its wait: "k of n". */
function counts(run: Run): { answered: number; k: number; waiting: boolean } {
  let k = 0;
  run.states.forEach((step, i) => {
    if (step !== 'waiting') {
      k = i + 1;
    }
  });
  return { answered: run.answered, k, waiting: run.states.some((step) => step === 'waiting') };
}

function stages(input: SendInput, run: Run): Child[] {
  const values = run.parts?.map((part) => part.note.v) ?? [];
  const back = run.parts?.reduce((sum, part) => sum + part.change.reduce((a, b) => a + b, 0), 0) ?? 0;
  const slow = !run.done && elapsed(run) > run.D;
  const mint = atMintWords(run.n, counts(run).k, { done: run.done, slow }, run.amount, back);
  return [
    h('li', { class: 'done' }, h('i'), h('span', {}, values.length > 0 ? chosenWords(values) : notesLabel(run.n))),
    h('li', { class: run.done ? 'done' : 'now' }, h('i'), h('span', {}, mint.line, h('small', {}, mint.small))),
    h('li', { class: run.done ? 'now' : 'todo' }, h('i'), h('span', {}, STAGE_CODE)),
  ];
}

function sendNowButton(input: SendInput, run: Run): HTMLElement | null {
  if (run.n < 2 || run.nowSent || run.done || !counts(run).waiting) {
    return null;
  }
  return h(
    'button',
    { class: 'tbtn on-c send-now', type: 'button', 'data-fk': 'send-now', on: { click: input.actions.sendNow } },
    h('span', {}, SEND_NOW.text),
    h('small', {}, SEND_NOW.small),
  );
}

function sendingView(input: SendInput): HTMLElement {
  const { state } = input;
  const run = state.run;
  const A = state.req?.amount ?? run?.amount ?? 0;
  if (run === undefined) {
    return frame(input, [h1('mt-8', sendingHeading(A))], []);
  }
  const el = elapsed(run);
  const prog = f2(progressOf(run, el) * 100);
  const D = Math.round(run.D / 1000);
  const count =
    run.n === 1
      ? h(
          'div',
          { class: 'count once' },
          h('b', {}, 'At ', h('em', {}, 'once'), '.'),
          h('p', {}, h('b', {}, 'Keep this open.'), h('br'), h('span', { class: 'muted' }, 'Then the hand-back.')),
        )
      : h(
          'div',
          { class: 'count', role: 'timer', 'aria-live': 'off' },
          h('b', { id: 'secs' }, String(secondsLeft(run, el)), h('i', {}, 's')),
          h('p', {}, h('b', {}, 'Keep this open.'), h('br'), h('span', { class: 'muted' }, `About ${D} s in all.`)),
        );
  return frame(
    input,
    [
      h('p', { class: 'lab muted mt-10' }, SENDING_LAB),
      h1('mt-8', sendingHeading(A)),
      h('div', { class: 'field slice', id: 'slice-send', 'aria-hidden': 'true' }),
      h('ol', { class: 'stages', id: 'stages' }, ...stages(input, run)),
    ],
    [
      count,
      h('div', { id: 'send-now-slot' }, sendNowButton(input, run)),
      h(
        'div',
        { class: 'tr-wrap', 'aria-hidden': 'true' },
        h('div', { class: 'trule' }, h('i', { id: 'tr-i', style: { width: `${prog}%` } }), h('b', { id: 'tr-b', style: { left: `${prog}%` } })),
        h(
          'div',
          { class: 'trule-labs lab' },
          ...(run.n === 1
            ? [h('span', {}, 'To the mint'), h('span', {}, 'Hand-back')]
            : [h('span', { class: 's' }, '0 s'), h('span', { class: 's' }, `${D} s`)]),
        ),
      ),
    ],
  );
}

/**
 * The sending slice: the spent notes as ghosts once their transfer leaves,
 * and each answered part's change landed in pink, the change still to come
 * laid out but not drawn. It follows the count of answers, not their
 * identity: the SDK does not say which note a transfer spends.
 */
function drawSendSlice(input: SendInput, host: HTMLElement): void {
  const { state } = input;
  const run = state.run;
  if (run === undefined) {
    return;
  }
  const parts = run.parts ?? [];
  const A = state.req?.amount ?? run.amount;
  const marks = new Map<string, 'ghost'>();
  const landed: SliceNote[] = [];
  const reserve: SliceNote[] = [];
  const pink = new Map<string, number>();
  const answered = run.answered;
  const left = run.states.filter((step) => step !== 'waiting').length;
  let out = 0;
  parts.forEach((part, i) => {
    if (i < Math.max(left, answered)) {
      marks.set(part.note.id, 'ghost');
      out += part.theirs.reduce((a, b) => a + b, 0);
    }
    const lane = part.lands >= lastLayer ? 'rest' : 'ready';
    part.change.forEach((v, j) => {
      const note: SliceNote = { id: `cb${i}-${j}`, v, lane, order: i * 100 + j };
      if (i < answered) {
        landed.push(note);
        const at = run.landed[i];
        if (at !== undefined) {
          pink.set(note.id, at);
        }
      } else {
        reserve.push(note);
      }
    });
  });
  const tags: SliceTag[] = [];
  const firstGone = parts.find((part) => marks.has(part.note.id));
  if (firstGone !== undefined) {
    tags.push({ at: firstGone.note.id, text: outTag(out, A), kind: 'ink' });
  }
  const firstBack = landed[0];
  if (firstBack !== undefined) {
    tags.push({ at: firstBack.id, text: backTag(landed.reduce((sum, note) => sum + note.v, 0)), kind: 'pink', below: true });
  }
  const order = Math.max(0, ...input.notes.map((note) => note.order)) + 1;
  drawSlice(host, {
    lanes: ['ready', 'rest'],
    notes: [...input.notes, ...landed.map((note) => ({ ...note, order: order + note.order }))],
    marks,
    pink,
    tags,
    reserve,
  });
}

/** The last layer, where change usually lands; the slices draw it as Resting. */
let lastLayer = 3;

/** Tells the sending slice which layer is the mint's last. */
export function setLastLayer(layers: number): void {
  lastLayer = Math.max(1, layers - 1);
}

/**
 * The parts of the sending view an answer changes, redrawn in place: the
 * stages, the slice and "Send the rest now". The countdown and the tear
 * are the loop's (see app.ts).
 */
export function refreshSending(input: SendInput): void {
  const run = input.state.run;
  if (run === undefined) {
    return;
  }
  const list = $('#stages');
  if (list !== null) {
    put(list, ...stages(input, run));
  }
  const slot = $('#send-now-slot');
  if (slot !== null) {
    const focused = slot.contains(document.activeElement);
    slot.replaceChildren(...[sendNowButton(input, run)].filter((el): el is HTMLElement => el !== null));
    if (focused && slot.childElementCount === 0) {
      $<HTMLElement>('#app [data-fk="h1"]')?.focus({ preventScroll: true });
    }
  }
  const host = $('#slice-send');
  if (host !== null) {
    drawSendSlice(input, host);
  }
}

/** One frame of the sending loop: the countdown, the rule and the tear. */
export function tickSending(run: Run, still: boolean): void {
  const el = elapsed(run);
  const prog = progressOf(run, el);
  const host = $('#tear');
  if (host !== null) {
    const W = Math.max(100, Math.round(host.clientWidth));
    host.replaceChildren(tearSvg(W, still ? 0 : prog));
  }
  const secs = $('#secs');
  if (secs !== null) {
    const left = run.done || el > run.D ? 0 : secondsLeft(run, el);
    if (secs.firstChild?.textContent !== String(left)) {
      secs.replaceChildren(String(left), h('i', {}, 's'));
    }
  }
  $('#tr-i')?.style.setProperty('width', `${f2(prog * 100)}%`);
  $('#tr-b')?.style.setProperty('left', `${f2(prog * 100)}%`);
}

// ---------- hand-back ----------

function handBackView(input: SendInput): HTMLElement {
  const { state } = input;
  const hb = state.hb;
  if (hb === undefined) {
    return frame(input, [h1('mt-8', HANDBACK_HEADING)], []);
  }
  const age = input.now - hb.landed;
  const fresh = !state.again && age < 6000;
  let changed: HTMLElement | null;
  if (hb.partial) {
    // the turned-away note is back, as fresh as any change: pink while it is new, and cold when shown again
    const line = partialLine(hb.went, hb.asked, hb.turned);
    changed = h(
      'p',
      { class: 'changed' },
      h('i', { class: `sw ${fresh ? 'live' : 'cold'}`, 'aria-hidden': 'true', style: { animationDelay: `${f2(-age / 1000)}s` } }),
      h('span', {}, h('b', {}, line.b), h('span', { class: 'muted' }, line.muted)),
    );
  } else if (hb.back === undefined) {
    changed = null;
  } else if (hb.back > 0) {
    const line = backLine(hb.back, hb.backLands, hb.readyAt, input.now);
    changed = h(
      'p',
      { class: 'changed' },
      h('i', { class: `sw ${fresh ? 'live' : 'cold'}`, 'aria-hidden': 'true', style: { animationDelay: `${f2(-age / 1000)}s` } }),
      h('span', {}, h('b', {}, line.b), h('span', { class: 'muted' }, line.muted)),
    );
  } else {
    changed = h('p', { class: 'changed' }, h('i', { class: 'sw cold', 'aria-hidden': 'true' }), h('span', { class: 'muted' }, nothingBack(hb.spent)));
  }
  const paper: Child[] = [
    h('p', { class: 'lab muted mt-10' }, sentLine(hb.amount, hb.t, input.now)),
    h1('mt-8', HANDBACK_HEADING),
    h('p', { class: 'body mt-12' }, hb.partial ? partialBody(hb.went, hb.asked) : carriesWords(hb.amount)),
    changed,
  ];
  if (hb.link === '') {
    // the receiver's notes are kept, but no link could be written for them here: never a code of nothing
    return frame(input, paper, [
      h('div', { class: 'c-mid hb-mid' }, h('p', { class: 'body', role: 'alert' }, HANDBACK.unwritten)),
      h('div', { class: 'c-low' }, h('div', { class: 'btn-row hb-acts' }, h('a', { class: 'btn solid', href: '#home' }, 'Done'))),
    ]);
  }
  const plan = input.qr === true ? qrPlan(hb.link) : undefined;
  const linkFirst = plan !== undefined && plan.kind !== 'plain';
  const copy = h(
    'button',
    {
      class: `btn${linkFirst ? ' solid' : ''}`,
      type: 'button',
      'data-fk': 'copy-take',
      // the link row's own code, in the page, is what the fallback selects
      on: { click: (event) => doCopy(event.currentTarget as HTMLElement, hb.link, $('#take-link-out')) },
    },
    copyIcon(),
    h('span', {}, HANDBACK.copy),
  );
  const row = linkRow(hb.link, 'take-link-out', undefined, HANDBACK.share, handBackShareTitle(hb.amount), 'copy-take-ib', 'share-take');
  return frame(
    input,
    paper,
    [
      h('div', { class: 'c-mid hb-mid' }, stampOf(input, hb)),
      h(
        'div',
        { class: 'c-low' },
        row,
        h('div', { class: 'btn-row hb-acts' }, copy, h('a', { class: `btn${linkFirst ? '' : ' solid'}`, href: '#home' }, 'Done')),
        h('p', { class: 'note-line muted hb-again' }, HANDBACK.again),
      ),
    ],
    ' flow',
    state.plan === undefined || state.again ? {} : { 'data-plan': state.plan },
  );
}

/** The hand-back's stamp: the code stamp, without the middle class (its column holds that). */
function stampOf(input: SendInput, hb: HandBack): HTMLElement {
  const el = codeStamp(input, hb.link, handBackQrLabel(hb.amount), handBackStamp(hb.amount, input.name), QR.denseTake);
  el.classList.remove('c-mid');
  return el;
}

// ---------- waiting, turned away, lost ----------

function endView(input: SendInput): HTMLElement {
  const { state, actions } = input;
  const end = state.end;
  const A = end?.amount ?? state.req?.amount ?? 0;
  let heading = WAITING_HEADING;
  let body = WAITING_BODY;
  const buttons: Child[] = [];
  if (state.step === 'waiting') {
    buttons.push(
      back(),
      h(
        'button',
        { class: 'btn solid', type: 'button', disabled: end?.trying === true, 'data-fk': 'retry', on: { click: actions.retrySettle } },
        end?.trying === true ? 'Trying…' : 'Try again now',
      ),
    );
  } else if (state.step === 'turned') {
    heading = TURNED_HEADING;
    body = turnedBody(end?.layerFull ?? false, input.at);
    buttons.push(back(true));
  } else {
    heading = LOST_HEADING;
    body = lostBody(end?.lostTheirs ?? A, end?.lost ?? A);
    if (end?.hb !== undefined) {
      buttons.push(back(), h('button', { class: 'btn solid', type: 'button', 'data-fk': 'show-hb', on: { click: actions.showHandBack } }, 'Show the hand-back'));
    } else {
      buttons.push(back(true));
    }
  }
  return frame(
    input,
    [
      h('p', { class: 'lab muted mt-10' }, (state.step === 'waiting' ? sentLine : sendOfLine)(A, end?.t ?? input.now, input.now)),
      h1('mt-8', heading),
      h('p', { class: 'body mt-12' }, body),
      state.step === 'waiting' && end?.words !== undefined ? h('p', { class: 'err flow-err', role: 'alert' }, end.words) : null,
    ],
    [h('div', { class: 'c-low' }, h('div', { class: 'btn-row' }, ...buttons))],
  );
}

// ---------- once in the page ----------

/** What the view needs once it is in the page: its slice, the large time, the code's size, the camera. */
export function mountSend(input: SendInput): void {
  const { state } = input;
  if (state.step === 'scan') {
    const host = $('#slice-scan');
    if (host !== null) {
      drawSlice(host, { lanes: ['ready'], notes: [...input.scan.notes] });
    }
    const box = $('.scan-area .scanner');
    if (box !== null) {
      attachCamera(box);
    }
  }
  const short = $('#slice-short');
  if (short !== null) {
    drawShortSlice(input, short);
  }
  const confirm = $('#slice-confirm');
  if (confirm !== null) {
    drawConfirmSlice(input, confirm);
  }
  const send = $('#slice-send');
  if (send !== null) {
    drawSendSlice(input, send);
  }
  if (state.step === 'confirm') {
    fitPre();
  }
  if (state.step === 'handback') {
    fitQR();
  }
}
