// The app: its state, its two routes, what the person's actions do, what
// keeps Holdings current while it is open, and keep-alive's wiring: when
// the keeper runs, what other tabs hear of it, and how a run shows. The
// views (first.ts, home.ts) and the field (field.ts) are drawn from the
// state on every change; the SDK is reached only through purses.ts, which is
// loaded once the shell has painted.

import type { FaucetInfo, MintInfo } from 'worgl';

import { $, announce as speak, reduced } from './dom.ts';
import { type Said, linkWords, say, transient } from './errors.ts';
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
} from './field.ts';
import { type FirstInput, type FirstState, firstView } from './first.ts';
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
  savedHoldings,
} from './holdings.ts';
import { type Elsewhere, type HomeData, type HomeInput, type KeepShown, homeView, placeholder, renderBack } from './home.ts';
import type { Moved } from './keep.ts';
import { Keeper, type KeptServer, Keeping, type Shown } from './keeper.ts';
import { type MintLink, hostOf, isMintHash, mintHash, parseMintLink } from './link.ts';
import type { About, Read } from './purses.ts';
import { type LogEntry, type MintRecord, Records, type SavedInfo } from './records.ts';
import { keptWords, rotatedWords, tookWords, unreachableFor } from './words.ts';

type Sdk = typeof import('./purses.ts');

/** The two views. */
type Route = 'first' | 'home';

/** What Holdings shows and what it is doing. */
interface HomeState {
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
  /** A take to announce once the read after it is drawn. */
  took: { amount: number; ids: string[] } | undefined;
  /** What the last read drew, so that a read that finds nothing new draws nothing (and moves no focus). */
  drawn: string | undefined;
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
  /** The build's playground, when the build names one. */
  playground: undefined as MintLink | undefined,
  standalone: false,
  first: blankFirst(),
  home: {
    data: undefined,
    oldest: undefined,
    error: undefined,
    opened: undefined,
    delta: undefined,
    pink: new Map(),
    taking: false,
    arriving: [],
    took: undefined,
    drawn: undefined,
    alerted: undefined,
  } as HomeState,
  keep: {
    progress: undefined,
    watching: undefined,
    hops: new Map(),
    hopped: new Map(),
    lanes: new Set(),
    lanesSig: undefined,
  } as KeepState,
};

/** The mint Holdings shows: the first one added, until the Mints screen comes. */
const shown = (): MintRecord | undefined => state.mints[0];

const isPlayground = (link: MintLink): boolean =>
  state.playground !== undefined && state.playground.mint === link.mint && state.playground.server === link.server;

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

// ---------- drawing ----------

let observer: ResizeObserver | undefined;

/** Draws the current view, keeping focus, a text field's selection and the scroll where they were. */
function render(): void {
  const app = document.getElementById('app');
  if (app === null) {
    return;
  }
  const active = document.activeElement;
  const key = active instanceof HTMLElement || active instanceof SVGElement ? active.dataset['fk'] : undefined;
  // on Holdings drawn again, where focus goes when what had it is not drawn: a number whose notes all moved, a tip that closed
  const again = state.route === 'home' && document.documentElement.dataset['view'] === 'home';
  const near = key === undefined || !again ? [] : nearKeys(key, active);
  const selection =
    active instanceof HTMLInputElement ? ([active.selectionStart, active.selectionEnd] as const) : undefined;
  const y = window.scrollY;
  const record = shown();
  const view = state.route === 'home' && record !== undefined ? homeView(homeInput(record)) : firstView(firstInput());
  app.replaceChildren(view);
  document.documentElement.dataset['view'] = state.route;
  if (state.route === 'home' && record !== undefined) {
    mountField();
    state.home.drawn = shownSignature(Date.now());
    state.home.alerted = homeError(record)?.words;
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
    now: Date.now(),
    take: () => void takeHome(),
    toggleLane,
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
}

/** Focus goes to the new view's heading after a step of the person's, never on boot. */
function focusH1(): void {
  $('#app [data-fk="h1"]')?.focus({ preventScroll: true });
}

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

/** The view a hash asks for: Holdings once the purse holds a mint, else the first run. */
const routeFor = (hash: string): Route => (hash === '#first' || shown() === undefined ? 'first' : 'home');

/**
 * Shows `route`: the hash is replaced, not pushed; a new view scrolls to the
 * top and, after a step of the person's, takes focus at its heading.
 */
function setRoute(route: Route, options: { focus?: boolean; hash?: string } = {}): void {
  const next: Route = route === 'home' && shown() === undefined ? 'first' : route;
  const changed = next !== state.route;
  if (changed && next === 'home') {
    state.first = blankFirst();
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
  }
}

/** Follows an in-page link or a hash the person typed. */
function navigate(hash: string): void {
  if (isMintHash(hash)) {
    openLink(hash, true);
  } else {
    setRoute(routeFor(hash));
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
const shareable = (link: MintLink): string => `${location.origin}${location.pathname}${mintHash(link)}`;

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
  await records.putMint({
    mint: link.mint,
    server: link.server,
    name: isPlayground(link) ? 'Playground' : hostOf(link.server),
    unit: 'tokens',
    added: Date.now(),
    // nothing is held at a mint just added
    info: savedInfo(about.info, null),
  });
  state.mints = await records.mints();
  tell();
}

/** Add mint: keep it, and open Holdings. */
async function addFirst(): Promise<void> {
  const link = state.first.link ?? state.playground;
  const about = state.first.about;
  if (link === undefined || about === undefined || state.first.busy !== undefined) {
    return;
  }
  if (state.mints.some((record) => record.mint === link.mint)) {
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

/** Takes from the faucet at `link`'s server, logs it, and marks the new note to arrive in pink. */
async function takeInto(link: MintLink): Promise<void> {
  const loaded = await loadSdk();
  keepStorage();
  const { value, serials } = await loaded.take(link.server, link.mint);
  const t = Date.now();
  const amount = Number(value);
  // the note is kept by now: a log that cannot be written loses only the entry
  await logEntry({ t, kind: 'faucet', mint: link.mint, amount, serials }).catch(() => undefined);
  state.home.delta = { v: amount, t };
  for (const id of serials) {
    state.home.pink.set(id, t);
  }
  state.home.arriving.push(...serials);
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
 * Adds an entry to the purse's log and reads the log back. Keep-alive's
 * entry for a rotation joins the one the log has for it already (`spent`:
 * the notes its run moved, see `Records.addKept`).
 */
async function logEntry(entry: Omit<LogEntry, 'id'>, spent?: readonly Moved[]): Promise<void> {
  const records = state.records;
  if (records === undefined) {
    return;
  }
  if (entry.kind === 'kept' && entry.round !== undefined) {
    await records.addKept(entry, spent ?? []);
  } else {
    await records.addLog(entry);
  }
  state.log = await records.log();
}

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
    if (state.route === 'home' && shownSignature(Date.now()) !== state.home.drawn) {
      render();
    }
  })();
  askingFaucet = asking;
  void asking.finally(() => {
    if (askingFaucet === asking) {
      askingFaucet = undefined;
    }
  });
  return asking;
}

/** How long the first read of an empty purse waits for its faucet, so the Take button usually comes with it. */
const FAUCET_GRACE_MS = 800;

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
    if (state.route === 'home' && shownSignature(Date.now()) !== state.home.drawn) {
      render();
    }
    return;
  }
  if (shown()?.mint !== record.mint) {
    return;
  }
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
  arrive(state.home.arriving.splice(0));
  void keepInfo(record, read.info, firstFade(holdings));
  // fading tokens, or tokens left on their way, call for a pass; and the first read of a visit, to settle what is outgoing
  const calls = keeping.saw(record.mint, read);
  if (!finishing && (!pokedOnOpen || calls === 'due' || (calls === 'moving' && !keeper.running))) {
    pokedOnOpen = true;
    keeper.poke();
  }
  if (state.route !== 'home') {
    return;
  }
  if (holdings.notes.length === 0) {
    const asked = askFaucet(record);
    if (before === undefined) {
      await Promise.race([asked, new Promise((resolve) => setTimeout(resolve, FAUCET_GRACE_MS))]);
      if (state.route !== 'home' || shown()?.mint !== record.mint) {
        return;
      }
    }
  }
  const now = Date.now();
  planRotation();
  const seen = shownSignature(now);
  if (seen === state.home.drawn && state.home.took === undefined && !rotated) {
    return;
  }
  render();
  const took = state.home.took;
  if (took !== undefined) {
    state.home.took = undefined;
    const note = holdings.notes.find((each) => took.ids.includes(each.id));
    announce(tookWords(took.amount, note === undefined ? undefined : laneOf(note.pos, holdings.layers), holdings, now));
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
    await state.records.putMint({ ...record, info: saved });
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

// ---------- other tabs ----------

const channel = typeof BroadcastChannel === 'function' ? new BroadcastChannel('purse') : undefined;

/** Tells the purse's other tabs that this one changed something. */
function tell(): void {
  channel?.postMessage('changed');
}

/** What another tab's keep-alive says on the channel. */
type KeepMessage =
  | { kind: 'keeping'; mint: string; amount: number; done: number; total: number }
  | { kind: 'kept'; mint: string }
  | { kind: 'ask' };

const count = (n: unknown): n is number => typeof n === 'number' && Number.isInteger(n) && n >= 0;

/** A keep-alive message, if `data` is one; anything else is read as a change. */
function keepMessage(data: unknown): KeepMessage | undefined {
  if (typeof data !== 'object' || data === null || !('kind' in data)) {
    return undefined;
  }
  const m = data as Record<string, unknown>;
  if (m['kind'] === 'ask') {
    return { kind: 'ask' };
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
 * run is over, it asks how this tab's run stands, or it changed something
 * (read the stores and the records again).
 */
async function heard(data: unknown): Promise<void> {
  const message = keepMessage(data);
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
    } catch {
      // keep what was read last
    }
  }
  if (state.route === 'home') {
    await refreshHome();
  } else {
    render();
  }
}

// ---------- keep-alive ----------

/** The lock a keep-alive pass holds, so one tab runs at a time; never the SDK store's own `worgl:<server URL>`. */
const TRANSFERS_LOCK = 'purse:transfers';
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

/** Runs `work` holding the lock `purse:transfers`, if no other tab holds it; false, without running it, if one does. */
async function withTransfersLock(work: () => Promise<void>): Promise<boolean> {
  if (!('locks' in navigator)) {
    // no Web Locks: the SDK's store cannot open either, and the pass's reads say so
    await work();
    return true;
  }
  return navigator.locks.request(TRANSFERS_LOCK, { ifAvailable: true }, async (lock) => {
    if (lock === null) {
      return false;
    }
    await work();
    return true;
  });
}

/** Says words in the live region for a run, only where the page shows it: Holdings. */
function announce(words: string): void {
  if (state.route === 'home') {
    speak(words);
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
  keepServer: async (server, work) => (await loadSdk()).keepServer(server, work),
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
  announce,
  finish: finishPass,
  reopen,
  now: () => Date.now(),
  random: () => Math.random(),
});

const keeper: Keeper = new Keeper({
  pass: (run) => keeping.pass(run),
  lock: withTransfersLock,
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
    announce(keptWords(kept, holdings === undefined ? undefined : comeBackAt(holdings, now), now));
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
    // the hover tip goes first, as content that came by hover must; then the open tip
    if (!dismissHover() && state.home.opened !== undefined) {
      closeTip(true);
    }
  });
  window.addEventListener('hashchange', () => {
    const hash = location.hash;
    if (isMintHash(hash)) {
      openLink(hash, true);
    } else if (routeFor(hash) !== state.route || hash !== `#${state.route}`) {
      // a hash for neither view, or for Holdings with no mint yet, is written as the view shown
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
      } else {
        redrawField();
      }
    }, 120);
  });
  void document.fonts?.ready.then(redrawField);
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      // keep-alive stops at the next safe step: a page in the background keeps nothing alive
      keeper.hide();
      return;
    }
    // another tab may have changed the store meanwhile
    reopen();
    keeping.forget();
    if (state.route === 'home') {
      void refreshHome();
    }
    // on every route: what is held is kept, and what an abort left is settled
    keeper.show();
  });
  // the page goes: stop at once; what is in hand is saved, and the next open settles the rest
  window.addEventListener('pagehide', () => keeper.leave());
  // back from the back/forward cache, which skips start()
  window.addEventListener('pageshow', (event) => {
    if (!event.persisted) {
      return;
    }
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
  } catch (error) {
    state.storeError = recordsWords(error);
  }
  listen();
  const hash = location.hash;
  if (isMintHash(hash)) {
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
      () => {
        // on the first run, the mints already held are kept too; Holdings pokes once its first read is drawn, or has failed
        if (state.route === 'first') {
          pokedOnOpen = true;
          keeper.poke();
        }
      },
      () => undefined,
    );
  });
}
