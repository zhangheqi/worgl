// Receive (#receive): ask for an amount, show the request as a code and a
// link, and take in the hand-back that comes for it, from a link, a paste
// or the camera. Three steps: ask, request, and take (opened by a #take=
// link). The views are drawn from ReceiveState in app.ts; their words are
// in flow-words.ts, and the field's slice is drawn once the view is in the
// page (see mountReceive).

import { type Camera, attachCamera, cameraWords } from './camera.ts';
import { $, type Child, h, put } from './dom.ts';
import type { AskRecord, MintRecord } from './entries.ts';
import {
  type TakeEnd,
  ASK_UNWRITTEN,
  CAMERA,
  COPY,
  HOW_IT_GOES,
  QR,
  SMALLER,
  TAKE_READING,
  TAKING_BODY,
  amountWords,
  askHint,
  askQrLabel,
  askRowWords,
  askStampLabel,
  askShareTitle,
  cancelQuestion,
  handBackLabel,
  madeWords,
  recvCap,
  takeEndWords,
  takingHeading,
  unitWords,
} from './flow-words.ts';
import { canShare, corners, doCopy, fitQR, qrWrap, rich, shareLink, sizeAmount } from './flow.ts';
import { shortDay } from './format.ts';
import { copyIcon, pasteIcon, scanIcon, shareIcon } from './icons.ts';
import { type AmountProblem, parseAmount } from './amount.ts';
import type { TakeLink } from './links.ts';
import { type Denoms, MAX_REQUESTED_NOTES, atMostOptions, split } from './plan.ts';
import { type QrPlan, qrPlan } from './qr.ts';
import { deskbar, tbSub } from './shell.ts';
import { type SliceNote, drawSlice } from './slice.ts';

/** How a take of a hand-back stands: its link read, the take on its way, or how it ended. */
export type TakeState = 'reading' | 'taking' | TakeEnd;

/** What Receive is showing and doing. */
export interface ReceiveState {
  step: 'ask' | 'request' | 'take';
  /** The amount field, as typed. */
  amount: string;
  /** Whether "Ask in smaller notes" is open, and the size chosen in it. */
  smaller: boolean;
  atMost: number | undefined;
  asking: boolean;
  error: string | undefined;
  /** The request shown. */
  ask: AskRecord | undefined;
  /** The camera or the paste field, under the request. */
  mode: 'scan' | 'paste' | undefined;
  paste: string;
  taking: boolean;
  takeError: string | undefined;
  confirmCancel: boolean;
  cancelling: boolean;
  /** A hand-back opened by its link: the take step. No link while it is read, or when it did not read. */
  take:
    | { link: TakeLink | undefined; state: TakeState; words?: string | undefined; transient?: boolean; at?: number | undefined; name: string; host: string }
    | undefined;
}

/** A fresh Receive, on the ask step with 20 in the field. */
export const blankReceive = (): ReceiveState => ({
  step: 'ask',
  amount: '20',
  smaller: false,
  atMost: undefined,
  asking: false,
  error: undefined,
  ask: undefined,
  mode: undefined,
  paste: '',
  taking: false,
  takeError: undefined,
  confirmCancel: false,
  cancelling: false,
  take: undefined,
});

/** What Receive calls when the person acts. */
export interface ReceiveActions {
  amountChanged: (text: string) => void;
  toggleSmaller: () => void;
  atMostChanged: (value: number) => void;
  ask: () => void;
  openAsk: (id: string) => void;
  newRequest: () => void;
  toggleScan: () => void;
  openPaste: () => void;
  pasteChanged: (text: string) => void;
  take: () => void;
  camera: () => void;
  cameraOff: () => void;
  askCancel: () => void;
  keepAsk: () => void;
  doCancel: () => void;
  retryTake: () => void;
}

/** Everything Receive draws from. */
export interface ReceiveInput {
  state: ReceiveState;
  /** The mint Receive asks in: the one Holdings shows (none on a take opened by a purse that holds no mint). */
  record: MintRecord | undefined;
  /** The mint the top bar shows. */
  bar: { mint: string; name: string };
  denoms: Denoms;
  /** The shown mint's open requests, newest first. */
  asks: readonly AskRecord[];
  /** The shown request's link, once encoded; and whether it could not be. */
  link: string | undefined;
  linkFailed: boolean;
  /** Whether the code's encoder is loaded: `undefined` while it loads. */
  qr: boolean | undefined;
  /** The shown mint's resting notes, for the slice. */
  resting: readonly SliceNote[];
  /** The next rotation, in words. */
  at: string;
  camera: Camera;
  /** Words for a scanned code that is not a hand-back. */
  scanError: string | undefined;
  now: number;
  actions: ReceiveActions;
}

/** Receive. */
export function receiveView(input: ReceiveInput): HTMLElement {
  const { state } = input;
  if (state.step === 'take' && state.take !== undefined) {
    return takeView(input, state.take);
  }
  if (state.step === 'request' && state.ask !== undefined) {
    return requestView(input, state.ask);
  }
  return askView(input);
}

// ---------- ask ----------

/** What the amount field reads as: its amount and notes, or what is wrong. */
interface AskRead {
  amount: number | undefined;
  problem: AmountProblem | undefined;
  notes: number[];
  options: number[];
  atMost: number | undefined;
}

function readAsk(input: ReceiveInput): AskRead {
  const parsed = parseAmount(input.state.amount);
  if (!parsed.ok) {
    return { amount: undefined, problem: parsed.problem, notes: [], options: [], atMost: undefined };
  }
  const options = atMostOptions(input.denoms, parsed.amount);
  const chosen = input.state.atMost;
  const atMost = input.state.smaller && options.length > 0 ? (chosen !== undefined && options.includes(chosen) ? chosen : options[0]) : undefined;
  const notes = split(input.denoms, parsed.amount, atMost, MAX_REQUESTED_NOTES);
  return { amount: parsed.amount, problem: undefined, notes: Array.isArray(notes) ? notes : [], options, atMost };
}

/** The size "Ask in smaller notes" would ask in now, if any: for app.ts, which asks in it. */
export function atMostFor(input: ReceiveInput): number | undefined {
  return readAsk(input).atMost;
}

function askView(input: ReceiveInput): HTMLElement {
  const { state, actions, bar } = input;
  const read = readAsk(input);
  const amount = h('input', {
    id: 'ask-amount',
    'data-fk': 'ask-amount',
    inputmode: 'numeric',
    pattern: '[0-9]*',
    autocomplete: 'off',
    maxlength: 4,
    value: state.amount,
    'aria-describedby': 'ask-split',
    style: { '--len': String(Math.max(1, state.amount.length)) },
    on: {
      input: (event) => actions.amountChanged((event.target as HTMLInputElement).value),
      focus: (event) => (event.target as HTMLInputElement).select(),
    },
  });
  const form = h(
    'form',
    {
      novalidate: true,
      on: {
        submit: (event) => {
          event.preventDefault();
          actions.ask();
        },
      },
    },
    h('h1', { class: 'lab muted h-lab', tabindex: -1, 'data-fk': 'h1' }, h('label', { for: 'ask-amount' }, 'Ask for')),
    h('div', { class: 'amount' }, amount, h('i', { id: 'ask-unit' }, unitWords(read.amount))),
    h('p', { class: 'body ask-hint', id: 'ask-split' }, hintWords(read)),
    state.error === undefined ? null : h('p', { class: 'err flow-err', role: 'alert' }, state.error),
    h('div', { class: 'smaller-row', id: 'smaller-row', 'data-sig': smallerSig(input, read) }, ...smallerRow(input, read)),
    h(
      'button',
      { class: 'btn solid wide ask-go', type: 'submit', disabled: state.asking, 'data-fk': 'ask-go' },
      state.asking ? 'Asking…' : 'Ask',
    ),
  );
  return h(
    'div',
    { class: 'view v-recv v-recv-ask v-sub' },
    deskbar(true, 'receive'),
    h(
      'main',
      { class: 'vmain' },
      h(
        'section',
        { class: 'band-p' },
        h(
          'div',
          { class: 'wrap' },
          tbSub('Receive', bar.mint, bar.name),
          form,
          h(
            'div',
            { class: 'slice-foot' },
            h('div', { class: 'field slice', id: 'slice-recv', 'aria-hidden': 'true' }),
            h('p', { class: 'slice-cap', id: 'recv-cap' }, ...rich(recvCap(read.amount, read.notes, input.at))),
          ),
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
            { class: 'c-mid' },
            input.asks.length === 0 ? null : openList(input),
            h('p', { class: 'lab muted' }, 'How it goes'),
            h(
              'ol',
              { class: 'steps3' },
              ...HOW_IT_GOES.map((step) => h('li', {}, h('i', {}, step.n), h('p', {}, h('b', {}, step.b), ' ', h('span', {}, step.rest)))),
            ),
          ),
        ),
      ),
    ),
  );
}

const hintWords = (read: AskRead): string =>
  read.amount === undefined || read.notes.length === 0 ? amountWords(read.problem ?? 'not_whole') : askHint(read.amount, read.notes);

/** What the smaller-notes row shows: redrawn only when that changes, so its select keeps focus as it is used. */
const smallerSig = (input: ReceiveInput, read: AskRead): string => `${input.state.smaller ? 1 : 0}:${read.options.join(',')}:${read.atMost ?? ''}`;

/** "Ask in smaller notes": its toggle, and its sizes when open; nothing when no smaller size exists. */
function smallerRow(input: ReceiveInput, read: AskRead): Child[] {
  const { state, actions } = input;
  if (read.options.length === 0) {
    return [];
  }
  const toggle = h(
    'button',
    {
      class: 'tbtn',
      type: 'button',
      'aria-expanded': state.smaller ? 'true' : 'false',
      // it names the sizes only while they are in the page
      'aria-controls': state.smaller ? 'smaller' : undefined,
      'data-fk': 'ask-smaller',
      on: { click: actions.toggleSmaller },
    },
    SMALLER.toggle,
  );
  if (!state.smaller) {
    return [toggle];
  }
  const select = h(
    'select',
    {
      class: 'inp',
      id: 'ask-atmost',
      'data-fk': 'ask-atmost',
      on: { change: (event) => actions.atMostChanged(Number((event.target as HTMLSelectElement).value)) },
    },
    ...read.options.map((size) => h('option', { value: size, selected: size === read.atMost }, String(size))),
  );
  return [
    toggle,
    h(
      'div',
      { class: 'smaller fld', id: 'smaller' },
      h('label', { class: 'lab', for: 'ask-atmost' }, SMALLER.label),
      h('div', { class: 'sel' }, select),
      h('p', { class: 'note-line muted' }, SMALLER.hint),
    ),
  ];
}

/** The open requests, above How it goes: each opens its request again. */
function openList(input: ReceiveInput): HTMLElement {
  return h(
    'div',
    { class: 'asks' },
    h('div', { class: 'col-h' }, h('h2', { class: 'lab' }, 'Your open requests')),
    ...input.asks.map((ask) => {
      const words = askRowWords(ask, input.now);
      return h(
        'button',
        {
          class: 'act go',
          type: 'button',
          'aria-label': words.label,
          'data-fk': `act-${ask.id.slice(0, 16)}`,
          on: { click: () => input.actions.openAsk(ask.id) },
        },
        h('time', { datetime: new Date(ask.t).toISOString() }, shortDay(ask.t, input.now)),
        h('span', {}, words.title, words.small === undefined ? null : h('small', {}, words.small)),
        h('b', {}, String(ask.amount)),
      );
    }),
  );
}

/**
 * The parts of the ask step an input changes, redrawn in place so the field
 * keeps its focus and the keyboard stays: the unit, the hint, the smaller
 * notes, the caption and the slice.
 */
export function refreshAsk(input: ReceiveInput): void {
  const read = readAsk(input);
  $('#ask-unit')?.replaceChildren(unitWords(read.amount));
  $('#ask-split')?.replaceChildren(hintWords(read));
  const cap = $('#recv-cap');
  if (cap !== null) {
    put(cap, ...rich(recvCap(read.amount, read.notes, input.at)));
  }
  const row = $('#smaller-row');
  const sig = smallerSig(input, read);
  if (row !== null && row.dataset['sig'] !== sig) {
    row.dataset['sig'] = sig;
    put(row, ...smallerRow(input, read));
  }
  const field = $<HTMLInputElement>('#ask-amount');
  if (field !== null) {
    sizeAmount(field);
  }
  drawRecvSlice(input, read);
}

/** The resting lane, with the first four notes the request would ask for as ghosts where they would land. */
function drawRecvSlice(input: ReceiveInput, read: AskRead): void {
  const host = $('#slice-recv');
  if (host === null) {
    return;
  }
  const ghosts: SliceNote[] = read.notes.slice(0, 4).map((v, i) => ({ id: `rg${i}`, v, lane: 'rest', order: 1e6 + i }));
  drawSlice(host, {
    lanes: ['rest'],
    notes: [...input.resting, ...ghosts],
    marks: new Map(ghosts.map((ghost) => [ghost.id, 'ghost' as const])),
  });
}

// ---------- request ----------

function requestView(input: ReceiveInput, ask: AskRecord): HTMLElement {
  const { state, actions, bar } = input;
  const link = input.link;
  // never a code or a link of nothing: the code waits for the link, and a link that could not be written says so
  let code: HTMLElement;
  if (link !== undefined) {
    code = codeStamp(input, link, askQrLabel(ask.amount), askStampLabel(ask.amount, bar.name), QR.denseAsk);
  } else if (input.linkFailed) {
    code = h('div', { class: 'c-mid' }, h('p', { class: 'body', role: 'alert' }, ASK_UNWRITTEN));
  } else {
    code = codeStamp({ qr: undefined }, '', askQrLabel(ask.amount), askStampLabel(ask.amount, bar.name), QR.denseAsk);
  }
  return h(
    'div',
    { class: 'view v-recv v-sub flow' },
    deskbar(true, 'receive'),
    h(
      'main',
      { class: 'vmain' },
      h(
        'section',
        { class: 'band-p' },
        h(
          'div',
          { class: 'wrap' },
          tbSub('Receive', bar.mint, bar.name),
          h(
            'div',
            { class: 'ask-row' },
            h(
              'h1',
              { tabindex: -1, 'data-fk': 'h1' },
              h('span', { class: 'lab muted' }, 'Ask for'),
              h('span', { class: 'ask-amt' }, h('b', {}, String(ask.amount)), h('i', {}, unitWords(ask.amount))),
            ),
            h('button', { class: 'tbtn', type: 'button', 'data-fk': 'ask-new', on: { click: actions.newRequest } }, 'New request'),
          ),
          h('p', { class: 'body recv-made' }, madeWords(ask.t, ask.notes)),
        ),
      ),
      h(
        'section',
        { class: 'band-c band-req' },
        h(
          'div',
          { class: 'wrap' },
          code,
          h(
            'div',
            { class: 'c-low' },
            link === undefined ? null : linkRow(link, 'ask-link', COPY.ask, COPY.shareAsk, askShareTitle(ask.amount), 'copy-ask', 'share-ask'),
            h(
              'div',
              { class: 'pair' },
              h(
                'button',
                { class: 'btn', type: 'button', 'aria-pressed': state.mode === 'scan' ? 'true' : 'false', 'data-fk': 'recv-scan', on: { click: actions.toggleScan } },
                scanIcon(),
                'Scan their hand-back',
              ),
              h(
                'button',
                { class: 'btn', type: 'button', 'aria-pressed': state.mode === 'paste' ? 'true' : 'false', 'data-fk': 'recv-paste', on: { click: actions.openPaste } },
                pasteIcon(),
                'Paste their link',
              ),
            ),
            ...(state.mode === 'scan' ? scanner(input, 'on-c sm', CAMERA.or, 'recv-panel') : []),
            state.mode === 'scan' && input.scanError !== undefined ? h('p', { class: 'err flow-err', role: 'alert' }, input.scanError) : null,
            state.mode === 'paste' ? pasteForm(input) : null,
            cancelBlock(input, ask),
          ),
        ),
      ),
    ),
  );
}

/**
 * A request's or a hand-back's code in its perforated stamp, with the
 * stamp's label: a dense code gets a word under it, and one too large for
 * a code gets the words in its place.
 */
export function codeStamp(input: { qr: boolean | undefined }, link: string, qrLabel: string, label: string, dense: string, extra = ''): HTMLElement {
  let plan: QrPlan | undefined;
  if (input.qr === true) {
    plan = qrPlan(link);
  }
  let parts: Child[];
  if (input.qr === undefined) {
    parts = [h('div', { class: 'qr-wrap' }, h('div', { class: 'qr-wait' }))];
  } else if (plan?.kind === 'none') {
    parts = [h('p', { class: 'qr-none' }, QR.none)];
  } else {
    parts = [qrWrap(link, qrLabel), plan?.kind === 'dense' ? h('p', { class: 'qr-note' }, dense) : null];
  }
  return h('div', { class: `stamp qstamp c-mid${extra === '' ? '' : ` ${extra}`}` }, ...parts, h('span', { class: 'lab' }, label));
}

/** A link in a row, with its copy button, and a share button where the browser has a share sheet. */
export function linkRow(link: string, id: string, copyLabel: string | undefined, shareLabel: string, shareTitle: string, copyKey: string, shareKey: string): HTMLElement {
  const code = h('code', { id }, link);
  return h(
    'div',
    { class: 'linkrow' },
    code,
    copyLabel === undefined
      ? null
      : h(
          'button',
          {
            class: 'ib',
            type: 'button',
            'aria-label': copyLabel,
            'data-fk': copyKey,
            on: { click: (event) => doCopy(event.currentTarget as HTMLElement, link, code) },
          },
          copyIcon(),
        ),
    canShare()
      ? h('button', { class: 'ib', type: 'button', 'aria-label': shareLabel, 'data-fk': shareKey, on: { click: () => shareLink(shareTitle, link) } }, shareIcon())
      : null,
  );
}

/**
 * The viewfinder: its corners, and the camera once it runs, with the words
 * for where the camera is; before it starts, the button that starts it,
 * described by those words. The words are said in the page's live region
 * as the camera's state changes (app.ts), not here: this is drawn anew.
 */
export function scanner(input: { camera: Camera; actions: { camera: () => void; cameraOff: () => void } }, cls: string, or: string, id?: string): Child[] {
  const { camera, actions } = input;
  const running = camera.kind === 'running';
  const words = cameraWords(camera);
  const wordsId = `${id ?? 'scan'}-words`;
  const start =
    camera.kind === 'idle' || camera.kind === 'refused' || camera.kind === 'starting'
      ? h(
          'button',
          {
            class: 'btn sm',
            type: 'button',
            disabled: camera.kind === 'starting',
            'aria-describedby': words === '' ? undefined : wordsId,
            'data-fk': 'cam',
            on: { click: actions.camera },
          },
          scanIcon(),
          h('span', {}, camera.kind === 'refused' ? CAMERA.again : CAMERA.on),
        )
      : null;
  const box = h(
    'div',
    { class: `scanner ${cls}${running ? ' live' : ''}`, id },
    corners(),
    running
      ? h('p', { id: wordsId }, words)
      : h(
          'div',
          { class: 'stack' },
          words === '' ? null : h('p', { id: wordsId }, words),
          start,
          words === '' ? h('p', { class: camera.kind === 'idle' ? 'muted' : undefined }, or) : null,
        ),
  );
  return [
    box,
    running
      ? h('button', { class: 'tbtn cam-toggle', type: 'button', 'data-fk': 'cam-toggle', on: { click: actions.cameraOff } }, CAMERA.off)
      : null,
  ];
}

function pasteForm(input: ReceiveInput): HTMLElement {
  const { state, actions } = input;
  return h(
    'form',
    {
      class: 'take-form',
      id: 'recv-panel',
      on: {
        submit: (event) => {
          event.preventDefault();
          actions.take();
        },
      },
    },
    h(
      'div',
      { class: 'fld' },
      h('label', { class: 'lab', for: 'take-link' }, 'Their hand-back link'),
      h(
        'div',
        { class: 'paste-row' },
        h('input', {
          class: 'inp mono',
          id: 'take-link',
          'data-fk': 'take-link',
          placeholder: 'Paste their hand-back link',
          autocomplete: 'off',
          autocapitalize: 'off',
          spellcheck: 'false',
          value: state.paste,
          on: { input: (event) => actions.pasteChanged((event.target as HTMLInputElement).value) },
        }),
        h('button', { class: 'btn solid', type: 'submit', disabled: state.taking, 'data-fk': 'take-go' }, state.taking ? 'Taking…' : 'Take'),
      ),
    ),
    state.takeError === undefined ? null : h('p', { class: 'err flow-err', role: 'alert' }, state.takeError),
  );
}

/** Cancel this request, and the question it asks first. */
function cancelBlock(input: ReceiveInput, ask: AskRecord): HTMLElement {
  const { state, actions } = input;
  if (!state.confirmCancel) {
    return h(
      'div',
      { class: 'cancel-low' },
      h('button', { class: 'tbtn on-c', type: 'button', 'data-fk': 'cancel-ask', on: { click: actions.askCancel } }, 'Cancel this request'),
    );
  }
  return h(
    'div',
    { class: 'cancel-ask', role: 'group', 'aria-labelledby': 'cancel-q' },
    h('p', { id: 'cancel-q' }, cancelQuestion(ask.amount)),
    state.error === undefined ? null : h('p', { class: 'err flow-err', role: 'alert' }, state.error),
    h(
      'div',
      { class: 'btn-row' },
      h('button', { class: 'btn', type: 'button', disabled: state.cancelling, 'data-fk': 'keep-ask', on: { click: actions.keepAsk } }, 'Keep it'),
      h(
        'button',
        { class: 'btn solid', type: 'button', disabled: state.cancelling, 'data-fk': 'do-cancel', on: { click: actions.doCancel } },
        state.cancelling ? 'Cancelling…' : 'Cancel it',
      ),
    ),
  );
}

// ---------- take ----------

function takeView(input: ReceiveInput, take: NonNullable<ReceiveState['take']>): HTMLElement {
  const { bar } = input;
  const amount = take.link?.amount ?? 0;
  const ended =
    take.state === 'reading' || take.state === 'taking'
      ? undefined
      : takeEndWords(take.state, { amount, at: take.at, now: input.now, host: take.host, words: take.words });
  const busy = ended === undefined;
  const back = h('a', { class: 'btn solid', href: '#home' }, 'Back to the purse');
  let buttons: Child[] = [];
  if (take.state === 'failed' && take.transient === true && take.link !== undefined) {
    buttons = [h('button', { class: 'btn', type: 'button', 'data-fk': 'retry', on: { click: input.actions.retryTake } }, 'Try again'), back];
  } else if (!busy) {
    buttons = [back];
  }
  return h(
    'div',
    { class: 'view v-recv v-sub flow low v-take' },
    deskbar(true, 'receive'),
    h(
      'main',
      { class: 'vmain', 'aria-busy': busy ? 'true' : undefined },
      h(
        'section',
        { class: 'band-p' },
        h(
          'div',
          { class: 'wrap' },
          tbSub('Receive', bar.mint, bar.name),
          // what it carries, once its link is read: a link that did not read carries nothing to name
          take.link === undefined ? null : h('p', { class: 'lab muted mt-10' }, handBackLabel(amount, take.name)),
          h(
            'h1',
            { class: `h1 ${take.link === undefined ? 'mt-10' : 'mt-8'}`, tabindex: -1, 'data-fk': 'h1' },
            ...rich(ended?.h1 ?? (take.state === 'reading' ? TAKE_READING : takingHeading(amount))),
          ),
          h('p', { class: 'body mt-12', role: busy ? 'status' : undefined }, ended?.body ?? TAKING_BODY),
        ),
      ),
      h('section', { class: 'band-c' }, h('div', { class: 'wrap' }, h('div', { class: 'c-low' }, buttons.length === 0 ? null : h('div', { class: 'btn-row' }, ...buttons)))),
    ),
  );
}

// ---------- once in the page ----------

/** What the view needs once it is in the page: the field's slice, the amount's width, the code's size, the camera. */
export function mountReceive(input: ReceiveInput): void {
  const { state } = input;
  if (state.step === 'ask') {
    const field = $<HTMLInputElement>('#ask-amount');
    if (field !== null) {
      sizeAmount(field);
    }
    drawRecvSlice(input, readAsk(input));
  }
  if (state.step === 'request') {
    fitQR();
    const box = $('#recv-panel.scanner');
    if (box !== null) {
      attachCamera(box);
    }
  }
}
