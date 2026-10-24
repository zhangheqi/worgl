// The app: its state, its two routes, what the person's actions do, and
// what keeps Holdings current while it is open. The views (first.ts,
// home.ts) and the field (field.ts) are drawn from the state on every
// change; the SDK is reached only through purses.ts, which is loaded once
// the shell has painted.

import type { FaucetInfo, MintInfo } from 'worgl';

import { $, announce, reduced } from './dom.ts';
import { type Said, linkWords, say } from './errors.ts';
import {
  type FieldInput,
  type Opened,
  arrive,
  bindPointer,
  dismissHover,
  drawMain,
  fieldTook,
  moveNeedle,
} from './field.ts';
import { type FirstInput, type FirstState, firstView } from './first.ts';
import { type Arrival, type LaneKey, fadedAt, holdingsFrom, laneOf } from './holdings.ts';
import { type HomeData, type HomeInput, homeView, placeholder } from './home.ts';
import { type MintLink, hostOf, isMintHash, mintHash, parseMintLink } from './link.ts';
import type { About, Read } from './purses.ts';
import { type LogEntry, type MintRecord, Records, type SavedInfo } from './records.ts';
import { rotatedWords, tookWords } from './words.ts';

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
  } as HomeState,
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
  }
  if (key !== undefined) {
    const el = app.querySelector<HTMLElement>(`[data-fk="${CSS.escape(key)}"]`);
    if (el !== null) {
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
    log: state.log.filter((entry) => entry.mint === record.mint),
    error: state.home.error,
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

/** What the purse keeps of a mint's info: enough to draw its lanes before it answers. */
const savedInfo = (info: MintInfo): SavedInfo => ({
  layers: info.layers,
  periodSecs: info.rotation === undefined ? undefined : Number(info.rotation.periodSecs),
  paused: info.paused,
  readAt: Date.now(),
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
    info: savedInfo(about.info),
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

/** Adds an entry to the purse's log and reads the log back. */
async function logEntry(entry: Omit<LogEntry, 'id'>): Promise<void> {
  const records = state.records;
  if (records === undefined) {
    return;
  }
  await records.addLog(entry);
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

/** What the purse's log knows of each note of `mint`: when it came, and from where. */
function arrivals(mint: string): Map<string, Arrival> {
  const known = new Map<string, Arrival>();
  for (const entry of state.log) {
    if (entry.mint === mint && entry.kind === 'faucet') {
      for (const id of entry.serials) {
        known.set(id, { at: entry.t, source: 'faucet' });
      }
    }
  }
  return known;
}

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
    const loaded = await loadSdk();
    read = await loaded.read(record.server, record.mint);
    if (read.notes.some((note) => note.state === 'expired')) {
      const { lost, generation } = await loaded.prune(record.server, record.mint);
      if (lost > 0n && generation !== undefined) {
        const t = fadedAt(generation, read.info, read.skew, Date.now());
        await logEntry({ t, kind: 'faded', mint: record.mint, amount: Number(lost), serials: [] }).catch(() => undefined);
        tell();
      }
      read = await loaded.read(record.server, record.mint);
    }
  } catch (error) {
    const said = say(error, 'open', { host: hostOf(record.server) });
    heed(said);
    state.home.error = { words: said.words, at: 'read' };
    if (state.route === 'home' && shownSignature(Date.now()) !== state.home.drawn) {
      render();
    }
    return;
  }
  if (shown()?.mint !== record.mint) {
    return;
  }
  const before = state.home.data?.holdings;
  const holdings = holdingsFrom(read.info, read.notes, read.total, read.firstExpiry, read.skew, arrivals(record.mint));
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
  void keepInfo(record, read.info);
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
  return JSON.stringify([
    holdings?.total,
    holdings?.notes.map((note) => [note.id, note.pos, note.expiresAt, note.readyAt]),
    holdings?.schedule?.nextMs,
    holdings?.paused,
    holdings?.firstExpiry,
    state.home.data?.faucet?.amount.toString(),
    state.home.error?.words,
    state.log.length,
    state.log[0]?.id,
    delta !== undefined && now - delta.t < 60 * 60_000,
    new Date(now).toDateString(),
  ]);
}

/** Saves the mint's info when its shape changed, so that the next open draws its lanes at once. */
async function keepInfo(record: MintRecord, info: MintInfo): Promise<void> {
  const saved = savedInfo(info);
  const was = record.info;
  if (
    state.records === undefined ||
    (was !== undefined && was.layers === saved.layers && was.periodSecs === saved.periodSecs && was.paused === saved.paused)
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

/** Another tab changed something: read the stores and the records again. */
async function heard(): Promise<void> {
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
    if (!document.hidden && state.route === 'home') {
      // another tab may have changed the store meanwhile
      reopen();
      void refreshHome();
    }
  });
  channel?.addEventListener('message', () => void heard());

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
  // and the mint is asked again each minute, for a rotation by hand, a pause, or another device's change
  setInterval(() => {
    if (state.route === 'home' && !document.hidden) {
      void refreshHome();
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
    void loadSdk().catch(() => undefined);
  });
}
