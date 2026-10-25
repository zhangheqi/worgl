// The app: its state, its four routes, what the person's actions do, what
// keeps Holdings current while it is open, and keep-alive's wiring: when
// the keeper runs, what other tabs hear of it, and how a run shows. The
// views (first.ts, home.ts, receive.ts, send.ts) and the field (field.ts)
// are drawn from the state on every change; the SDK is reached only through
// purses.ts, which is loaded once the shell has painted.

import type { FaucetInfo, MintInfo } from 'worgl';

import { parseAmount } from './amount.ts';
import { askAfterTake, newestOpen, openAsks } from './asks.ts';
import { type Camera, cameraWords, startCamera, stopCamera } from './camera.ts';
import { $, announce, reduced } from './dom.ts';
import { type Said, linkWords, notCancelled, notRecorded, say, transient } from './errors.ts';
import {
  type FieldInput,
  type Opened,
  arrive,
  bindPointer,
  dismissHover,
  drawMain,
  fieldTook,
  hop as hopNote,
  moveNeedle,
  order,
} from './field.ts';
import { type FirstInput, type FirstState, firstView } from './first.ts';
import { fitPre, fitQR } from './flow.ts';
import {
  amountWords,
  cancelledWords,
  linkProblemWords,
  otherAskWords,
  progressWords,
  receivedWords,
  sentWords,
  serverNote,
  settledWords,
  takeEndWords,
} from './flow-words.ts';
import {
  type Arrival,
  type Holdings,
  type LaneKey,
  type LastFade,
  arrivalsOf,
  comeBackAt,
  fadedAt,
  firstFade,
  holdingsFrom,
  laneOf,
  laneTotal,
  savedHoldings,
} from './holdings.ts';
import { type Elsewhere, type HomeData, type HomeInput, type KeepShown, homeView, placeholder, renderBack } from './home.ts';
import type { Moved } from './keep.ts';
import { Keeper, type KeptServer, Keeping, type Shown } from './keeper.ts';
import { withTransfersLock } from './lock.ts';
import { type MintLink, hostOf, isMintHash, mintHash, parseMintLink } from './link.ts';
import { type AskLink, type TakeLink, decode, encodeAsk, encodeTake, linkFor } from './links.ts';
import { holdersOf, readNote, valueOf } from './notes.ts';
import { type Denoms, type Planned, type Policy, type Spendable, droppedValue, plan, shortCase, shown as shownPlan } from './plan.ts';
import { placePostmark } from './postmark.ts';
import type { About, Came, ForSend, Kept, Read, ReadRequest, Sent, Settled } from './purses.ts';
import { loadQr, qrReady } from './qr.ts';
import { type ReceiveInput, type ReceiveState, atMostFor, blankReceive, mountReceive, receiveView, refreshAsk } from './receive.ts';
import { type AskRecord, type LogEntry, type MintRecord, Records, type SavedInfo } from './records.ts';
import {
  type HandBack,
  type Run,
  type SendInput,
  type SendState,
  blankSend,
  elapsed,
  mountSend,
  refreshSending,
  sendView,
  setLastLayer,
  tickSending,
} from './send.ts';
import type { SliceNote } from './slice.ts';
import { keptWords, rotatedWords, rotationWords, tookWords, unreachableFor } from './words.ts';

type Sdk = typeof import('./purses.ts');

/** The four views. */
type Route = 'first' | 'home' | 'receive' | 'send';

/** What Holdings shows and what it is doing. */
interface HomeState {
  /** The mint `data` is of: the one shown when it was read. */
  mint: string | undefined;
  data: HomeData | undefined;
  /** The mint's rotations so far at the last read, to tell when it rotates while the purse is open. */
  oldest: bigint | undefined;
  error: { words: string; at: 'read' | 'take' } | undefined;
  opened: Opened;
  delta: { v: number; t: number } | undefined;
  /** Notes that just came, by when: pink for six seconds. */
  pink: Map<string, number>;
  taking: boolean;
  /** Notes just taken, which the next draw slides in. */
  arriving: string[];
  /** A take to announce once the read after it is drawn: from the faucet, or a hand-back taken in. */
  took: { amount: number; ids: string[]; received?: boolean; also?: string | undefined } | undefined;
  /** Tokens just taken in from a hand-back: the postmark and the sentence's clause, until the person leaves Holdings. */
  received: { amount: number; t: number; lane: LaneKey | undefined } | undefined;
  /** What the last read drew, so that a read that finds nothing new draws nothing (and moves no focus). */
  drawn: string | undefined;
  /** The same, for the flows' scan and ask steps, which show what the shown mint holds. */
  sideDrawn: string | undefined;
  /** The error words last drawn: drawn again, they are not an alert again, so a screen reader says them once. */
  alerted: string | undefined;
}

/** A keep-alive run, as Holdings shows it. */
interface KeepState {
  /** The shown mint's run in this tab: its hops' value, how many are done, of how many. */
  progress: Shown | undefined;
  /** Another tab's run of the shown mint, as it last said, and when. */
  watching: { amount: number; done: number; total: number; at: number } | undefined;
  /** The shown mint's hops answered, by the note each spent, until a read shows the note it brought. */
  hops: Map<string, { v: number; generation: bigint; at: number }>;
  /** Notes keep-alive just moved a layer on, by when: their thread, and their place until the run's log entry is in. */
  hopped: Map<string, number>;
  /** The layers a run's hops pass through, kept open from its first moving note until the notes change after it. */
  lanes: Set<number>;
  /** The notes as the run left them; undefined while it runs. */
  lanesSig: string | undefined;
}

/** A payment the purse keeps with its hand-back, and the log entry of its send. */
interface KeptHandBack {
  server: string;
  kept: Kept;
  entry: LogEntry | undefined;
}

const blankFirst = (): FirstState => ({
  link: undefined,
  about: undefined,
  reading: false,
  pasting: false,
  paste: '',
  busy: undefined,
  error: undefined,
});

const state = {
  route: 'first' as Route,
  records: undefined as Records | undefined,
  /** Why the purse's records did not open, if they did not. */
  storeError: undefined as string | undefined,
  mints: [] as MintRecord[],
  log: [] as LogEntry[],
  asks: [] as AskRecord[],
  /** Every payment kept with owed notes or still on its way, across servers, newest first. */
  handBacks: [] as KeptHandBack[],
  /** The build's playground, when the build names one. */
  playground: undefined as MintLink | undefined,
  standalone: false,
  first: blankFirst(),
  home: {
    mint: undefined,
    data: undefined,
    oldest: undefined,
    error: undefined,
    opened: undefined,
    delta: undefined,
    pink: new Map(),
    taking: false,
    arriving: [],
    took: undefined,
    received: undefined,
    drawn: undefined,
    sideDrawn: undefined,
    alerted: undefined,
  } as HomeState,
  receive: blankReceive() as ReceiveState,
  send: blankSend() as SendState,
  /** Each mint's denominations, once its info is read: Receive's sizes of note. */
  policies: new Map<string, Denoms>(),
  /** Each request's link, by its id, once encoded; and the requests whose link could not be written here. */
  links: new Map<string, string>(),
  unwritten: new Set<string>(),
  camera: { kind: 'idle' } as Camera,
  /** Words for a scanned code that is not what the view looks for. */
  scanError: undefined as string | undefined,
  keep: {
    progress: undefined,
    watching: undefined,
    hops: new Map(),
    hopped: new Map(),
    lanes: new Set(),
    lanesSig: undefined,
  } as KeepState,
};

/** What Send read of the request's mint before it planned: kept for the check under the lock and the slices. */
let forSend: { fs: ForSend; holdings: Holdings; link: string } | undefined;

/** The mint Holdings shows: the one last used (added, taken from, asked in, sent from or taken into). */
const shown = (): MintRecord | undefined =>
  state.mints.reduce<MintRecord | undefined>(
    (best, record) => (best === undefined || (record.used ?? record.added) > (best.used ?? best.added) ? record : best),
    undefined,
  );

const isPlayground = (link: MintLink): boolean =>
  state.playground !== undefined && state.playground.mint === link.mint && state.playground.server === link.server;

/** The purse's own address, which links are opened at. */
const base = (): string => `${location.origin}${location.pathname}`;

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

const sum = (values: readonly number[]): number => values.reduce((a, b) => a + b, 0);

// ---------- the SDK, loaded after the first paint ----------

let sdk: Promise<Sdk> | undefined;

/** The SDK side of the purse: importing it fetches and compiles the wasm, once. */
function loadSdk(): Promise<Sdk> {
  if (sdk === undefined) {
    const loading = import('./purses.ts');
    sdk = loading;
    loading.catch(() => {
      sdk = undefined;
    });
  }
  return sdk;
}

/** Forgets the open SDK purses, if the SDK is loaded, so the next call reads each store again. */
function reopen(): void {
  void sdk?.then((loaded) => loaded.reopen(), () => undefined);
}

/** Acts on what an error asks for beyond its words. */
function heed(said: Said): void {
  if (said.reopen === true) {
    reopen();
  }
}

/** Aborted when the person leaves the page: the only thing that cuts a send short. */
let page = new AbortController();

// ---------- the QR encoder, loaded on the first view with a code ----------

let qr: boolean | undefined;
let qrLoading = false;

/** Whether the code's encoder is loaded: `undefined` while it loads. */
function qrState(): boolean | undefined {
  if (qr === undefined && qrReady()) {
    qr = true;
  }
  if (qr === undefined && !qrLoading) {
    qrLoading = true;
    void loadQr().then((ok) => {
      qr = ok;
      const step = state.send.step;
      if ((state.route === 'receive' && state.receive.step === 'request') || (state.route === 'send' && step !== 'sending' && step !== 'scan')) {
        render();
      }
    });
  }
  return qr;
}

// ---------- drawing ----------

let observer: ResizeObserver | undefined;

/** The shown mint changed: what was read of the last one is not this one's. */
function syncShown(): void {
  const mint = shown()?.mint;
  if (mint !== state.home.mint) {
    state.home.mint = mint;
    state.home.data = undefined;
    state.home.oldest = undefined;
    state.home.opened = undefined;
    state.home.drawn = undefined;
    state.home.sideDrawn = undefined;
  }
}

/** Draws the current view, keeping focus, a text field's selection and the scroll where they were. */
function render(): void {
  const app = document.getElementById('app');
  if (app === null) {
    return;
  }
  syncShown();
  const active = document.activeElement;
  const key = active instanceof HTMLElement || active instanceof SVGElement ? active.dataset['fk'] : undefined;
  // on Holdings drawn again, where focus goes when what had it is not drawn: a number whose notes all moved, a tip that closed
  const again = state.route === 'home' && document.documentElement.dataset['view'] === 'home';
  const near = key === undefined || !again ? [] : nearKeys(key, active);
  const selection =
    active instanceof HTMLInputElement ? ([active.selectionStart, active.selectionEnd] as const) : undefined;
  const y = window.scrollY;
  const record = shown();
  let view: HTMLElement;
  let receive: ReceiveInput | undefined;
  let send: SendInput | undefined;
  if (state.route === 'home' && record !== undefined) {
    view = homeView(homeInput(record));
  } else if (state.route === 'receive') {
    receive = receiveInput();
    view = receiveView(receive);
  } else if (state.route === 'send') {
    send = sendInput();
    view = sendView(send);
  } else {
    view = firstView(firstInput());
  }
  app.replaceChildren(view);
  document.documentElement.dataset['view'] = state.route;
  // the camera runs only while a viewfinder shows
  if (state.camera.kind !== 'idle' && app.querySelector('.scanner') === null) {
    stopCamera();
    state.camera = { kind: 'idle' };
  }
  if (state.route === 'home' && record !== undefined) {
    mountField();
    const pm = $('.pm-wrap');
    if (pm !== null) {
      placePostmark(pm);
    }
    state.home.drawn = shownSignature(Date.now());
    state.home.alerted = homeError(record)?.words;
  }
  if (receive !== undefined) {
    mountReceive(receive);
    state.home.sideDrawn = shownSignature(Date.now());
  }
  if (send !== undefined) {
    mountSend(send);
    state.home.sideDrawn = shownSignature(Date.now());
    if (state.send.step === 'sending') {
      startTear();
    }
  }
  if (key !== undefined) {
    const el = [key, ...near]
      .map((each) => app.querySelector<HTMLElement>(`[data-fk="${CSS.escape(each)}"]`))
      .find((each) => each !== null && each.getClientRects().length > 0);
    if (el !== undefined && el !== null) {
      el.focus({ preventScroll: true });
      if (selection !== undefined && el instanceof HTMLInputElement) {
        try {
          el.setSelectionRange(selection[0], selection[1]);
        } catch {
          // not a text field
        }
      }
    }
  }
  if (Math.abs(window.scrollY - y) > 1) {
    window.scrollTo(0, y);
  }
}

/**
 * Where focus goes, in order, when the element with `key` is not drawn
 * again: a number in the sentence goes to its lane's label, a tip's own
 * button to the label of the lane the tip is about; else the heading.
 */
function nearKeys(key: string, active: Element | null): string[] {
  const lane = /^num-(fade|ready|rest)$/.exec(key)?.[1];
  if (lane !== undefined) {
    return [`lane-${lane}`, 'h1'];
  }
  if (active?.closest('.f-tip') != null) {
    // the field marks the lane its open tip is about, as it was drawn
    const of = $('#field')?.dataset['sel'];
    return of === undefined ? ['h1'] : [`lane-${of}`, 'h1'];
  }
  return ['h1'];
}

function firstInput(): FirstInput {
  return {
    state: state.first,
    records: state.mints,
    playground: state.playground,
    standalone: state.standalone,
    actions: {
      take: () => void takeFirst(),
      add: () => void addFirst(),
      openPaste,
      pasteChanged: (text) => {
        state.first.paste = text;
      },
      submitPaste,
    },
  };
}

/** The ids of the requests whose hand-back the purse still keeps. */
const keptIds = (): Set<string> => new Set(state.handBacks.filter((each) => each.kept.owed.length > 0).map((each) => each.kept.id));

function homeInput(record: MintRecord): HomeInput {
  const opened = state.home.opened;
  return {
    record,
    data: state.home.data,
    saved: savedShown(record, Date.now()),
    log: state.log.filter((entry) => entry.mint === record.mint),
    error: homeError(record),
    alert: homeError(record)?.words !== state.home.alerted,
    keeps: keeps(record),
    keep: keepShown(),
    elsewhere: elsewhere(record, Date.now()),
    openLane: opened !== undefined && 'lane' in opened ? opened.lane : undefined,
    delta: state.home.delta,
    taking: state.home.taking,
    received: state.home.received,
    kept: keptIds(),
    now: Date.now(),
    take: () => void takeHome(),
    toggleLane,
    openRow,
  };
}

function fieldInput(record: MintRecord): FieldInput {
  return {
    holdings: state.home.data?.holdings ?? placeholder(record),
    mintName: record.name,
    opened: state.home.opened,
    pink: state.home.pink,
    keeps: keeps(record),
    busy: state.keep.lanes,
    hopped: state.keep.hopped,
    toggleLane,
    toggleNote,
    showNote,
    close: () => closeTip(true),
  };
}

/** Draws the field into the view just drawn, and redraws it when its width changes. */
function mountField(): void {
  const host = $('#field');
  const record = shown();
  if (host === null || record === undefined) {
    return;
  }
  drawMain(host, fieldInput(record));
  bindPointer(host);
  observer?.disconnect();
  let width = host.clientWidth;
  observer = new ResizeObserver(() => {
    if (Math.abs(host.clientWidth - width) > 1) {
      width = host.clientWidth;
      redrawField();
    }
  });
  observer.observe(host);
}

/** Draws the field again, as it is: after a resize, or once the fonts are in. */
function redrawField(): void {
  const host = $('#field');
  const record = shown();
  if (host !== null && record !== undefined && state.route === 'home') {
    drawMain(host, fieldInput(record));
  }
  const pm = $('.pm-wrap');
  if (pm !== null) {
    placePostmark(pm);
  }
}

/** Focus goes to the new view's heading after a step of the person's, never on boot. */
function focusH1(): void {
  $('#app [data-fk="h1"]')?.focus({ preventScroll: true });
}

/** Focuses the control `key` names, if it is drawn. */
function focusKey(key: string): void {
  $<HTMLElement>(`#app [data-fk="${key}"]`)?.focus({ preventScroll: true });
}

// ---------- the flows' slices ----------

/** Notes as the slices draw them: the Ready lane (fading notes in it, dissolving) and the Resting lane. */
function sliceNotes(holdings: Holdings): SliceNote[] {
  return order(holdings.notes).map((placed) => {
    const lane = laneOf(placed.pos, holdings.layers);
    return { id: placed.id, v: placed.v, lane: lane === 'rest' ? 'rest' : 'ready', fading: lane === 'fade', order: placed.order };
  });
}

/** The next rotation, in words: when the resting notes are ready. */
const atWords = (holdings: Holdings | undefined): string =>
  holdings === undefined ? 'the next rotation' : rotationWords(holdings, 0, Date.now());

// ---------- tips ----------

function toggleLane(lane: LaneKey): void {
  const opened = state.home.opened;
  state.home.opened = opened !== undefined && 'lane' in opened && opened.lane === lane ? undefined : { lane };
  render();
}

function toggleNote(id: string): void {
  const opened = state.home.opened;
  state.home.opened = opened !== undefined && 'note' in opened && opened.note === id ? undefined : { note: id };
  render();
}

function showNote(id: string): void {
  state.home.opened = { note: id };
  render();
}

/** Closes the open tip; from the keyboard, focus goes back to its lane's label. */
function closeTip(refocus: boolean): void {
  const opened = state.home.opened;
  if (opened === undefined) {
    return;
  }
  const holdings = state.home.data?.holdings;
  let lane: LaneKey | undefined;
  if ('lane' in opened) {
    lane = opened.lane;
  } else {
    const note = holdings?.notes.find((each) => each.id === opened.note);
    lane = note === undefined || holdings === undefined ? undefined : laneOf(note.pos, holdings.layers);
  }
  const fromTip = document.activeElement?.closest('.f-tip') != null;
  state.home.opened = undefined;
  render();
  if ((refocus || fromTip) && lane !== undefined) {
    ($(`[data-fk="lane-${lane}"]`) ?? $(`[data-fk="num-${lane}"]`))?.focus({ preventScroll: true });
  }
}

// ---------- routes ----------

/**
 * Whether a flow shows something that needs no mint of the purse's own: a
 * request for a mint it does not hold, a request link that did not read
 * (with its words), or a hand-back.
 */
const flowOpen = (route: Route): boolean =>
  (route === 'send' && (state.send.step !== 'scan' || state.send.error !== undefined)) || (route === 'receive' && state.receive.step === 'take');

/** The view a hash asks for: Holdings, Receive or Send once the purse holds a mint, else the first run. */
function routeFor(hash: string): Route {
  if (hash === '#first' || shown() === undefined) {
    return 'first';
  }
  if (hash === '#receive') {
    return 'receive';
  }
  if (hash === '#send') {
    return 'send';
  }
  return 'home';
}

/** Whether a hash carries a request or a hand-back. */
const flowHash = (hash: string): boolean => /^#(ask|take)=/.test(hash);

/** Receive, as it opens: on the shown request still, on an open request made in the last half hour, or on the ask. */
function openingReceive(): void {
  const record = shown();
  const r = state.receive;
  if (record === undefined) {
    state.receive = blankReceive();
    return;
  }
  const current = r.step === 'request' && r.ask !== undefined ? state.asks.find((each) => each.id === r.ask?.id) : undefined;
  if (current !== undefined && current.state === 'open' && current.mint === record.mint) {
    state.receive = { ...r, ask: current, mode: undefined, confirmCancel: false, cancelling: false, error: undefined, takeError: undefined };
    return;
  }
  const newest = newestOpen(state.asks, record.mint, Date.now());
  state.receive = newest === undefined ? blankReceive() : requestState(newest);
}

/** The send this tab is making or made last, whichever screen shows: it stays reachable from Send and Recent. */
let liveSend: SendState | undefined;

/** Send, as it opens: a send in progress and an unseen hand-back stay; anything else starts afresh. */
function openingSend(): void {
  const s = liveSend ?? state.send;
  if (s.step === 'sending' && s.run !== undefined) {
    state.send = s;
    return;
  }
  if (s.step === 'handback' && s.unseen) {
    s.unseen = false;
    state.send = s;
    return;
  }
  state.send = blankSend();
  forSend = undefined;
}

/**
 * Shows `route`: the hash is replaced, not pushed; a new view scrolls to the
 * top and, after a step of the person's, takes focus at its heading. With
 * `keep`, the flow shows as the caller set it, not as it opens.
 */
function setRoute(route: Route, options: { focus?: boolean; hash?: string; keep?: boolean } = {}): void {
  if (route !== state.route && options.keep !== true) {
    if (route === 'receive') {
      openingReceive();
    } else if (route === 'send') {
      openingSend();
    }
  }
  const next: Route = route !== 'first' && shown() === undefined && !flowOpen(route) ? 'first' : route;
  const changed = next !== state.route;
  if (changed && next === 'home') {
    state.first = blankFirst();
  }
  if (changed && state.route === 'home') {
    // the received postmark shows until the person leaves Holdings
    state.home.received = undefined;
  }
  if (changed) {
    state.scanError = undefined;
    // the camera is for the view that started it: leaving the route turns it off, though the next view has a viewfinder too
    if (state.camera.kind !== 'idle') {
      stopCamera();
      state.camera = { kind: 'idle' };
    }
  }
  state.route = next;
  state.home.opened = undefined;
  const hash = options.hash ?? `#${next}`;
  if (location.hash !== hash) {
    history.replaceState(null, '', hash);
  }
  render();
  if (changed) {
    window.scrollTo(0, 0);
    if (options.focus !== false) {
      focusH1();
    }
  }
  if (next === 'home') {
    void refreshHome();
  } else {
    clearTimeout(rotationTimer);
    if (next === 'receive' || next === 'send') {
      // the flows show what the shown mint holds: read it, if it is not yet
      void refreshHome();
    }
  }
  const record = shown();
  if (next === 'receive' && record !== undefined) {
    loadPolicy(record);
  }
}

/** Follows an in-page link or a hash the person typed. */
function navigate(hash: string): void {
  if (flowHash(hash)) {
    openFlow(hash);
  } else if (isMintHash(hash)) {
    openLink(hash, true);
  } else {
    setRoute(routeFor(hash));
  }
}

/**
 * A request or a hand-back opened by its link: its flow shows at once,
 * reading it, and the address bar says #send or #receive before anything
 * is read, so a reload never sends or takes again and the payload does not
 * linger in the tab's history. On boot, `focus` is false.
 */
function openFlow(text: string, focus = true): void {
  if (/^#?take=/.test(text.trim())) {
    stopCamera();
    state.camera = { kind: 'idle' };
    state.receive = { ...blankReceive(), step: 'take', take: { link: undefined, state: 'reading', name: '', host: '' } };
    setRoute('receive', { hash: '#receive', keep: true, focus });
    void takeText(text, 'link', focus);
  } else {
    state.send = { ...blankSend(), step: 'reading' };
    forSend = undefined;
    ticket = '';
    setRoute('send', { hash: '#send', keep: true, focus });
    void openAsk(text, 'link', focus);
  }
}

// ---------- the first run ----------

/** Opens the first run on a mint link: read the mint, or say what is wrong with the link. */
function openLink(text: string, focus: boolean): void {
  const parsed = parseMintLink(text);
  if (!parsed.ok) {
    state.first = { ...blankFirst(), error: linkWords(parsed.problem) };
    setRoute('first', { focus });
    return;
  }
  state.first = blankFirst();
  readLink(parsed.link, focus);
}

/** A link others can open again: the purse's own address with the mint after `#`. */
const shareable = (link: MintLink): string => `${base()}${mintHash(link)}`;

/**
 * Reads the mint a link names through the SDK, then shows its stamp. A mint
 * this purse holds at that server already, with no faucet to take from,
 * goes straight to Holdings.
 */
function readLink(link: MintLink, focus: boolean): void {
  const pasted = state.first.paste !== '';
  state.first = { ...blankFirst(), link, reading: true, paste: state.first.paste };
  setRoute('first', { focus, hash: mintHash(link) });
  void (async () => {
    let about: About;
    try {
      about = await (await loadSdk()).readMint(link);
    } catch (error) {
      if (state.first.link !== link) {
        return;
      }
      state.first = {
        ...state.first,
        reading: false,
        error: say(error, 'read', { host: hostOf(link.server) }).words,
        // the link stays in the paste row, so that Add tries it again
        paste: state.first.paste === '' ? shareable(link) : state.first.paste,
      };
      render();
      return;
    }
    if (state.first.link !== link) {
      return;
    }
    const held = state.mints.find((record) => record.mint === link.mint);
    if (held !== undefined && held.server === link.server && about.faucet === undefined) {
      // Holdings shows the mint last used: the one this link names, now
      await setUsed(held.mint);
      setRoute('home', { focus });
      return;
    }
    state.first = { ...state.first, about, reading: false };
    render();
    if (pasted && (document.activeElement === document.body || document.activeElement === null)) {
      $('#app .first-acts .btn')?.focus({ preventScroll: true });
    }
  })();
}

/** Reads the build's playground, for state A; if it does not answer, the paste row shows instead. */
function readPlayground(link: MintLink): void {
  state.first = { ...blankFirst(), reading: true };
  void (async () => {
    try {
      const about = await (await loadSdk()).readMint(link);
      if (state.first.link === undefined && state.first.reading) {
        state.first = { ...state.first, about, reading: false };
      }
    } catch {
      if (state.first.link === undefined) {
        state.first = { ...state.first, reading: false };
      }
    }
    if (state.route === 'first') {
      render();
    }
  })();
}

function openPaste(): void {
  state.first.pasting = true;
  render();
  $<HTMLInputElement>('#first-link')?.focus();
}

function submitPaste(): void {
  const parsed = parseMintLink(state.first.paste);
  if (!parsed.ok) {
    state.first = { ...state.first, error: linkWords(parsed.problem) };
    render();
    $<HTMLInputElement>('#first-link')?.focus();
    return;
  }
  readLink(parsed.link, true);
}

/**
 * What the purse keeps of a mint's info: enough to draw its lanes before it
 * answers, and what fades first, to say when it does not.
 */
const savedInfo = (info: MintInfo, fades: LastFade | null): SavedInfo => ({
  layers: info.layers,
  periodSecs: info.rotation === undefined ? undefined : Number(info.rotation.periodSecs),
  paused: info.paused,
  readAt: Date.now(),
  fades,
});

/** Adds a mint to the purse's records. */
async function saveMint(link: MintLink, about: About): Promise<void> {
  const records = state.records;
  if (records === undefined) {
    throw new DOMException('the purse has no records in this browser', 'NotSupportedError');
  }
  const now = Date.now();
  await records.putMint({
    mint: link.mint,
    server: link.server,
    name: isPlayground(link) ? 'Playground' : hostOf(link.server),
    unit: 'tokens',
    added: now,
    // nothing is held at a mint just added
    info: savedInfo(about.info, null),
    used: now,
  });
  state.mints = await records.mints();
  syncShown();
  tell();
}

/** Marks a mint used now, so Holdings shows it. */
async function setUsed(mint: string): Promise<void> {
  const records = state.records;
  const record = state.mints.find((each) => each.mint === mint);
  if (records === undefined || record === undefined) {
    return;
  }
  try {
    await records.putMint({ ...record, used: Date.now() });
    state.mints = await records.mints();
  } catch {
    // only which mint Holdings shows depends on it: show it here all the same, until the next read
    state.mints = state.mints.map((each) => (each.mint === mint ? { ...each, used: Date.now() } : each));
  }
  syncShown();
}

/** Add mint: keep it, and open Holdings. */
async function addFirst(): Promise<void> {
  const link = state.first.link ?? state.playground;
  const about = state.first.about;
  if (link === undefined || about === undefined || state.first.busy !== undefined) {
    return;
  }
  if (state.mints.some((record) => record.mint === link.mint)) {
    await setUsed(link.mint);
    setRoute('home');
    return;
  }
  state.first = { ...state.first, busy: 'add', error: undefined };
  render();
  try {
    await saveMint(link, about);
  } catch (error) {
    state.first = { ...state.first, busy: undefined, error: say(error, 'open', { host: hostOf(link.server) }).words };
    render();
    return;
  }
  setRoute('home');
}

/** Take from the faucet: keep the mint first, so the note always has its mint, then take, then Holdings. */
async function takeFirst(): Promise<void> {
  const link = state.first.link ?? state.playground;
  const about = state.first.about;
  const faucet = about?.faucet;
  if (link === undefined || about === undefined || faucet === undefined || state.first.busy !== undefined) {
    return;
  }
  state.first = { ...state.first, busy: 'take', error: undefined };
  render();
  try {
    if (!state.mints.some((record) => record.mint === link.mint)) {
      await saveMint(link, about);
    }
    await takeInto(link);
  } catch (error) {
    const said = say(error, 'take', { host: hostOf(link.server), perHour: faucet.perAddressPerHour });
    heed(said);
    state.first = {
      ...state.first,
      busy: undefined,
      error: said.words,
      about: said.noFaucet === true ? { ...about, faucet: undefined } : about,
    };
    render();
    return;
  }
  setRoute('home');
}

// ---------- taking ----------

let persisting = false;

/** Asks the browser to keep the purse's storage under pressure: once, on the first take. */
function keepStorage(): void {
  if (persisting) {
    return;
  }
  persisting = true;
  try {
    void navigator.storage?.persist?.().catch(() => undefined);
  } catch {
    // the purse works without it; the browser may clear it under pressure
  }
}

/** Notes that just came: pink on Holdings for six seconds, sliding in on its next draw. */
function arrived(ids: readonly string[], t: number): void {
  for (const id of ids) {
    state.home.pink.set(id, t);
  }
  state.home.arriving.push(...ids);
}

/** Takes from the faucet at `link`'s server, logs it, and marks the new note to arrive in pink. */
async function takeInto(link: MintLink): Promise<void> {
  const loaded = await loadSdk();
  keepStorage();
  const { value, serials } = await loaded.take(link.server, link.mint);
  const t = Date.now();
  const amount = Number(value);
  // the note is kept by now: a log that cannot be written loses only the entry
  await logEntry({ t, kind: 'faucet', mint: link.mint, amount, serials }).catch(() => undefined);
  await setUsed(link.mint);
  state.home.delta = { v: amount, t };
  arrived(serials, t);
  state.home.took = { amount, ids: serials };
  tell();
}

/** The empty purse's faucet button on Holdings. */
async function takeHome(): Promise<void> {
  const record = shown();
  const faucet = state.home.data?.faucet;
  if (record === undefined || faucet === undefined || state.home.taking) {
    return;
  }
  state.home.taking = true;
  state.home.error = undefined;
  render();
  try {
    await takeInto(record);
  } catch (error) {
    const said = say(error, 'take', { host: hostOf(record.server), perHour: faucet.perAddressPerHour });
    heed(said);
    state.home.taking = false;
    state.home.error = { words: said.words, at: 'take' };
    if (said.noFaucet === true && state.home.data !== undefined) {
      state.home.data = { ...state.home.data, faucet: undefined };
    }
    render();
    return;
  }
  state.home.taking = false;
  await refreshHome();
  // the button is gone with the empty purse: focus goes to the total
  if (document.activeElement === document.body || document.activeElement === null) {
    focusH1();
  }
}

/**
 * Adds an entry to the purse's log and reads the log back. Resolves to its
 * id; keep-alive's entry for a rotation joins the one the log has for it
 * already (`spent`: the notes its run moved, see `Records.addKept`), and
 * resolves to none.
 */
async function logEntry(entry: Omit<LogEntry, 'id'>, spent?: readonly Moved[]): Promise<number | undefined> {
  const records = state.records;
  if (records === undefined) {
    return undefined;
  }
  let id: number | undefined;
  if (entry.kind === 'kept' && entry.round !== undefined) {
    await records.addKept(entry, spent ?? []);
  } else {
    id = await records.addLog(entry);
  }
  state.log = await records.log();
  return id;
}

/** Saves what changed of a log entry, and reads the log back. */
async function updateEntry(entry: LogEntry): Promise<void> {
  const records = state.records;
  if (records === undefined || entry.id === undefined) {
    return;
  }
  try {
    await records.updateLog(entry);
    state.log = await records.log();
  } catch {
    // the payment is kept by the SDK; only Recent's row depends on it
  }
}

/** A log entry by its id, as last read. */
const entryById = (id: number | undefined): LogEntry | undefined =>
  id === undefined ? undefined : state.log.find((entry) => entry.id === id);

// ---------- keeping Holdings current ----------

let reading: Promise<void> | undefined;
let readAgain = false;

/** Reads the shown mint again; calls while a read runs ask for one more after it, not one each. */
function refreshHome(): Promise<void> {
  if (reading !== undefined) {
    readAgain = true;
    return reading;
  }
  reading = (async () => {
    try {
      do {
        readAgain = false;
        await readHome();
      } while (readAgain);
    } finally {
      reading = undefined;
    }
  })();
  return reading;
}

/** What the purse's log knows of each note of `mint`, and of the notes keep-alive just moved: see `arrivalsOf`. */
const arrivals = (mint: string): Map<string, Arrival> => arrivalsOf(state.log, mint, state.keep.hopped);

/** The shown mint's faucet being asked now, if it is. */
let askingFaucet: Promise<void> | undefined;

/**
 * Asks the faucet at the shown mint's server, apart from the read, so that
 * a slow or silent faucet holds back nothing else; when it answers, the
 * empty purse's Take button comes or goes. One ask at a time.
 */
function askFaucet(record: MintRecord): Promise<void> {
  if (askingFaucet !== undefined) {
    return askingFaucet;
  }
  const asking = (async () => {
    let faucet: FaucetInfo | undefined;
    try {
      faucet = await (await loadSdk()).faucetAt(record.server, record.mint);
    } catch {
      // the SDK did not load: the read says so
      return;
    }
    const data = state.home.data;
    if (shown()?.mint !== record.mint || data === undefined) {
      return;
    }
    state.home.data = { ...data, faucet };
    redrawIfChanged();
  })();
  askingFaucet = asking;
  void asking.finally(() => {
    if (askingFaucet === asking) {
      askingFaucet = undefined;
    }
  });
  return asking;
}

/** Draws again where what the shown mint holds shows, when that changed: Holdings, Send's scan and Receive's ask. */
function redrawIfChanged(): void {
  const seen = shownSignature(Date.now());
  if (state.route === 'home' && seen !== state.home.drawn) {
    render();
  } else if (
    ((state.route === 'send' && state.send.step === 'scan') || (state.route === 'receive' && state.receive.step === 'ask')) &&
    seen !== state.home.sideDrawn
  ) {
    render();
  }
}

/** How long the first read of an empty purse waits for its faucet, so the Take button usually comes with it. */
const FAUCET_GRACE_MS = 800;

/** The lane most of `ids` landed in: where the sentence says the tokens just taken went. */
function landedLane(holdings: Holdings, ids: readonly string[]): LaneKey | undefined {
  const notes = holdings.notes.filter((note) => ids.includes(note.id));
  if (notes.length === 0) {
    return undefined;
  }
  const rest = sum(notes.filter((note) => laneOf(note.pos, holdings.layers) === 'rest').map((note) => note.v));
  return rest * 2 >= sum(notes.map((note) => note.v)) ? 'rest' : 'ready';
}

/**
 * One read of the shown mint: drop what faded (and log it), then draw what
 * is held, say what changed, and plan the read after the next rotation.
 * While the purse is empty, its faucet is asked too, on the side.
 */
async function readHome(): Promise<void> {
  const record = shown();
  if (record === undefined) {
    return;
  }
  let read: Read;
  try {
    read = await readPruned(record.server, record.mint);
  } catch (error) {
    const host = hostOf(record.server);
    const said = say(error, 'open', { host });
    heed(said);
    // a mint that does not answer: what fades, and when, unless the purse reaches it
    const fades = transient(error) ? unreachableFor(host, state.home.data?.holdings, record.info?.fades, Date.now()) : undefined;
    state.home.error = { words: fades ?? said.words, at: 'read' };
    // the visit's first pass runs all the same: the other mints are kept, and what a closed tab left outgoing is
    // settled while the mints still keep their answers; the keeper asks this mint again each minute after it
    if (!finishing && !pokedOnOpen) {
      pokedOnOpen = true;
      keeper.poke();
    }
    redrawIfChanged();
    return;
  }
  if (shown()?.mint !== record.mint) {
    return;
  }
  state.home.mint = record.mint;
  const before = state.home.data?.holdings;
  const holdings = holdingsFrom(read.info, read.notes, read.total, read.firstExpiry, read.skew, arrivals(record.mint), read.moving);
  landHops(before, holdings);
  keepLanes(holdings);
  const rotated = state.home.oldest !== undefined && read.info.oldest > state.home.oldest;
  state.home.oldest = read.info.oldest;
  state.home.data = { holdings, faucet: state.home.data?.faucet };
  if (state.home.error?.at === 'read') {
    state.home.error = undefined;
  }
  const opened = state.home.opened;
  if (opened !== undefined && 'note' in opened && !holdings.notes.some((note) => note.id === opened.note)) {
    state.home.opened = undefined;
  }
  void keepInfo(record, read.info, firstFade(holdings));
  // fading tokens, or tokens left on their way, call for a pass; and the first read of a visit, to settle what is outgoing
  const calls = keeping.saw(record.mint, read);
  if (!finishing && (!pokedOnOpen || calls === 'due' || (calls === 'moving' && !keeper.running))) {
    pokedOnOpen = true;
    keeper.poke();
  }
  if (holdings.notes.length === 0) {
    const asked = askFaucet(record);
    if (before === undefined && state.route === 'home') {
      await Promise.race([asked, new Promise((resolve) => setTimeout(resolve, FAUCET_GRACE_MS))]);
      if (state.route !== 'home' || shown()?.mint !== record.mint) {
        return;
      }
    }
  }
  if (state.route !== 'home') {
    redrawIfChanged();
    return;
  }
  arrive(state.home.arriving.splice(0));
  const now = Date.now();
  planRotation();
  const took = state.home.took;
  if (took?.received === true && state.home.received !== undefined && state.home.received.lane === undefined) {
    state.home.received = { ...state.home.received, lane: landedLane(holdings, took.ids) };
  }
  const seen = shownSignature(now);
  if (seen === state.home.drawn && took === undefined && !rotated) {
    return;
  }
  render();
  if (took !== undefined) {
    state.home.took = undefined;
    if (took.received === true) {
      const notes = holdings.notes.filter((note) => took.ids.includes(note.id));
      const resting = sum(notes.filter((note) => laneOf(note.pos, holdings.layers) === 'rest').map((note) => note.v));
      announce(receivedWords(took.amount, sum(notes.map((note) => note.v)) - resting, resting, notes.length, atWords(holdings)));
      if (took.also !== undefined) {
        const also = took.also;
        setTimeout(() => announce(also), 1500);
      }
    } else {
      const note = holdings.notes.find((each) => took.ids.includes(each.id));
      announce(tookWords(took.amount, note === undefined ? undefined : laneOf(note.pos, holdings.layers), holdings, now));
    }
  } else if (rotated && before !== undefined) {
    const start = holdings.schedule?.startMs;
    announce(rotatedWords(start !== undefined && start <= now && start > (before.schedule?.startMs ?? 0) ? start : now, holdings));
  }
}

/**
 * What Holdings shows that can change between reads: the notes and their
 * times, the schedule, the faucet, the error, the log, whether the chip
 * shows and the day (for Recent's times). The rest changes only on a step
 * of the person's, which draws anyway.
 */
function shownSignature(now: number): string {
  const holdings = state.home.data?.holdings;
  const delta = state.home.delta;
  const record = shown();
  const keep = keepShown();
  return JSON.stringify([
    state.home.mint,
    holdings?.total,
    holdings?.notes.map((note) => [note.id, note.pos, note.expiresAt, note.readyAt, note.moving]),
    holdings?.schedule?.nextMs,
    holdings?.paused,
    holdings?.firstExpiry,
    state.home.data?.faucet?.amount.toString(),
    record === undefined ? undefined : homeError(record)?.words,
    record === undefined ? undefined : keeps(record),
    keep === undefined ? undefined : [keep.amount, keep.done, keep.total, keep.spaced, keep.watching],
    record === undefined ? undefined : elsewhereSig(record, now),
    record === undefined ? undefined : savedShown(record, now)?.notes.map((note) => [note.v, note.pos, note.expiresAt]),
    state.log.length,
    state.log[0]?.id,
    state.log.map((entry) => entry.state ?? '').join(),
    state.handBacks.length,
    delta !== undefined && now - delta.t < 60 * 60_000,
    new Date(now).toDateString(),
  ]);
}

/**
 * Saves the mint's info when its shape changed, so that the next open draws
 * its lanes at once, or when what fades first changed, so that an open the
 * mint does not answer can say when.
 */
async function keepInfo(record: MintRecord, info: MintInfo, fades: LastFade | null): Promise<void> {
  const saved = savedInfo(info, fades);
  const was = record.info;
  if (
    state.records === undefined ||
    (was !== undefined &&
      was.layers === saved.layers &&
      was.periodSecs === saved.periodSecs &&
      was.paused === saved.paused &&
      was.fades !== undefined &&
      was.fades?.at === fades?.at &&
      was.fades?.amount === fades?.amount)
  ) {
    return;
  }
  try {
    const current = state.mints.find((each) => each.mint === record.mint) ?? record;
    await state.records.putMint({ ...current, info: saved });
    state.mints = await state.records.mints();
  } catch {
    // only the drawing before the first read depends on it
  }
}

let rotationTimer: ReturnType<typeof setTimeout> | undefined;

/** Reads again just after the next rotation, or shortly when the mint has not made it yet. */
function planRotation(): void {
  clearTimeout(rotationTimer);
  const schedule = state.home.data?.holdings.schedule;
  if (schedule === undefined || state.route !== 'home') {
    return;
  }
  const wait = schedule.nextMs - Date.now();
  rotationTimer = setTimeout(
    () => {
      if (state.route === 'home' && !document.hidden) {
        void refreshHome();
      }
    },
    wait > 0 ? Math.min(wait + 1500, 2 ** 31 - 1) : 5000,
  );
}

// ---------- the camera ----------

/** Starts the camera for the view shown: a request on Send, a hand-back on Receive. */
function camera(): void {
  state.scanError = undefined;
  const route = state.route;
  void startCamera(
    (text) => {
      if (state.route !== route) {
        return;
      }
      if (route === 'send') {
        void openAsk(text, 'camera');
      } else {
        void takeText(text, 'camera');
      }
    },
    (next) => {
      const was = state.camera.kind;
      state.camera = next;
      render();
      if (next.kind === 'running') {
        focusKey('cam-toggle');
      } else if (was === 'starting' && next.kind === 'refused') {
        focusKey('cam');
      }
      // said in the page's one live region: the viewfinder's words are drawn anew with it, where a reader may miss them
      const words = cameraWords(next);
      if (words !== '') {
        announce(words);
      }
    },
  );
}

/** Stops the camera; from its own button or Escape, focus goes back to the button that starts it. */
function cameraOff(refocus = true): void {
  stopCamera();
  state.camera = { kind: 'idle' };
  render();
  if (refocus) {
    focusKey('cam');
  }
}

// ---------- keep-alive ----------

/** A hop whose new note no read has shown after this long slides nowhere. */
const HOP_MS = 10_000;
/** How long a note keep-alive moved keeps its place by its hop, while its run's log entry is on its way. */
const HOPPED_MS = 10 * 60_000;

/** The pass is reading the shown mint after its work: that read calls for no pass of its own. */
let finishing = false;
/** Whether this visit poked the keeper on open, once the first read was drawn, or failed. */
let pokedOnOpen = false;

/** The servers the purse knows, with their mints: the shown mint's server first, and the shown mint first in it. */
function keptServers(): KeptServer[] {
  const first = shown();
  const records = first === undefined ? state.mints : [first, ...state.mints.filter((record) => record !== first)];
  const servers = new Map<string, KeptServer>();
  for (const record of records) {
    const server = servers.get(record.server) ?? { server: record.server, mints: [] };
    server.mints.push({ mint: record.mint, host: hostOf(record.server) });
    servers.set(record.server, server);
  }
  return [...servers.values()];
}

/** What the purse holds at `mint`, once what faded is dropped and logged. */
async function readPruned(server: string, mint: string): Promise<Read> {
  const loaded = await loadSdk();
  const read = await loaded.read(server, mint);
  return read.notes.some((note) => note.state === 'expired') ? pruneAndLog(loaded, server, mint, read) : read;
}

/** Drops the notes whose layer the mint dropped, logs "Faded N", and reads again: for Holdings' reads and the keeper's alike. */
async function pruneAndLog(loaded: Sdk, server: string, mint: string, read: Read): Promise<Read> {
  const { lost, generation } = await loaded.prune(server, mint);
  if (lost > 0n && generation !== undefined) {
    const t = fadedAt(generation, read.info, read.skew, Date.now());
    await logEntry({ t, kind: 'faded', mint, amount: Number(lost), serials: [] }).catch(() => undefined);
    tell();
  }
  return loaded.read(server, mint);
}

/** This tab's sends and settles from Send that hold the lock, or wait for it: a pass gives way to them. */
let sendsHere = 0;

/**
 * A keep-alive pass's lock (lock.ts), if nothing else holds it: false,
 * without running `work`, while another tab does. While a send of this
 * tab's holds it or waits for it, the pass gives way and runs nothing; the
 * send pokes the keeper once it is over.
 */
async function passLock(work: () => Promise<void>): Promise<boolean> {
  if (sendsHere > 0) {
    return true;
  }
  return (
    (await withTransfersLock(async () => {
      await work();
      return true;
    }, true)) ?? false
  );
}

/**
 * Runs `work`, a send or a settle the person asked for, holding the lock and
 * waiting for it: the other tabs' passes are asked to stop at their next
 * safe step, and this tab's pass stops first. Keep-alive runs again after.
 */
async function sending<R>(work: () => Promise<R>): Promise<R> {
  channel?.postMessage({ kind: 'yield' });
  await keeper.stop();
  sendsHere++;
  try {
    return await work();
  } finally {
    sendsHere--;
    keeper.poke();
  }
}

/** Says words in the live region for a run, only where the page shows it: Holdings. */
function announceHome(words: string): void {
  if (state.route === 'home') {
    announce(words);
  }
}

const keeping: Keeping = new Keeping({
  servers: keptServers,
  shown: () => shown()?.mint,
  read: async (server, mint) => {
    const read = await readPruned(server, mint);
    // what fades first at each mint the purse keeps, for an open the mint does not answer
    const record = state.mints.find((each) => each.mint === mint);
    if (record !== undefined) {
      void keepInfo(record, read.info, firstFade(holdingsOf(read)));
    }
    return read;
  },
  saved: (mint) => state.mints.find((each) => each.mint === mint)?.info?.fades,
  outgoing: async (server) => (await loadSdk()).outgoingAt(server),
  keepServer: async (server, work) => {
    const loaded = await loadSdk();
    let sends = false;
    try {
      // a settle resends a send's transfers too: each send's log entry says what it came to, as Send's own settle would
      return await loaded.keepServer(server, work, async (settled) => {
        await applySettled(server, settled);
        sends ||= settled.sends.length > 0;
      });
    } finally {
      if (sends) {
        await loadHandBacks();
        tell();
        afterSettle(false);
      }
    }
  },
  log: async ({ spent, ...entry }) => {
    // the tokens are safe in the SDK's store by now: a log that cannot be written loses only the entry
    await logEntry(entry, spent).catch(() => undefined);
    tell();
  },
  progress: keepProgress,
  hop: (spent) => {
    state.keep.hops.set(spent.id, { v: spent.v, generation: spent.generation, at: Date.now() });
    tell();
    void refreshHome();
  },
  troubled: () => {
    const record = shown();
    if (state.route === 'home' && record !== undefined && shownSignature(Date.now()) !== state.home.drawn) {
      render();
    }
  },
  kept: (mint) => channel?.postMessage({ kind: 'kept', mint }),
  announce: announceHome,
  finish: finishPass,
  reopen,
  now: () => Date.now(),
  random: () => Math.random(),
});

const keeper: Keeper = new Keeper({
  pass: (run) => keeping.pass(run),
  lock: passLock,
  hidden: () => document.hidden,
  wakeLock: () => ('wakeLock' in navigator ? navigator.wakeLock : undefined),
  // the first run shows no run: one of a few seconds there keeps the screen as it is
  shows: () => state.route === 'home',
  // another tab runs a pass: it says how its run stands
  busy: () => channel?.postMessage({ kind: 'ask' }),
  quiet: () => {
    if (state.keep.watching !== undefined) {
      drawBack(() => {
        state.keep.watching = undefined;
      });
    }
  },
  setTimeout: (run, ms) => setTimeout(run, ms),
  clearTimeout: (timer) => clearTimeout(timer as ReturnType<typeof setTimeout> | undefined),
  now: () => Date.now(),
});

/** Tells the other tabs how the shown mint's run stands here. */
function postKeeping(mint: string, p: Shown): void {
  channel?.postMessage({ kind: 'keeping', mint, amount: p.amount, done: p.done, total: p.total });
}

/** The shown mint's run moved on, or ended: count it in the back row and column, and tell the other tabs. */
function keepProgress(p: Shown | undefined): void {
  const record = shown();
  const was = state.keep.progress;
  drawBack(() => {
    state.keep.progress = p;
  });
  if (record === undefined) {
    return;
  }
  if (p === undefined) {
    // the run is over here, or stopped: the tabs watching it stop showing it
    channel?.postMessage({ kind: 'kept', mint: record.mint });
    return;
  }
  postKeeping(record.mint, p);
  if (p.total > (was?.total ?? 0)) {
    // more hops planned, and saved as outgoing: their notes are moving now, here and in the other tabs
    tell();
    void refreshHome();
  }
}

/** The pass is over: read the shown mint again, then say what was kept of it and the new date to come back by. */
async function finishPass(kept: number): Promise<void> {
  finishing = true;
  try {
    await refreshHome();
  } finally {
    finishing = false;
  }
  if (kept > 0) {
    const holdings = state.home.data?.holdings;
    const now = Date.now();
    announceHome(keptWords(kept, holdings === undefined ? undefined : comeBackAt(holdings, now), now));
  }
}

/** The run Holdings shows: this tab's, or another's it hears of. */
function keepShown(): KeepShown | undefined {
  const { progress, watching } = state.keep;
  if (progress !== undefined && progress.total > 0) {
    return { ...progress, watching: false };
  }
  if (watching !== undefined && watching.total > 0) {
    return { amount: watching.amount, done: watching.done, total: watching.total, spaced: true, watching: true };
  }
  return undefined;
}

/** Whether the purse can keep the shown mint's tokens alive now: not while it is paused, stuck, or its read fails. */
function keeps(record: MintRecord): boolean {
  if (state.home.error?.at === 'read') {
    // what the purse cannot read now, it cannot move either
    return false;
  }
  const paused = state.home.data?.holdings.paused ?? record.info?.paused ?? false;
  return keeping.canKeep(record.mint, paused);
}

/**
 * The shown mint's first fade as the purse saved it, once the visit's read
 * failed before any came back: the come-back row and column say it, as the
 * words under the field do. Before that, nothing, as while a read is on its way.
 */
function savedShown(record: MintRecord, now: number): Holdings | undefined {
  return state.home.data === undefined && state.home.error?.at === 'read' && record.info !== undefined
    ? savedHoldings(record.info, now)
    : undefined;
}

/**
 * What Holdings says went wrong: a take's error at its button; a read's
 * under the field, in the keeper's words when the mint did not answer it
 * either, since those say what could not move; else why keep-alive is
 * stuck, at the shown mint or, naming it, at another the purse keeps.
 */
function homeError(record: MintRecord): HomeInput['error'] {
  const error = state.home.error;
  const trouble = keeping.trouble.get(record.mint);
  if (error?.at === 'read' && trouble?.kind === 'unreachable') {
    return { words: trouble.words, at: 'read' };
  }
  if (error !== undefined) {
    return error;
  }
  if (trouble !== undefined) {
    return { words: trouble.words, at: 'keep' };
  }
  const other = state.mints.flatMap((each) => {
    const elsewhere = each.mint === record.mint ? undefined : keeping.trouble.get(each.mint);
    return elsewhere === undefined ? [] : [elsewhere];
  })[0];
  return other === undefined ? undefined : { words: other.words, at: 'keep' };
}

/** A mint's holdings from a read, for the words and the come-back date. */
const holdingsOf = (read: Read): Holdings =>
  holdingsFrom(read.info, read.notes, read.total, read.firstExpiry, read.skew, new Map(), read.moving);

/**
 * The mint whose tokens go first, when it is not the shown one: the date to
 * come back by is the earliest of every mint the purse keeps, and Holdings
 * shows only the first mint added until the Mints screen comes. A mint this
 * page has not read, since it has not answered yet or does not, counts by
 * what the purse saved of what fades there first.
 */
function elsewhere(record: MintRecord, now: number): Elsewhere | undefined {
  let found: Elsewhere | undefined;
  let soonest = (state.home.data?.holdings ?? savedShown(record, now))?.firstExpiry ?? Number.POSITIVE_INFINITY;
  for (const other of state.mints) {
    if (other.mint === record.mint) {
      continue;
    }
    const read = keeping.last(other.mint);
    const holdings = read !== undefined ? holdingsOf(read) : other.info === undefined ? undefined : savedHoldings(other.info, now);
    if (holdings === undefined) {
      continue;
    }
    const first = holdings.firstExpiry;
    if (holdings.notes.length > 0 && first !== undefined && first > now && first < soonest) {
      soonest = first;
      found = { name: other.name, holdings, keeps: keeping.canKeep(other.mint, holdings.paused) };
    }
  }
  return found;
}

/** What the come-back date shows of another mint, for the signature. */
function elsewhereSig(record: MintRecord, now: number): unknown {
  const other = elsewhere(record, now);
  return other === undefined
    ? undefined
    : [other.name, other.keeps, other.holdings.firstExpiry, other.holdings.notes.map((note) => [note.v, note.pos])];
}

/**
 * Changes what the come-back row and column show, and draws only those two
 * again: the count moves every few seconds, and focus, an open tip and the
 * field stay as they are. When the view was current before, it is current
 * after, so the next read that finds nothing else new draws nothing.
 */
function drawBack(change: () => void): void {
  const record = shown();
  const current = state.route === 'home' && record !== undefined && state.home.drawn === shownSignature(Date.now());
  change();
  if (state.route !== 'home' || record === undefined) {
    return;
  }
  renderBack(homeInput(record));
  if (current) {
    state.home.drawn = shownSignature(Date.now());
  }
}

/** The notes as drawn, by place: a run's open lanes stay while it is unchanged. */
const noteSig = (holdings: Holdings): string => holdings.notes.map((note) => `${note.id}:${note.pos}`).join(',');

/**
 * A read after hops were answered: each new note takes its old note's place
 * in the field, so it slides one lane on from there, turns pink and draws its
 * thread. A hop whose note no read shows within {@link HOP_MS} is dropped.
 */
function landHops(before: Holdings | undefined, holdings: Holdings): void {
  const now = Date.now();
  for (const [id, at] of state.keep.hopped) {
    if (now - at > HOPPED_MS) {
      state.keep.hopped.delete(id);
    }
  }
  const { hops } = state.keep;
  if (hops.size === 0) {
    return;
  }
  const old = new Set(before?.notes.map((note) => note.id) ?? []);
  const ids = new Set(holdings.notes.map((note) => note.id));
  const taken = new Set<string>();
  for (const [oldId, hop] of hops) {
    const fresh = ids.has(oldId)
      ? undefined
      : holdings.notes.find(
          (note) => !old.has(note.id) && !taken.has(note.id) && note.v === hop.v && note.id.startsWith(`${hop.generation + 1n}:`),
        );
    if (fresh !== undefined) {
      taken.add(fresh.id);
      hopNote(oldId, fresh.id);
      state.home.pink.set(fresh.id, hop.at);
      state.keep.hopped.set(fresh.id, hop.at);
      hops.delete(oldId);
    } else if (now - hop.at > HOP_MS) {
      hops.delete(oldId);
    }
  }
}

/**
 * The lanes a run passes through stay open: each moving note's layer and
 * the next, from its first read until the notes change after the run.
 */
function keepLanes(holdings: Holdings): void {
  const keep = state.keep;
  const moving = holdings.notes.filter((note) => note.moving);
  if (moving.length > 0 || keep.progress !== undefined) {
    if (keep.lanesSig !== undefined) {
      // a new run
      keep.lanes = new Set();
      keep.lanesSig = undefined;
    }
    for (const note of moving) {
      keep.lanes.add(note.pos);
      keep.lanes.add(note.pos + 1);
    }
  } else if (keep.lanes.size > 0) {
    const sig = noteSig(holdings);
    if (keep.lanesSig === undefined) {
      keep.lanesSig = sig;
    } else if (keep.lanesSig !== sig) {
      keep.lanes = new Set();
      keep.lanesSig = undefined;
    }
  }
}

// ---------- Receive ----------

/** A request's link, as the purse hands it out: its address with the request after `#ask=`. */
async function askLink(ask: AskRecord): Promise<string> {
  const known = state.links.get(ask.id);
  if (known !== undefined) {
    return known;
  }
  try {
    const link = `${base()}${linkFor('ask', await encodeAsk(ask.server, ask.json))}`;
    state.links.set(ask.id, link);
    state.unwritten.delete(ask.id);
    return link;
  } catch (error) {
    // the request view says so, rather than show a code of nothing; showing it again tries again
    state.unwritten.add(ask.id);
    throw error;
  }
}

/** Receive on a request's view: its link is written now, if it is not yet, and the view drawn again once it is (or could not be). */
function requestState(ask: AskRecord): ReceiveState {
  if (!state.links.has(ask.id)) {
    state.unwritten.delete(ask.id);
    void askLink(ask)
      .catch(() => undefined)
      .then(() => {
        if (state.route === 'receive' && state.receive.ask?.id === ask.id) {
          render();
        }
      });
  }
  return {
    ...blankReceive(),
    step: 'request',
    ask,
    amount: String(ask.amount),
    atMost: ask.atMost,
    smaller: ask.atMost !== undefined,
  };
}

/** Reads a mint's denominations once, for Receive's sizes of note: powers of two until then. */
const policyLoading = new Set<string>();
function loadPolicy(record: MintRecord): void {
  if (state.policies.has(record.mint) || policyLoading.has(record.mint)) {
    return;
  }
  policyLoading.add(record.mint);
  void (async () => {
    try {
      const { policy } = await (await loadSdk()).policyAt(record.server, record.mint);
      state.policies.set(record.mint, policy.denominations);
      if (state.route === 'receive' && state.receive.step === 'ask' && policy.denominations.kind !== 'powers') {
        render();
      }
    } catch {
      // asked again on the next visit
    } finally {
      policyLoading.delete(record.mint);
    }
  })();
}

function receiveInput(): ReceiveInput {
  const record = shown();
  const r = state.receive;
  const take = r.take;
  const holdings = record !== undefined && state.home.mint === record.mint ? state.home.data?.holdings : undefined;
  const now = Date.now();
  return {
    state: r,
    record,
    bar: record !== undefined ? { mint: record.mint, name: record.name } : { mint: take?.link?.mint ?? '', name: take?.host ?? '' },
    denoms: (record === undefined ? undefined : state.policies.get(record.mint)) ?? { kind: 'powers' },
    asks: record === undefined ? [] : openAsks(state.asks, record.mint),
    link: r.ask === undefined ? undefined : state.links.get(r.ask.id),
    linkFailed: r.ask !== undefined && state.unwritten.has(r.ask.id),
    qr: r.step === 'request' ? qrState() : qr,
    resting: holdings === undefined ? [] : sliceNotes(holdings).filter((note) => note.lane === 'rest'),
    at: atWords(holdings),
    camera: state.camera,
    scanError: state.scanError,
    now,
    actions: receiveActions,
  };
}

const receiveActions: ReceiveInput['actions'] = {
  amountChanged: (text) => {
    state.receive.amount = text;
    if (state.receive.error !== undefined) {
      state.receive.error = undefined;
      render();
    } else {
      refreshAsk(receiveInput());
    }
  },
  toggleSmaller: () => {
    state.receive.smaller = !state.receive.smaller;
    render();
  },
  atMostChanged: (value) => {
    state.receive.atMost = value;
    refreshAsk(receiveInput());
  },
  ask: () => void askFor(),
  openAsk: (id) => {
    const ask = state.asks.find((each) => each.id === id);
    if (ask === undefined) {
      return;
    }
    state.receive = requestState(ask);
    render();
    window.scrollTo(0, 0);
    focusH1();
  },
  newRequest: () => {
    const r = state.receive;
    state.receive = { ...blankReceive(), amount: r.amount, atMost: r.atMost, smaller: r.atMost !== undefined };
    render();
    window.scrollTo(0, 0);
    focusKey('ask-amount');
  },
  toggleScan: () => {
    const r = state.receive;
    r.mode = r.mode === 'scan' ? undefined : 'scan';
    r.takeError = undefined;
    state.scanError = undefined;
    render();
  },
  openPaste: () => {
    const r = state.receive;
    r.mode = 'paste';
    state.scanError = undefined;
    render();
    const field = $<HTMLInputElement>('#take-link');
    field?.focus({ preventScroll: true });
    $('#recv-panel')?.scrollIntoView({ block: 'nearest', behavior: reduced() ? 'auto' : 'smooth' });
  },
  pasteChanged: (text) => {
    state.receive.paste = text;
  },
  take: () => void takeText(state.receive.paste, 'paste'),
  camera,
  cameraOff: () => cameraOff(),
  askCancel: () => {
    state.receive.confirmCancel = true;
    state.receive.error = undefined;
    render();
    focusKey('keep-ask');
  },
  keepAsk: () => keepAsk(),
  doCancel: () => void cancelRequest(),
  retryTake: () => {
    const link = state.receive.take?.link;
    if (link !== undefined) {
      void takeLink(link, 'link');
    }
  },
};

/** Closes the cancel question: the request stays open. */
function keepAsk(): void {
  state.receive.confirmCancel = false;
  state.receive.error = undefined;
  render();
  focusKey('cancel-ask');
}

/** Ask: make the request, keep its record, and show it. */
async function askFor(): Promise<void> {
  const record = shown();
  const r = state.receive;
  if (record === undefined || r.asking) {
    return;
  }
  const input = receiveInput();
  const parsed = parseAmount(r.amount);
  if (!parsed.ok) {
    r.error = amountWords(parsed.problem);
    render();
    focusKey('ask-amount');
    return;
  }
  const atMost = atMostFor(input);
  r.asking = true;
  r.error = undefined;
  render();
  const host = hostOf(record.server);
  let asked: Awaited<ReturnType<Sdk['ask']>>;
  const loaded = await loadSdk().catch(() => undefined);
  try {
    if (loaded === undefined) {
      throw new TypeError('the purse did not load');
    }
    asked = await loaded.ask(record.server, record.mint, parsed.amount, atMost);
  } catch (error) {
    const said = say(error, 'ask', { host });
    heed(said);
    r.asking = false;
    r.error = said.words;
    render();
    return;
  }
  const ask: AskRecord = {
    id: asked.id,
    mint: record.mint,
    server: record.server,
    amount: parsed.amount,
    notes: asked.notes,
    atMost,
    json: asked.json,
    t: Date.now(),
    state: 'open',
    got: 0,
    endedAt: undefined,
  };
  try {
    if (state.records === undefined) {
      throw new DOMException('no records', 'NotSupportedError');
    }
    await state.records.putAsk(ask);
    state.asks = await state.records.asks();
  } catch {
    // a request the purse cannot list is one nobody can cancel
    await loaded.cancelAsk(record.server, asked.json).catch(() => undefined);
    r.asking = false;
    r.error = notRecorded;
    render();
    return;
  }
  await setUsed(record.mint);
  tell();
  await askLink(ask).catch(() => undefined);
  if (state.route !== 'receive') {
    return;
  }
  state.receive = requestState(ask);
  render();
  window.scrollTo(0, 0);
  focusH1();
}

/** Cancel it: withdraw the request, after the question. */
async function cancelRequest(): Promise<void> {
  const r = state.receive;
  const ask = r.ask;
  if (ask === undefined || r.cancelling) {
    return;
  }
  r.cancelling = true;
  r.error = undefined;
  render();
  const records = state.records;
  // the record says cancelled before the keys go, so a request is never listed open without its keys
  try {
    if (records === undefined) {
      throw new DOMException('no records', 'NotSupportedError');
    }
    await records.putAsk({ ...ask, state: 'cancelled', endedAt: Date.now() });
    state.asks = await records.asks();
  } catch {
    r.cancelling = false;
    r.error = notCancelled;
    render();
    return;
  }
  try {
    await (await loadSdk()).cancelAsk(ask.server, ask.json);
  } catch (error) {
    // its keys are kept, so it is open again; should that not save, a hand-back for it is still taken in
    await records
      .putAsk(ask)
      .then(async () => {
        state.asks = await records.asks();
      })
      .catch(() => undefined);
    const said = say(error, 'ask', { host: hostOf(ask.server) });
    heed(said);
    r.cancelling = false;
    r.error = said.words;
    render();
    return;
  }
  tell();
  announce(cancelledWords(ask.amount));
  state.receive = { ...blankReceive(), amount: String(ask.amount), atMost: ask.atMost, smaller: ask.atMost !== undefined };
  render();
  window.scrollTo(0, 0);
  focusH1();
}

/** A hand-back from a link, a paste or the camera: read it, then take it in. */
async function takeText(text: string, via: 'link' | 'paste' | 'camera', focus = true): Promise<void> {
  const decoded = await decode(text);
  if (decoded.ok && decoded.link.kind === 'take') {
    if (via === 'camera') {
      stopCamera();
      state.camera = { kind: 'idle' };
      state.scanError = undefined;
      state.receive.mode = 'paste';
    }
    await takeLink(decoded.link, via, focus);
    return;
  }
  const words = decoded.ok ? linkProblemWords('take', 'kind') : linkProblemWords('take', decoded.problem, decoded.kind);
  if (via === 'camera') {
    state.scanError = words;
    render();
    return;
  }
  if (via === 'link') {
    // a hand-back link that does not read: the take step says why, whatever the purse holds
    const take = state.receive.take;
    if (state.route !== 'receive' || state.receive.step !== 'take' || take === undefined || take.state !== 'reading') {
      return;
    }
    state.receive.take = { link: undefined, state: 'failed', words, name: '', host: '' };
    render();
    if (focus) {
      focusH1();
    }
    return;
  }
  state.receive.takeError = words;
  render();
  focusKey('take-link');
}

/**
 * Takes in a hand-back: by its link on the take step, or inline on the
 * request view. A good one ends on Holdings; one that took nothing says why.
 *
 * Its notes' own bytes say what it carries and which request it answers,
 * by the one-time keys they are paid to: the link's own words for these
 * prove nothing, since anyone can write a link. So the purse always asks
 * the SDK to take the notes in, and says "already" only from its records.
 */
async function takeLink(given: TakeLink, via: 'link' | 'paste' | 'camera', focus = true): Promise<void> {
  const holders = new Set(given.notes.flatMap((note) => readNote(note)?.holder ?? []));
  const carried = valueOf(given.notes);
  const link: TakeLink = carried === undefined || carried < 1 ? given : { ...given, amount: carried };
  const record = state.mints.find((each) => each.mint === link.mint);
  const host = hostOf(record?.server ?? link.server);
  const name = record?.name ?? host;
  const inline = via !== 'link';
  const r = state.receive;
  const end = (kind: 'failed' | 'twice' | 'not-ours' | 'cancelled' | 'unheld', o: { at?: number | undefined; words?: string; transient?: boolean } = {}): void => {
    if (inline && state.route === 'receive' && state.receive.step === 'request') {
      state.receive.taking = false;
      state.receive.takeError = takeEndWords(kind, { amount: link.amount, at: o.at, now: Date.now(), host, words: o.words }).body;
      render();
      return;
    }
    state.receive = {
      ...state.receive,
      step: 'take',
      take: { link, state: kind, words: o.words, transient: o.transient === true, at: o.at, name, host },
    };
    setRoute('receive', { hash: '#receive', keep: true, focus: false });
    render();
    if (focus) {
      focusH1();
    }
  };
  if (!inline) {
    // the take step at once: opening a hand-back means wanting it taken
    state.receive = { ...blankReceive(), step: 'take', take: { link, state: 'taking', name, host } };
    setRoute('receive', { hash: '#receive', keep: true, focus: false });
    if (focus) {
      focusH1();
    }
  } else {
    r.taking = true;
    r.takeError = undefined;
    render();
  }
  if (record === undefined) {
    end('unheld');
    return;
  }
  // the request whose keys these notes are paid to, if it is this purse's
  const ask = state.asks.find((each) => each.mint === link.mint && holdersOf(each.json).some((holder) => holders.has(holder)));
  const askId = ask?.id ?? (holders.has(link.ask) ? link.ask : undefined);
  let took: { value: number; serials: string[] };
  let loaded: Sdk | undefined;
  try {
    loaded = await loadSdk();
    took = await loaded.acceptHandBack(record.server, link.mint, link.notes);
  } catch (error) {
    const said = say(error, 'accept', { host });
    heed(said);
    end('failed', { words: said.words, transient: loaded === undefined || loaded.transient(error) });
    return;
  }
  if (took.value <= 0) {
    // nothing taken: "already" only when this purse's own records took this request's tokens in
    const entry = state.log.find((each) => each.kind === 'received' && each.ask !== undefined && (each.ask === ask?.id || holders.has(each.ask)));
    if (entry !== undefined || (ask !== undefined && (ask.got > 0 || ask.state === 'taken'))) {
      end('twice', { at: entry?.t });
    } else if (ask?.state === 'cancelled') {
      end('cancelled', { at: ask.endedAt });
    } else {
      end('not-ours');
    }
    return;
  }
  const t = Date.now();
  const shownAsk = state.receive.ask;
  if (ask !== undefined) {
    try {
      await state.records?.putAsk(askAfterTake(ask, took.value, t));
      state.asks = (await state.records?.asks()) ?? state.asks;
    } catch {
      // taken in all the same: the request may show as open until the next write
    }
  }
  await logEntry({ t, kind: 'received', mint: link.mint, amount: took.value, ...(askId === undefined ? {} : { ask: askId }), serials: took.serials }).catch(() => undefined);
  await setUsed(link.mint);
  state.home.delta = { v: took.value, t };
  arrived(took.serials, t);
  state.home.took = {
    amount: took.value,
    ids: took.serials,
    received: true,
    // never refuse tokens that are this purse's: say which request they were for
    also: ask !== undefined && shownAsk !== undefined && ask.id !== shownAsk.id && ask.state === 'open' ? otherAskWords(ask.amount) : undefined,
  };
  tell();
  state.receive = blankReceive();
  stopCamera();
  state.camera = { kind: 'idle' };
  state.home.received = { amount: took.value, t, lane: undefined };
  setRoute('home');
}

// ---------- Send: reading a request ----------

/** The request's ticket link, as its code on the ticket draws it. */
async function ticketLink(ask: AskLink): Promise<string> {
  return `${base()}${linkFor('ask', await encodeAsk(ask.server, ask.json))}`;
}

/** What can be spent now (fading included) and what rests, as the planner takes them: in notesAt order. */
function spendables(read: Read): { ready: Spendable[]; frozen: Spendable[] } {
  const ready: Spendable[] = [];
  const frozen: Spendable[] = [];
  for (const note of read.notes) {
    if (note.position === undefined) {
      continue;
    }
    const each = { id: `${note.serial.generation}:${note.serial.index}`, v: Number(note.amount), pos: note.position };
    if (note.state === 'spendable' || note.state === 'expiring') {
      ready.push(each);
    } else if (note.state === 'frozen') {
      frozen.push(each);
    }
  }
  return { ready, frozen };
}

/** The plan for a request from what is held: the confirm, or the short case. */
function planned(read: Read, req: ReadRequest, policy: Policy, layers: number): { confirm: ReturnType<typeof shownPlan> | undefined; short: ReturnType<typeof shortCase> | undefined; ready: number; resting: number; refused: Exclude<Planned, { ok: true }> | undefined } {
  const { ready, frozen } = spendables(read);
  const readyNow = sum(ready.map((note) => note.v));
  const restingNow = sum(frozen.map((note) => note.v));
  const p = plan(ready, req.outputs, policy, layers);
  if (p === undefined) {
    // amounts past what a JS number holds: say less, guess nothing
    return { confirm: undefined, short: undefined, ready: readyNow, resting: restingNow, refused: undefined };
  }
  if (p.ok) {
    return { confirm: shownPlan(p.parts, readyNow, restingNow, layers), short: undefined, ready: readyNow, resting: restingNow, refused: undefined };
  }
  return { confirm: undefined, short: shortCase(p, ready, frozen, req.outputs, policy, layers), ready: readyNow, resting: restingNow, refused: p };
}

/**
 * Reads a request from a link, a paste or the camera (§5.2): decode it,
 * read it, then tell your own, an unheld mint, a request sent to already,
 * and what can go.
 */
async function openAsk(text: string, via: 'camera' | 'link', focus = true): Promise<void> {
  const decoded = await decode(text);
  const fail = (words: string): void => {
    if (via === 'camera') {
      state.scanError = words;
      render();
      return;
    }
    if (state.route !== 'send' || state.send.step !== 'scan') {
      state.send = blankSend();
      forSend = undefined;
    }
    state.send.paste = text;
    state.send.error = words;
    setRoute('send', { hash: '#send', keep: true, focus: false });
    render();
    if (focus) {
      focusKey('req-link');
    }
  };
  if (!decoded.ok) {
    fail(linkProblemWords('ask', decoded.problem, decoded.kind));
    return;
  }
  if (decoded.link.kind !== 'ask') {
    fail(linkProblemWords('ask', 'kind'));
    return;
  }
  const ask = decoded.link;
  let req: ReadRequest;
  try {
    req = (await loadSdk()).readRequest(ask.json);
  } catch {
    fail(linkProblemWords('ask', 'request'));
    return;
  }
  if (via === 'camera') {
    stopCamera();
    state.camera = { kind: 'idle' };
    state.scanError = undefined;
  }
  // the ticket's small code: an empty stub if its link could not be written again here
  const link = await ticketLink(ask).catch(() => '');
  state.send = { ...blankSend(), via, at: Date.now(), ask, req };
  forSend = undefined;
  ticket = link;
  // your own request: signing it moves nothing, and two purses here share one store
  if (state.asks.some((each) => each.id === req.id)) {
    state.send.step = 'own';
    showSendStep(focus);
    return;
  }
  const record = state.mints.find((each) => each.mint === req.mint);
  if (record === undefined) {
    state.send.step = 'unknown';
    showSendStep(focus);
    void readUnknown();
    return;
  }
  state.send.record = record;
  if (ask.server !== record.server) {
    // a held mint is reached where it was added; the person should know the link named another server
    state.send.serverNote = serverNote(hostOf(ask.server), hostOf(record.server));
  }
  state.send.step = 'reading';
  showSendStep(focus);
  await readForSendStep(focus);
}

/** The ticket's link: the request's own, for its small code. */
let ticket = '';

/** Shows Send as set, with its heading focused after the person's step: the hash is #send, so a reload sends nothing. */
function showSendStep(focus = true): void {
  setRoute('send', { hash: '#send', keep: true, focus: false });
  render();
  window.scrollTo(0, 0);
  if (focus) {
    focusH1();
  }
}

/** The unknown mint's stamp: read from the server the link names, before anything is kept. */
async function readUnknown(): Promise<void> {
  const s = state.send;
  const ask = s.ask;
  const req = s.req;
  if (ask === undefined || req === undefined) {
    return;
  }
  s.readError = undefined;
  s.about = undefined;
  try {
    const about = await (await loadSdk()).readMint({ mint: req.mint, server: ask.server });
    if (state.send !== s) {
      return;
    }
    s.about = about;
  } catch (error) {
    if (state.send !== s) {
      return;
    }
    s.readError = say(error, 'read', { host: hostOf(ask.server) }).words;
  }
  if (state.route === 'send' && state.send === s) {
    render();
  }
}

/** Add this mint, from a request for it: keep it, then go on as for a held mint. */
async function addFromRequest(): Promise<void> {
  const s = state.send;
  const ask = s.ask;
  const req = s.req;
  const about = s.about;
  if (ask === undefined || req === undefined || about === undefined || s.adding) {
    return;
  }
  s.adding = true;
  render();
  try {
    await saveMint({ mint: req.mint, server: ask.server }, about);
  } catch (error) {
    s.adding = false;
    s.readError = say(error, 'open', { host: hostOf(ask.server) }).words;
    render();
    return;
  }
  s.adding = false;
  s.record = state.mints.find((each) => each.mint === req.mint);
  s.step = 'reading';
  render();
  focusH1();
  await readForSendStep();
}

/** Reads what can go for the request, then shows sent already, confirm or not enough ready; its heading focused after the person's step. */
async function readForSendStep(focus = true): Promise<void> {
  const s = state.send;
  const record = s.record;
  const req = s.req;
  if (record === undefined || req === undefined) {
    return;
  }
  s.readError = undefined;
  let fs: ForSend;
  try {
    fs = await (await loadSdk()).readForSend(record.server, req.mint);
  } catch (error) {
    if (state.send !== s) {
      return;
    }
    const said = say(error, 'send', { host: hostOf(record.server) });
    heed(said);
    s.step = 'reading';
    s.readError = said.words;
    render();
    return;
  }
  if (state.send !== s) {
    return;
  }
  setLastLayer(fs.layers);
  const holdings = holdingsFrom(fs.read.info, fs.read.notes, fs.read.total, fs.read.firstExpiry, fs.read.skew, arrivals(req.mint), fs.read.moving);
  forSend = { fs, holdings, link: ticket };
  s.mintsReady = fs.mintLayer !== undefined && fs.mintLayer < fs.layers - 1;
  // sent already: kept with its hand-back, still on its way, or no longer kept but in the log
  const before = fs.sends.get(req.id);
  if (before !== undefined || state.log.some((each) => each.kind === 'sent' && each.ask === req.id)) {
    s.step = 'already';
    s.already = sentBefore(req, before);
    render();
    if (focus) {
      focusH1();
    }
    return;
  }
  applyPlan(s, fs, req);
  render();
  if (focus) {
    focusH1();
  }
  if (s.step === 'short') {
    void loadSendFaucet(s, record);
  }
}

/**
 * What became of a send this purse made to `req` already: kept with its
 * hand-back, still on its way, turned away by the mint (nothing went), or
 * no longer kept. A request is sent to once, whatever became of it.
 */
function sentBefore(req: ReadRequest, kept: Kept | undefined): NonNullable<SendState['already']> {
  const entry = state.log.find((each) => each.kind === 'sent' && each.ask === req.id);
  let kind: NonNullable<SendState['already']>['kind'];
  if (kept !== undefined && kept.outgoing) {
    kind = 'outgoing';
  } else if (kept !== undefined && kept.owed.length > 0) {
    kind = 'kept';
  } else {
    kind = entry?.state === 'turned' ? 'turned' : 'gone';
  }
  return { kind, t: entry?.t, amount: entry?.went ?? entry?.amount ?? req.amount, id: req.id };
}

/** Sets confirm or short from a plan of what is held now. */
function applyPlan(s: SendState, fs: ForSend, req: ReadRequest): void {
  const p = planned(fs.read, req, fs.policy, fs.layers);
  s.ready = p.ready;
  s.resting = p.resting;
  if (p.short !== undefined) {
    s.step = 'short';
    s.short = p.short;
    s.shown = undefined;
  } else {
    s.step = 'confirm';
    s.shown = p.confirm;
    s.short = undefined;
  }
}

/** The faucet at the request mint's server, for "not enough ready". */
async function loadSendFaucet(s: SendState, record: MintRecord): Promise<void> {
  try {
    const info = await (await loadSdk()).faucetAt(record.server, record.mint);
    if (state.send !== s || info === undefined) {
      return;
    }
    s.faucet = { info, taking: false, error: undefined };
    if (state.route === 'send' && s.step === 'short') {
      render();
    }
  } catch {
    // no faucet: receiving more is the way out
  }
}

/** The faucet's way out of "not enough ready": take, read again, plan again. */
async function shortFaucet(): Promise<void> {
  const s = state.send;
  const record = s.record;
  const faucet = s.faucet;
  if (record === undefined || faucet === undefined || faucet.taking) {
    return;
  }
  faucet.taking = true;
  faucet.error = undefined;
  render();
  try {
    await takeInto(record);
  } catch (error) {
    const said = say(error, 'take', { host: hostOf(record.server), perHour: faucet.info.perAddressPerHour });
    heed(said);
    faucet.taking = false;
    faucet.error = said.words;
    if (said.noFaucet === true) {
      s.faucet = undefined;
    }
    render();
    return;
  }
  faucet.taking = false;
  const took = state.home.took;
  // said here, not again on Holdings
  state.home.took = undefined;
  await readForSendStep();
  const holdings = forSend?.holdings;
  if (took !== undefined && holdings !== undefined) {
    const note = holdings.notes.find((each) => took.ids.includes(each.id));
    announce(tookWords(took.amount, note === undefined ? undefined : laneOf(note.pos, holdings.layers), holdings, Date.now()));
  }
}

// ---------- Send: sending ----------

let tearFrame = 0;

/** The tear, the countdown and the rule, on every frame while the sending view shows. */
function startTear(): void {
  cancelAnimationFrame(tearFrame);
  let slow = false;
  const loop = (): void => {
    const run = state.send.run;
    if (state.route !== 'send' || state.send.step !== 'sending' || run === undefined) {
      return;
    }
    tickSending(run, reduced());
    // past the time planned, the mint's line says it waits for the answer
    const late = !run.done && elapsed(run) > run.D;
    if (late !== slow) {
      slow = late;
      refreshSending(sendInput());
    }
    tearFrame = requestAnimationFrame(loop);
  };
  tearFrame = requestAnimationFrame(loop);
}

let sendingDrawn = false;

/** Redraws what an answer changes on the sending view, once per frame however many come. */
function sendingChanged(): void {
  if (sendingDrawn) {
    return;
  }
  sendingDrawn = true;
  requestAnimationFrame(() => {
    sendingDrawn = false;
    if (state.route === 'send' && state.send.step === 'sending') {
      refreshSending(sendInput());
    }
  });
}

/** Whether a plan's chosen notes and change are the same. */
const samePlan = (a: SendState['shown'], b: SendState['shown']): boolean =>
  JSON.stringify(a?.parts.map((part) => [part.note.id, part.change])) === JSON.stringify(b?.parts.map((part) => [part.note.id, part.change]));

/** The SDK's own refusal of a send, as a short case, from what is held now. */
function shortFrom(error: unknown, fs: ForSend, req: ReadRequest): ReturnType<typeof shortCase> | undefined {
  const fields = typeof error === 'object' && error !== null ? (error as Record<string, unknown>) : {};
  const details = (typeof fields['details'] === 'object' && fields['details'] !== null ? fields['details'] : {}) as Record<string, unknown>;
  const n = (key: string): number => Number(details[key] ?? 0);
  const { ready, frozen } = spendables(fs.read);
  let refused: Exclude<Planned, { ok: true }>;
  switch (fields['code']) {
    case 'unaffordable':
      refused = { ok: false, why: 'unaffordable', spendable: n('spendable'), needed: n('needed') };
      break;
    case 'fragmented':
      refused = { ok: false, why: 'fragmented', largest: n('largest'), atMost: details['atMost'] === undefined ? undefined : n('atMost') };
      break;
    case 'too_many_outputs':
      refused = { ok: false, why: 'too_many_outputs', maxOutputs: n('maxOutputs') };
      break;
    default:
      return undefined;
  }
  return shortCase(refused, ready, frozen, req.outputs, fs.policy, fs.layers);
}

/**
 * Send: check again under the lock, pay, then submit each transfer at its
 * time. Keep-alive gives way, here and in the other tabs, so the send has
 * the lock within seconds; its pass runs again once the send's end is
 * drawn, and settles what the send left outgoing.
 */
async function doSend(): Promise<void> {
  const s = state.send;
  const record = s.record;
  const req = s.req;
  const ask = s.ask;
  const ctx = forSend;
  if (s.step !== 'confirm' || s.signing || record === undefined || req === undefined || ask === undefined || ctx === undefined) {
    return;
  }
  s.signing = true;
  s.sendError = undefined;
  s.changed = false;
  render();
  await sending(() => sendTo(s, record, req, ask, ctx));
}

/** The send itself, once keep-alive has given way: see {@link doSend}. */
async function sendTo(s: SendState, record: MintRecord, req: ReadRequest, ask: AskLink, ctx: NonNullable<typeof forSend>): Promise<void> {
  const host = hostOf(record.server);
  const sendNow = new AbortController();
  let spent: string[] = [];
  let result: Sent | { stopped: 'changed' | 'short' | 'already' };
  try {
    result = await (await loadSdk()).send(record.server, ask.json, {
      check: (fresh) => {
        const p = planned(fresh, req, ctx.fs.policy, ctx.fs.layers);
        if (p.short !== undefined) {
          s.ready = p.ready;
          s.resting = p.resting;
          s.short = p.short;
          return 'short';
        }
        if (!samePlan(p.confirm, s.shown)) {
          s.shown = p.confirm;
          return 'changed';
        }
        return 'go';
      },
      signed: async (made) => {
        spent = made.spent;
        const t = Date.now();
        const entry = await logEntry({
          t,
          kind: 'sent',
          mint: req.mint,
          amount: req.amount,
          asked: req.amount,
          ask: req.id,
          server: record.server,
          state: 'sending',
          transfers: made.transfers,
          spent: made.spent,
          serials: [],
        }).catch(() => undefined);
        await setUsed(req.mint);
        tell();
        const now = performance.now();
        const run: Run = {
          t0: now,
          shownAt: now,
          n: made.transfers,
          afterSecs: made.afterSecs,
          D: Math.max(0, ...made.afterSecs) * 1000 + 1200,
          states: made.afterSecs.map(() => 'waiting'),
          answered: 0,
          refused: 0,
          nowSent: false,
          done: false,
          entry,
          amount: req.amount,
          parts: s.shown?.parts,
          landed: [],
          sendNow,
        };
        s.run = run;
        s.step = 'sending';
        s.signing = false;
        liveSend = s;
        if (state.route === 'send' && state.send === s) {
          render();
          window.scrollTo(0, 0);
          focusH1();
        }
      },
      progress: (progress) => {
        const run = s.run;
        if (run === undefined) {
          return;
        }
        run.states[progress.index] = progress.step;
        run.answered = progress.answered;
        run.refused = progress.refused;
        if (progress.step === 'answered') {
          run.landed.push(Date.now());
          if (run.n > 1) {
            announce(progressWords(progress.answered, progress.total));
          }
        }
        sendingChanged();
      },
      sendNow: sendNow.signal,
      signal: page.signal,
    });
  } catch (error) {
    if (s.run !== undefined) {
      // signed and logged: whatever failed after that, the send is the SDK's to finish, never "nothing was sent"
      await sendBroke(s, error, host);
      return;
    }
    await refusedToSend(s, error, record, req, host);
    return;
  }
  if ('stopped' in result) {
    s.signing = false;
    if (result.stopped === 'short') {
      s.step = 'short';
      s.shown = undefined;
      void loadSendFaucet(s, record);
    } else if (result.stopped === 'changed') {
      s.changed = true;
    }
    if (state.route === 'send' && state.send === s) {
      render();
      focusH1();
    }
    return;
  }
  await finishRun(s, result, spent, record);
}

/**
 * A send that failed once it was signed and logged: its transfers stay
 * outgoing, or were answered, and the SDK keeps them either way. The view
 * leaves the sending step for the waiting one, with the error's words,
 * then a settle sends what is outgoing and finds what was answered.
 */
async function sendBroke(s: SendState, error: unknown, host: string): Promise<void> {
  const run = s.run;
  if (run === undefined) {
    return;
  }
  run.done = true;
  s.signing = false;
  s.step = 'waiting';
  s.end = {
    amount: run.amount,
    t: entryById(run.entry)?.t ?? Date.now(),
    layerFull: false,
    lost: run.amount,
    lostTheirs: run.amount,
    words: say(error, 'settle', { host }).words,
    trying: false,
    entry: run.entry,
    hb: undefined,
    asPlanned: undefined,
  };
  const entry = entryById(run.entry);
  if (entry !== undefined && entry.state === 'sending') {
    await updateEntry({ ...entry, state: 'waiting' });
  }
  tell();
  if (state.route === 'send' && state.send === s) {
    render();
    window.scrollTo(0, 0);
    focusH1();
  }
  // the keeper's pass after the send settles what is outgoing, and the waiting view moves on with it (see keepServer)
}

/** When pay, or the check before it, refuses: the step it leads to, and its words. */
async function refusedToSend(s: SendState, error: unknown, record: MintRecord, req: ReadRequest, host: string): Promise<void> {
  const fields = typeof error === 'object' && error !== null ? (error as Record<string, unknown>) : {};
  s.signing = false;
  if (fields['name'] === 'AlreadySent' || (fields['name'] === 'WorglError' && fields['code'] === 'paid')) {
    const kept = fields['name'] === 'AlreadySent' ? (await (await loadSdk()).readForSend(record.server, req.mint).catch(() => undefined))?.sends.get(req.id) : undefined;
    s.step = 'already';
    s.already = sentBefore(req, kept);
  } else if (fields['name'] === 'WorglError' && (fields['code'] === 'unaffordable' || fields['code'] === 'fragmented' || fields['code'] === 'too_many_outputs')) {
    // the planner said it would go; the SDK says not: read again and say why, marked for the preview to catch
    const fs = await (await loadSdk()).readForSend(record.server, req.mint).catch(() => undefined);
    const c = fs === undefined ? undefined : shortFrom(error, fs, req);
    if (fs !== undefined && c !== undefined) {
      const p = planned(fs.read, req, fs.policy, fs.layers);
      forSend = {
        fs,
        holdings: holdingsFrom(fs.read.info, fs.read.notes, fs.read.total, fs.read.firstExpiry, fs.read.skew, arrivals(req.mint), fs.read.moving),
        link: ticket,
      };
      s.ready = p.ready;
      s.resting = p.resting;
      s.step = 'short';
      s.short = c;
      s.shown = undefined;
      s.plan = 'differ';
      void loadSendFaucet(s, record);
    } else {
      s.sendError = say(error, 'send', { host }).words;
    }
  } else {
    const said = say(error, 'send', { host });
    heed(said);
    s.sendError = said.words;
  }
  if (state.route === 'send' && state.send === s) {
    render();
    if (s.step !== 'confirm') {
      focusH1();
    }
  }
}

/** Where a send's change landed, and when its resting notes can move. */
function changeLands(back: readonly Came[], layers: number, skew: number): { lands: HandBack['backLands']; readyAt: number | undefined } {
  // only the notes placed by the mint's info say where the change landed
  const placedBack = back.filter((note) => note.pos !== undefined);
  if (placedBack.length === 0) {
    return { lands: undefined, readyAt: undefined };
  }
  const rest = placedBack.filter((note) => note.pos !== undefined && note.pos >= layers - 1);
  const lands = rest.length === 0 ? 'ready' : rest.length === placedBack.length ? 'rest' : 'mixed';
  const times = rest.map((note) => note.spendableAt).filter((t): t is bigint => t !== undefined);
  const readyAt = times.length === 0 ? undefined : (Number(times.reduce((a, b) => (b > a ? b : a))) - skew) * 1000;
  return { lands, readyAt };
}

/** Whether some `k` of the planned parts pay `went` between them: a partial send that went as planned. */
function partOfPlan(parts: readonly { theirs: number[] }[], went: number): boolean {
  const values = parts.map((part) => sum(part.theirs));
  if (values.length > 16) {
    return false;
  }
  for (let mask = 1; mask < 1 << values.length; mask++) {
    let total = 0;
    values.forEach((v, i) => {
      if ((mask & (1 << i)) !== 0) {
        total += v;
      }
    });
    if (total === went) {
      return true;
    }
  }
  return false;
}

/** The send's end: its step by what the transfers came to, its log entry, its hand-back (§5.7, Finish). */
async function finishRun(s: SendState, result: Sent, spent: readonly string[], record: MintRecord): Promise<void> {
  const run = s.run;
  const req = s.req;
  if (run === undefined || req === undefined) {
    return;
  }
  run.done = true;
  sendingChanged();
  const showing = performance.now() - run.shownAt;
  if (showing < 1200) {
    await sleep(1200 - showing);
  }
  const { submitted, back, backValue, went } = result;
  const A = req.amount;
  const kept = submitted.refused.some((refusal) => refusal.outcome === 'kept') || submitted.unsent.length > 0;
  const dropped = submitted.refused.filter((refusal) => refusal.outcome === 'dropped' && refusal.atStake !== undefined);
  const cancelled = submitted.refused.filter((refusal) => refusal.outcome === 'cancelled');
  let step: SendState['step'];
  let logState: NonNullable<LogEntry['state']>;
  if (kept) {
    step = 'waiting';
    logState = 'waiting';
  } else if (dropped.length > 0) {
    step = 'lost';
    logState = 'lost';
  } else if (cancelled.length > 0 && went > 0) {
    step = 'handback';
    logState = 'partial';
  } else if (cancelled.length > 0) {
    step = 'turned';
    logState = 'turned';
  } else {
    step = 'handback';
    logState = 'handback';
  }
  const { lost, theirs: lostTheirs } = droppedValue(dropped);
  const t2 = Date.now();
  const entry = entryById(run.entry);
  if (entry !== undefined) {
    const { went: _was, ...rest } = entry;
    void _was;
    await updateEntry({
      ...(kept ? rest : { ...entry, went }),
      state: logState,
      amount: kept ? A : went,
      back: backValue,
      serials: back.map((note) => note.id),
      t2,
      ...(logState === 'lost' ? { lost, lostTheirs } : {}),
    });
  }
  if (went > 0) {
    state.home.delta = { v: -went, t: t2 };
  }
  arrived(back.map((note) => note.id), t2);
  const layers = forSend?.fs.layers ?? 4;
  const skew = forSend?.fs.read.skew ?? 0;
  const { lands, readyAt } = changeLands(back, layers, skew);
  let hb: HandBack | undefined;
  if (submitted.theirs.length > 0) {
    // the SDK keeps their notes either way: a link that could not be written says so, and Send's list has it
    const link = await takeLinkOf(record.server, req.mint, req.id, went, submitted.theirs);
    hb = {
      id: req.id,
      server: record.server,
      mint: req.mint,
      amount: went,
      asked: A,
      link,
      t: entry?.t ?? Date.now(),
      landed: t2,
      back: backValue,
      backLands: lands,
      readyAt,
      spent: spent.length,
      partial: logState === 'partial',
      turned: cancelled.length,
      went,
    };
  }
  const plannedIds = s.shown?.chosen.map((note) => note.id).sort().join() ?? '';
  const parts = s.shown?.parts ?? [];
  const asPlanned = s.shown !== undefined && [...spent].sort().join() === plannedIds;
  s.plan =
    s.shown === undefined
      ? undefined
      : (logState === 'handback' && asPlanned && went === A) || (logState === 'partial' && partOfPlan(parts, went))
        ? 'match'
        : 'differ';
  s.hb = hb;
  s.again = false;
  s.step = step;
  s.end = {
    amount: A,
    t: entry?.t ?? Date.now(),
    layerFull: cancelled.some((refusal) => (refusal.error as { code?: unknown } | undefined)?.code === 'layer_full'),
    lost,
    lostTheirs,
    words: kept ? keptRefusalWords(submitted.refused) : undefined,
    trying: false,
    entry: run.entry,
    hb: step === 'lost' ? hb : undefined,
    asPlanned,
  };
  tell();
  void loadHandBacks();
  void refreshHome();
  const here = state.route === 'send' && state.send === s;
  if (here) {
    render();
    window.scrollTo(0, 0);
    focusH1();
    if (step === 'handback') {
      announce(sentWords(went, false));
    }
  } else if (step === 'handback') {
    s.unseen = true;
    announce(sentWords(went, true));
  }
  // what was kept for later, the keeper's pass after the send settles
}

/** The words of a kept transfer's last error, when it has one. */
function keptRefusalWords(refused: Settled['submitted']['refused']): string | undefined {
  const last = refused.find((refusal) => refusal.outcome === 'kept');
  if (last === undefined || state.send.record === undefined) {
    return undefined;
  }
  return say(last.error, 'settle', { host: hostOf(state.send.record.server) }).words;
}

/** Send the rest now: every waiting transfer goes at once, and the time left is what their answers take. */
function sendRestNow(): void {
  const run = state.send.run;
  if (run === undefined || run.nowSent) {
    return;
  }
  run.nowSent = true;
  run.sendNow.abort();
  const el = elapsed(run);
  run.D = el + (run.n - run.answered) * 1200;
  refreshSending(sendInput());
}

// ---------- Send: hand-backs kept ----------

/** Reads every payment kept with its hand-back, across servers, and joins each to its send's log entry. */
async function loadHandBacks(): Promise<void> {
  let loaded: Sdk;
  try {
    loaded = await loadSdk();
  } catch {
    return;
  }
  const out: KeptHandBack[] = [];
  for (const server of new Set(state.mints.map((record) => record.server))) {
    try {
      for (const kept of await loaded.handBacks(server)) {
        out.push({ server, kept, entry: state.log.find((entry) => entry.kind === 'sent' && entry.ask === kept.id) });
      }
    } catch {
      // that server's store did not open: its hand-backs show once it does
    }
  }
  out.sort((a, b) => (b.entry?.t ?? -1) - (a.entry?.t ?? -1));
  const was = JSON.stringify(state.handBacks.map((each) => [each.kept.id, each.kept.owed.length, each.kept.outgoing]));
  state.handBacks = out;
  const moved = await reconcile(loaded);
  if (moved || was !== JSON.stringify(out.map((each) => [each.kept.id, each.kept.owed.length, each.kept.outgoing]))) {
    if (state.route === 'home' || (state.route === 'send' && state.send.step === 'scan')) {
      render();
    }
  }
}

/**
 * Sends whose log entry still says sending or waiting, though no tab sends
 * them: answered meanwhile (kept with owed notes, nothing outgoing) is a
 * hand-back; no longer kept at all, as the store itself says, is done.
 * Whether any entry moved.
 */
async function reconcile(loaded: Sdk): Promise<boolean> {
  const live = state.send.run !== undefined && !state.send.run.done ? state.send.run.entry : undefined;
  const gone: LogEntry[] = [];
  let moved = false;
  for (const entry of state.log) {
    if (entry.kind !== 'sent' || (entry.state !== 'sending' && entry.state !== 'waiting') || entry.id === live) {
      continue;
    }
    const kept = state.handBacks.find((each) => each.kept.id === entry.ask);
    if (kept !== undefined && !kept.kept.outgoing && kept.kept.owed.length > 0) {
      const partial = entry.transfers !== undefined && kept.kept.transfers.length < entry.transfers;
      // nothing is outgoing, so its kept transfers were all answered: they say what went and what came back
      await updateEntry(answeredEntry(entry, kept.kept, partial ? 'partial' : 'handback'));
      moved = true;
    } else if (kept === undefined) {
      gone.push(entry);
    }
  }
  // neither a hand-back nor on its way: done once the store keeps no payment to it at all (its notes faded)
  const ids = new Map<string, Set<string> | undefined>();
  for (const entry of gone) {
    const server = entry.server ?? state.mints.find((record) => record.mint === entry.mint)?.server;
    if (server === undefined || entry.ask === undefined) {
      continue;
    }
    if (!ids.has(server)) {
      ids.set(server, await loaded.sendsKept(server).catch(() => undefined));
    }
    if (ids.get(server)?.has(entry.ask) === false) {
      await updateEntry({ ...entry, state: 'done' });
      moved = true;
    }
  }
  return moved;
}

/**
 * A sent entry once its transfers are answered: its state, and what went and
 * came back as the kept transfers say (the request's amount when they do
 * not read, or nothing for a part of it).
 */
function answeredEntry(entry: LogEntry, kept: Kept, state: 'handback' | 'partial', more: Partial<LogEntry> = {}): LogEntry {
  const { went: _went, back: _back, ...rest } = entry;
  void _went;
  void _back;
  const asked = entry.asked ?? entry.amount;
  const went = kept.went ?? (state === 'handback' ? asked : undefined);
  return {
    ...rest,
    state,
    ...(went === undefined ? {} : { went, amount: went }),
    ...(kept.back === undefined ? {} : { back: kept.back }),
    ...more,
  };
}

/**
 * A hand-back's link: the purse's address with the receiver's notes after
 * `#take=`, and the tokens they carry as their own bytes say. Empty when it
 * cannot be written; the SDK keeps the notes either way.
 */
async function takeLinkOf(server: string, mint: string, id: string, amount: number, notes: readonly string[]): Promise<string> {
  try {
    return `${base()}${linkFor('take', await encodeTake(server, mint, id, valueOf(notes) ?? amount, notes))}`;
  } catch {
    return '';
  }
}

/** A kept hand-back, drawn again from what the purse keeps of it. */
async function handBackOf(each: KeptHandBack): Promise<HandBack> {
  const { kept, entry, server } = each;
  // what it carries: its notes' own value; failing that, what the log or the kept transfers say
  const amount = valueOf(kept.owed) ?? entry?.went ?? kept.went ?? kept.amount;
  const link = await takeLinkOf(server, kept.mint, kept.id, amount, kept.owed);
  const holdings = state.home.mint === kept.mint ? state.home.data?.holdings : forSend?.fs.read.info.issuer === kept.mint ? forSend.holdings : undefined;
  const ids = entry?.serials ?? [];
  const notes = holdings?.notes.filter((note) => ids.includes(note.id)) ?? [];
  const rest = holdings === undefined ? [] : notes.filter((note) => laneOf(note.pos, holdings.layers) === 'rest');
  const times = rest.map((note) => note.readyAt).filter((t): t is number => t !== undefined);
  return {
    id: kept.id,
    server,
    mint: kept.mint,
    amount,
    asked: entry?.asked ?? kept.amount,
    link,
    t: entry?.t ?? Date.now(),
    landed: entry?.t2 ?? entry?.t ?? Date.now(),
    back: entry?.back ?? kept.back,
    backLands: notes.length === 0 ? undefined : rest.length === 0 ? 'ready' : rest.length === notes.length ? 'rest' : 'mixed',
    readyAt: times.length === 0 ? undefined : Math.max(...times),
    spent: entry?.spent?.length ?? 1,
    partial: entry?.state === 'partial',
    turned: entry?.transfers === undefined ? 0 : Math.max(0, entry.transfers - kept.transfers.length),
    went: amount,
  };
}

/** Shows a kept hand-back again: from Send's list, a Recent row, or Show the hand-back. */
async function showHandBackAgain(id: string): Promise<void> {
  if (!state.handBacks.some((each) => each.kept.id === id)) {
    await loadHandBacks();
  }
  const each = state.handBacks.find((kept) => kept.kept.id === id && kept.kept.owed.length > 0);
  if (each === undefined) {
    return;
  }
  const hb = await handBackOf(each);
  const record = state.mints.find((rec) => rec.mint === each.kept.mint);
  state.send = { ...blankSend(), step: 'handback', hb, again: true, record, at: hb.t };
  forSend = undefined;
  setRoute('send', { hash: '#send', keep: true, focus: false });
  render();
  window.scrollTo(0, 0);
  focusH1();
}

/** A send that is not this tab's to follow: its waiting or lost view, from its log entry. */
function showEnded(entry: LogEntry, step: 'waiting' | 'lost'): void {
  const record = state.mints.find((each) => each.mint === entry.mint);
  const kept = state.handBacks.find((each) => each.kept.id === entry.ask && each.kept.owed.length > 0);
  state.send = {
    ...blankSend(),
    step,
    record,
    end: {
      amount: entry.asked ?? entry.amount,
      t: entry.t,
      layerFull: false,
      lost: entry.lost ?? entry.asked ?? entry.amount,
      lostTheirs: entry.lostTheirs ?? entry.asked ?? entry.amount,
      words: undefined,
      trying: false,
      entry: entry.id,
      hb: undefined,
      asPlanned: undefined,
    },
  };
  forSend = undefined;
  setRoute('send', { hash: '#send', keep: true, focus: false });
  render();
  window.scrollTo(0, 0);
  focusH1();
  if (kept !== undefined && step === 'lost') {
    void handBackOf(kept).then((hb) => {
      if (state.send.end !== undefined && state.send.end.entry === entry.id) {
        state.send.end.hb = hb;
        render();
      }
    });
  }
}

/** A Recent row that opens something: the live send, its waiting view, its hand-back, or what was lost. */
function openRow(entry: LogEntry): void {
  const s = liveSend;
  if (entry.state === 'sending' && s?.run !== undefined && s.run.entry === entry.id && s.step === 'sending') {
    state.send = s;
    setRoute('send', { keep: true });
    return;
  }
  if (entry.state === 'sending' || entry.state === 'waiting') {
    showEnded(entry, 'waiting');
  } else if (entry.state === 'lost') {
    showEnded(entry, 'lost');
  } else if (entry.ask !== undefined) {
    void showHandBackAgain(entry.ask);
  }
}

// ---------- settling what a send kept for later ----------

/** What a settle came to, written to each send's log entry (§5.11). */
async function applySettled(server: string, settled: Settled): Promise<void> {
  const given = new Set<string>();
  for (const sendOutcome of settled.sends) {
    const entry = state.log.find(
      (each) => each.kind === 'sent' && each.ask === sendOutcome.id && (each.state === 'sending' || each.state === 'waiting'),
    );
    if (entry === undefined) {
      continue;
    }
    const t2 = Date.now();
    const asked = entry.asked ?? entry.amount;
    const dropped = sendOutcome.refused.filter((refusal) => refusal.outcome === 'dropped' && refusal.atStake !== undefined);
    if (sendOutcome.kept?.outgoing === true) {
      await updateEntry({ ...entry, state: 'waiting' });
      const words = sendOutcome.refused.find((refusal) => refusal.outcome === 'kept');
      waitingWords.set(entry.id ?? -1, words === undefined ? undefined : say(words.error, 'settle', { host: hostOf(server) }).words);
    } else if (dropped.length > 0) {
      const { lost, theirs: lostTheirs } = droppedValue(dropped);
      await updateEntry({ ...entry, state: 'lost', lost, lostTheirs });
    } else if (sendOutcome.kept !== undefined && sendOutcome.kept.owed.length > 0) {
      // the change that came for this sendOutcome's mint; several of one mint at once cannot be told apart
      const fresh = given.has(sendOutcome.mint) ? [] : (settled.fresh.get(sendOutcome.mint) ?? []);
      given.add(sendOutcome.mint);
      const partial = entry.transfers !== undefined && sendOutcome.kept.transfers.length < entry.transfers;
      const serials = fresh.map((note) => note.id);
      const settledEntry = answeredEntry(entry, sendOutcome.kept, partial ? 'partial' : 'handback', { serials, t2 });
      await updateEntry(settledEntry);
      arrived(serials, t2);
      announce(settledWords(settledEntry.went ?? asked));
    } else if (sendOutcome.refused.length > 0 && sendOutcome.refused.every((refusal) => refusal.outcome === 'cancelled')) {
      await updateEntry({ ...entry, state: 'turned' });
    } else if (sendOutcome.kept === undefined) {
      await updateEntry({ ...entry, state: 'done' });
    }
  }
}

/** The last words of a settle for each waiting entry, by its id. */
const waitingWords = new Map<number, string | undefined>();

/**
 * After a settle: the waiting view moves on as the send's end would have.
 * `focus` only when the person asked for it (Try again now): a settle on
 * open or on visible never moves focus.
 */
function afterSettle(focus: boolean): void {
  const s = state.send;
  if (s.step !== 'waiting' || s.end === undefined) {
    if (state.route === 'home') {
      render();
    }
    return;
  }
  const entry = entryById(s.end.entry);
  if (entry === undefined) {
    return;
  }
  if (entry.state === 'handback' || entry.state === 'partial') {
    if (entry.ask !== undefined) {
      const ask = entry.ask;
      void (async () => {
        const each = state.handBacks.find((kept) => kept.kept.id === ask && kept.kept.owed.length > 0);
        if (each === undefined) {
          return;
        }
        const hb = await handBackOf(each);
        // a second settle's news of the same send finds it shown already
        if (state.send !== s || s.step !== 'waiting') {
          return;
        }
        s.hb = { ...hb, t: entry.t };
        s.again = false;
        s.step = 'handback';
        // settled later: the planner was right if pay spent its notes and all of them went
        const asPlanned = s.end?.asPlanned;
        s.plan = asPlanned === undefined ? undefined : asPlanned && entry.state === 'handback' ? 'match' : 'differ';
        if (state.route === 'send') {
          render();
          if (focus) {
            focusH1();
          }
          announce(sentWords(hb.amount, false));
        } else {
          s.unseen = true;
          announce(sentWords(hb.amount, true));
        }
      })();
    }
  } else if (entry.state === 'lost' || entry.state === 'turned') {
    s.step = entry.state;
    s.end = { ...s.end, lost: entry.lost ?? s.end.lost, lostTheirs: entry.lostTheirs ?? s.end.lostTheirs };
    if (state.route === 'send') {
      render();
      if (focus) {
        focusH1();
      }
    }
  } else if (entry.state === 'waiting') {
    s.end = { ...s.end, words: waitingWords.get(entry.id ?? -1) ?? s.end.words };
    if (state.route === 'send') {
      render();
    }
  }
}

/** Try again now, on the waiting view: settle that server, waiting for its lock. */
async function retryWaiting(): Promise<void> {
  const s = state.send;
  const end = s.end;
  if (end === undefined || end.trying) {
    return;
  }
  const entry = entryById(end.entry);
  const server = entry?.server ?? s.record?.server;
  if (server === undefined) {
    return;
  }
  end.trying = true;
  end.words = undefined;
  render();
  let words: string | undefined;
  try {
    const loaded = await loadSdk();
    const settled = await sending(() => loaded.retrySettle(server, page.signal));
    if (settled !== undefined) {
      await applySettled(server, settled);
    }
  } catch (error) {
    words = say(error, 'settle', { host: hostOf(server) }).words;
  }
  end.trying = false;
  await loadHandBacks();
  tell();
  void refreshHome();
  if (state.send !== s) {
    return;
  }
  const now = entryById(end.entry);
  if (now?.state === 'waiting') {
    end.words = words ?? waitingWords.get(now.id ?? -1) ?? say(new TypeError('no answer'), 'settle', { host: hostOf(server) }).words;
    render();
    return;
  }
  afterSettle(true);
}

// ---------- Send's input ----------

function sendInput(): SendInput {
  const s = state.send;
  const req = s.req;
  const held = req === undefined ? s.record : state.mints.find((each) => each.mint === req.mint);
  const record = shown();
  const host = hostOf(s.ask?.server ?? held?.server ?? '');
  const homeHoldings = record !== undefined && state.home.mint === record.mint ? state.home.data?.holdings : undefined;
  const holdings = forSend?.holdings;
  const scanNotes = homeHoldings === undefined ? [] : sliceNotes(homeHoldings);
  const now = Date.now();
  const empty = homeHoldings !== undefined && homeHoldings.notes.length === 0 && homeHoldings.total === 0;
  const ticketed = s.step !== 'scan' && s.step !== 'sending' && s.step !== 'handback';
  return {
    state: s,
    bar:
      held !== undefined
        ? { mint: held.mint, name: held.name }
        : req !== undefined
          ? { mint: req.mint, name: host }
          : { mint: record?.mint ?? '', name: record?.name ?? '' },
    name: held?.name ?? host,
    link: ticket === '' ? undefined : ticket,
    qr: ticketed || s.step === 'handback' ? qrState() : qr,
    scan: {
      ready: homeHoldings === undefined ? 0 : laneTotal(homeHoldings, 'fade') + laneTotal(homeHoldings, 'ready'),
      resting: homeHoldings === undefined ? 0 : laneTotal(homeHoldings, 'rest'),
      notes: scanNotes.filter((note) => note.lane === 'ready'),
      faucet: empty ? (state.home.data?.faucet === undefined ? undefined : Number(state.home.data.faucet.amount)) : undefined,
    },
    notes: holdings === undefined ? [] : sliceNotes(holdings),
    at: atWords(holdings ?? homeHoldings),
    kept: state.handBacks
      .filter((each) => each.kept.owed.length > 0)
      .map((each) => ({
        key: each.kept.id,
        amount: valueOf(each.kept.owed) ?? each.entry?.went ?? each.kept.went ?? each.kept.amount,
        t: each.entry?.t,
      })),
    camera: state.camera,
    scanError: state.scanError,
    now,
    actions: sendActions,
  };
}

const sendActions: SendInput['actions'] = {
  pasteChanged: (text) => {
    state.send.paste = text;
  },
  open: () => void openAsk(state.send.paste, 'link'),
  camera,
  cameraOff: () => cameraOff(),
  showKept: (key) => void showHandBackAgain(key),
  retryRead: () => {
    if (state.send.step === 'unknown') {
      state.send.readError = undefined;
      render();
      void readUnknown();
    } else {
      state.send.readError = undefined;
      render();
      void readForSendStep();
    }
  },
  add: () => void addFromRequest(),
  send: () => void doSend(),
  sendNow: sendRestNow,
  takeFaucet: () => void shortFaucet(),
  showHandBack: () => {
    const s = state.send;
    if (s.step === 'lost' && s.end?.hb !== undefined) {
      s.hb = s.end.hb;
      s.again = true;
      s.step = 'handback';
      render();
      focusH1();
      return;
    }
    const id = s.already?.id ?? s.req?.id;
    if (id !== undefined) {
      void showHandBackAgain(id);
    }
  },
  showSend: () => {
    const id = state.send.already?.id;
    const entry = state.log.find((each) => each.kind === 'sent' && each.ask === id);
    if (entry !== undefined) {
      openRow({ ...entry, state: entry.state === 'sending' ? 'sending' : 'waiting' });
    }
  },
  showOwn: () => {
    const id = state.send.req?.id;
    const ask = state.asks.find((each) => each.id === id);
    if (ask !== undefined && ask.state === 'open') {
      state.receive = requestState(ask);
      setRoute('receive', { keep: true });
    } else {
      setRoute('receive');
    }
  },
  retrySettle: () => void retryWaiting(),
};

// ---------- other tabs ----------

const channel = typeof BroadcastChannel === 'function' ? new BroadcastChannel('purse') : undefined;

/** Tells the purse's other tabs that this one changed something. */
function tell(): void {
  channel?.postMessage('changed');
}

/** What another tab's keep-alive says on the channel, or its send: that this tab's pass should give way. */
type KeepMessage =
  | { kind: 'keeping'; mint: string; amount: number; done: number; total: number }
  | { kind: 'kept'; mint: string }
  | { kind: 'ask' }
  | { kind: 'yield' };

const count = (n: unknown): n is number => typeof n === 'number' && Number.isInteger(n) && n >= 0;

/** A keep-alive message, if `data` is one; anything else is read as a change. */
function keepMessage(data: unknown): KeepMessage | undefined {
  if (typeof data !== 'object' || data === null || !('kind' in data)) {
    return undefined;
  }
  const m = data as Record<string, unknown>;
  if (m['kind'] === 'ask' || m['kind'] === 'yield') {
    return { kind: m['kind'] };
  }
  if (typeof m['mint'] !== 'string') {
    return undefined;
  }
  if (m['kind'] === 'kept') {
    return { kind: 'kept', mint: m['mint'] };
  }
  if (m['kind'] === 'keeping' && count(m['amount']) && count(m['done']) && count(m['total'])) {
    return { kind: 'keeping', mint: m['mint'], amount: m['amount'], done: m['done'], total: m['total'] };
  }
  return undefined;
}

/**
 * Another tab said something: its run moved on (draw it, and watch it), its
 * run is over, it asks how this tab's run stands, it is about to send (this
 * tab's pass stops at its next safe step, so the send has the lock within
 * seconds, and that tab's next pass settles what this one left), or it
 * changed something (read the stores and the records again).
 */
async function heard(data: unknown): Promise<void> {
  const message = keepMessage(data);
  if (message?.kind === 'yield') {
    await keeper.stop();
    return;
  }
  if (message?.kind === 'ask') {
    const progress = state.keep.progress;
    const record = shown();
    if (keeper.running && progress !== undefined && record !== undefined) {
      postKeeping(record.mint, progress);
    }
    return;
  }
  if (message?.kind === 'keeping') {
    if (message.mint === shown()?.mint && !keeper.running && message.total > 0) {
      const { amount, done, total } = message;
      drawBack(() => {
        state.keep.watching = { amount, done, total, at: Date.now() };
      });
      keeper.watch();
    }
    return;
  }
  if (message?.kind === 'kept' && state.keep.watching !== undefined) {
    drawBack(() => {
      state.keep.watching = undefined;
    });
  }
  reopen();
  const records = state.records;
  if (records !== undefined) {
    try {
      state.mints = await records.mints();
      state.log = await records.log();
      state.asks = await records.asks();
    } catch {
      // keep what was read last
    }
  }
  syncShown();
  await loadHandBacks();
  if (state.route === 'home') {
    await refreshHome();
  } else if (state.route === 'first' || (state.route === 'send' && state.send.step === 'scan') || (state.route === 'receive' && state.receive.step === 'ask')) {
    render();
    void refreshHome();
  }
}

// ---------- start ----------

/** Words for records that did not open. */
function recordsWords(error: unknown): string {
  const name = error instanceof DOMException ? error.name : '';
  if (name === 'NotSupportedError') {
    return say(error, 'open', { host: '' }).words;
  }
  if (name === 'VersionError') {
    return 'A newer version of the purse saved here. Reload the page to update it.';
  }
  return 'This browser would not open the purse’s records. Close its other tabs, then reload the page.';
}

/** Whether the purse runs as a Home Screen app. */
function standalone(): boolean {
  try {
    return (
      matchMedia('(display-mode: standalone)').matches ||
      (navigator as Navigator & { standalone?: boolean }).standalone === true
    );
  } catch {
    return false;
  }
}

/** Escape, in order: the camera, the cancel question, Ask in smaller notes, then the tips. */
function escape(): boolean {
  if (state.camera.kind === 'running' || state.camera.kind === 'starting') {
    cameraOff();
    return true;
  }
  if (state.route === 'receive' && state.receive.confirmCancel && !state.receive.cancelling) {
    keepAsk();
    return true;
  }
  if (state.route === 'receive' && state.receive.step === 'ask' && state.receive.smaller) {
    state.receive.smaller = false;
    render();
    focusKey('ask-smaller');
    return true;
  }
  return false;
}

function listen(): void {
  document.addEventListener('click', (event) => {
    const target = event.target instanceof Element ? event.target : null;
    if (target === null) {
      return;
    }
    const link = target.closest('a[href^="#"]');
    if (link !== null) {
      event.preventDefault();
      navigate(link.getAttribute('href') ?? '#');
      return;
    }
    // a click outside the tip and what opens tips closes it; a tap the field answered is its own
    if (state.home.opened !== undefined && !fieldTook() && target.closest('.f-tip, .f-lanebtn, .num') === null) {
      closeTip(false);
    }
  });
  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') {
      return;
    }
    if (escape()) {
      return;
    }
    // the hover tip goes first, as content that came by hover must; then the open tip
    if (!dismissHover() && state.home.opened !== undefined) {
      closeTip(true);
    }
  });
  window.addEventListener('hashchange', () => {
    const hash = location.hash;
    if (flowHash(hash)) {
      openFlow(hash);
    } else if (isMintHash(hash)) {
      openLink(hash, true);
    } else if (routeFor(hash) !== state.route || hash !== `#${state.route}`) {
      // a hash for no view, or for one that needs a mint before there is one, is written as the view shown
      setRoute(routeFor(hash));
    }
  });
  let resized: ReturnType<typeof setTimeout> | undefined;
  let wide = window.innerWidth >= 1100;
  window.addEventListener('resize', () => {
    clearTimeout(resized);
    resized = setTimeout(() => {
      // across the desktop width the other half of the view shows, whose pink began when it was hidden: draw it all
      if (wide !== window.innerWidth >= 1100) {
        wide = !wide;
        render();
      } else if (state.route === 'home') {
        redrawField();
      } else if (state.route === 'receive' || state.route === 'send') {
        fitQR();
        if (state.route === 'send' && state.send.step === 'confirm') {
          fitPre();
        }
      }
    }, 120);
  });
  void document.fonts?.ready.then(redrawField);
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      // keep-alive stops at the next safe step: a page in the background keeps nothing alive
      keeper.hide();
      // the camera's light goes off the moment the page is not seen; focus on its off button goes to the one that starts it
      if (state.camera.kind !== 'idle') {
        cameraOff(document.activeElement instanceof HTMLElement && document.activeElement.dataset['fk'] === 'cam-toggle');
      }
      return;
    }
    // another tab may have changed the store meanwhile
    reopen();
    keeping.forget();
    if (state.route === 'home' || state.route === 'receive' || state.route === 'send') {
      void refreshHome();
    }
    // on every route: what is held is kept, and what an abort left outgoing, a pass's or a send's, is settled
    keeper.show();
  });
  window.addEventListener('pagehide', () => {
    // the only thing that cuts a send short: what is on its way stays outgoing, settled on the next open; keep-alive stops at once
    page.abort();
    keeper.leave();
    stopCamera();
    state.camera = { kind: 'idle' };
  });
  // back from the back/forward cache, which skips start()
  window.addEventListener('pageshow', (event) => {
    if (!event.persisted) {
      return;
    }
    page = new AbortController();
    reopen();
    keeping.forget();
    if (state.route === 'home') {
      void refreshHome();
    }
    keeper.show();
  });
  channel?.addEventListener('message', (event) => void heard(event.data));

  // the needle creeps once a second, or jumps once a minute for less motion
  let moved = 0;
  setInterval(() => {
    const host = $('#field');
    if (host === null || state.route !== 'home' || document.hidden) {
      return;
    }
    const now = Date.now();
    if (reduced() && now - moved < 60_000) {
      return;
    }
    moved = now;
    moveNeedle(host);
  }, 1000);
  // and the mint is asked again each minute, for a rotation by hand, a pause, or another device's change; on the
  // first run, keep-alive looks again at what it keeps, as Holdings' read would have it do
  setInterval(() => {
    if (document.hidden) {
      return;
    }
    if (state.route === 'home') {
      void refreshHome();
    } else {
      keeper.poke();
    }
  }, 60_000);
}

/** Opens the records, picks the view and draws it; the SDK loads once that has painted. */
export async function start(): Promise<void> {
  state.standalone = standalone();
  const setting = import.meta.env.VITE_PLAYGROUND_LINK;
  if (setting !== undefined && setting !== '') {
    const parsed = parseMintLink(setting);
    if (parsed.ok) {
      state.playground = parsed.link;
    }
  }
  try {
    state.records = await Records.open();
    state.mints = await state.records.mints();
    state.log = await state.records.log();
    state.asks = await state.records.asks();
  } catch (error) {
    state.storeError = recordsWords(error);
  }
  syncShown();
  listen();
  const hash = location.hash;
  if (flowHash(hash)) {
    // a request or a hand-back opened by its link: its flow, reading it, is the first view drawn
    openFlow(hash, false);
  } else if (isMintHash(hash)) {
    openLink(hash, false);
  } else {
    const route = routeFor(hash);
    if (route === 'first') {
      if (state.storeError !== undefined) {
        state.first.error = state.storeError;
      } else if (state.playground !== undefined && state.mints.length === 0) {
        readPlayground(state.playground);
      }
    }
    setRoute(route, { focus: false });
  }
  requestAnimationFrame(() => {
    void loadSdk().then(
      async () => {
        // the mints already held are kept on every route; Holdings pokes once its first read is drawn, or has failed
        if (state.route !== 'home') {
          pokedOnOpen = true;
          keeper.poke();
        }
        // the hand-backs kept show at once; the keeper's first pass settles what a closed tab left outgoing
        await loadHandBacks();
      },
      () => undefined,
    );
  });
}
