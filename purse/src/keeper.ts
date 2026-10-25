// Keep-alive while the purse is open. The Keeper decides when a pass runs
// (on open, when the page is seen again, at each rotation, after a read
// that finds fading tokens), takes the lock `purse:transfers` so that one
// tab runs at a time, stops at the next safe step when the page is hidden
// and at once when it goes, and keeps the screen awake while hops go.
// Keeping is the pass itself, per server: read each mint, drop what faded,
// settle what is outgoing, then per mint plan the SDK's refresh, spread its
// hops over the visit, submit them, and log what was kept; and why a mint's
// tokens cannot be kept now, in plain words.
//
// Every browser and SDK dependency is passed in, so both can be tested
// with stand-ins.

import type { Refused, ScheduledTransfer, Spacing, SubmitOptions, SubmitProgress, Submitted } from 'worgl';

import { fieldsOf, say, storeKind, transient } from './errors.ts';
import { type Holdings, type LastFade, fadedAt, holdingsFrom, noteId } from './holdings.ts';
import {
  type Moved,
  SETTLE_MAX_SECS,
  type Spent,
  broughtBy,
  budgetSecs,
  due,
  dueInput,
  hopOf,
  keptOf,
  leftSecs,
  outputsOf,
  respace,
  spaced,
  spacedPlan,
  spacingFor,
  spentBy,
  spentKey,
} from './keep.ts';
import type { Read } from './purses.ts';
import {
  type Stuck,
  keepStoreWords,
  keepingStart,
  layerFullWords,
  lostWords,
  pausedKeepWords,
  policyWords,
  uncheckedWords,
  unreachableFor,
  unreachableWords,
} from './words.ts';

// ---------- the keeper: when a pass runs ----------

/** What a pass is given: the signal that stops it, and where it reports each step of its transfers. */
export interface Run {
  signal: AbortSignal;
  /** Each progress step of every transfer the pass submits, so that the keeper knows when one is on its way. */
  step: (step: SubmitProgress['step']) => void;
}

/** How a pass ended, which decides when the next runs. */
export interface PassEnd {
  /** The soonest next rotation of the mints it knows, by this device's clock. */
  nextMs: number | undefined;
  /** Work is left over (a mint that did not answer, a transient refusal, a store problem): try again in a minute. */
  trouble: boolean;
  /** The store changed under a change, once: one more pass after a second. */
  soon: boolean;
  /** A mint came due that the pass did not plan for (its last read found new fading notes): one more pass follows. */
  again: boolean;
}

/** The Screen Wake Lock, as far as the keeper uses it. */
export interface WakeLockLike {
  request(type: 'screen'): Promise<WakeSentinelLike>;
}

/** A held Screen Wake Lock. */
export interface WakeSentinelLike {
  release(): Promise<void>;
  addEventListener(type: 'release', listener: () => void): void;
}

/** What the keeper needs of the page. */
export interface KeeperDeps {
  /** One pass over every server the purse knows. */
  pass: (run: Run) => Promise<PassEnd>;
  /** Runs `work` holding the lock `purse:transfers`, if no other tab holds it; resolves to false, without running it, if one does. */
  lock: (work: () => Promise<void>) => Promise<boolean>;
  hidden: () => boolean;
  /** The Screen Wake Lock, where the browser has one. */
  wakeLock: () => WakeLockLike | undefined;
  /** Whether the page shows the run (Holdings does; the first run does not): only then is the screen kept awake for it. */
  shows?: () => boolean;
  /** Another tab holds the lock: ask it how its run stands. */
  busy?: () => void;
  /** Another tab's run said nothing for {@link WATCH_MS}: stop showing it. */
  quiet?: () => void;
  setTimeout: (run: () => void, ms: number) => unknown;
  clearTimeout: (timer: unknown) => void;
  now: () => number;
}

/** After a rotation, how long the keeper waits for the mint to make it before the pass. */
export const ROTATION_SLACK_MS = 2500;
/** When work is left over: "The purse asks again in a minute". */
export const RETRY_MS = 60_000;
/** After a change the store refused once because another saved first. */
export const SOON_MS = 1000;
/** How long another tab's run may say nothing before this tab tries itself. */
export const WATCH_MS = 30_000;
/** Before a rotation the mint has not made yet, though its time has come. */
const LATE_MS = 5000;
/** The longest a timer holds. */
const MAX_TIMER_MS = 2 ** 31 - 1;

/**
 * Runs keep-alive passes: one at a time, only while the page is visible,
 * and only in the tab that holds the lock `purse:transfers`.
 */
export class Keeper {
  readonly #deps: KeeperDeps;
  #running = false;
  /** Poked during a pass: exactly one more follows it. */
  #again = false;
  /** The page is going away: nothing runs until it is shown again. */
  #left = false;
  #controller: AbortController | undefined;
  /** The pass running now, until it is over: what {@link stop} waits for. */
  #pass: Promise<void> | undefined;
  /** A transfer is on its way: hiding then waits for its answer. */
  #inFlight = false;
  /** Hidden while a transfer was on its way: stop once it is answered. */
  #stopSoon = false;
  /** The pass has hops to send, so the screen stays awake. */
  #moving = false;
  #sentinel: WakeSentinelLike | undefined;
  #asking: Promise<void> | undefined;
  #timer: unknown;
  #watch: unknown;

  constructor(deps: KeeperDeps) {
    this.#deps = deps;
  }

  /** Whether a pass runs in this tab now. */
  get running(): boolean {
    return this.#running;
  }

  /** Runs a pass now if one can run, or one more after the pass running now. */
  poke(): void {
    if (this.#running) {
      this.#again = true;
      return;
    }
    if (this.#left || this.#deps.hidden()) {
      return;
    }
    this.#pass = this.#run();
  }

  /** The page is visible again: the run goes on, the screen stays awake again, and a pass settles what an abort left. */
  show(): void {
    this.#left = false;
    this.#stopSoon = false;
    if (this.#running && this.#moving) {
      void this.#awake();
    }
    this.poke();
  }

  /**
   * The page is hidden: stop at the next safe step. A wait stops at once; a
   * transfer on its way is answered first, since an abort mid-request leaves
   * an answer only a resend within the mint's receipt gets back.
   */
  hide(): void {
    this.#halt();
  }

  /**
   * Stops the pass running now at its next safe step, as hiding does, and
   * resolves once it is over and the lock `purse:transfers` is free: for a
   * send, which takes that lock next. The pass's unsent hops stay outgoing,
   * and the next pass settles them. Pokes made before the stop give no pass
   * after it; later ones, and the timer it sets, run passes as before.
   */
  async stop(): Promise<void> {
    this.#again = false;
    const pass = this.#pass;
    this.#halt();
    await pass;
  }

  /** Stops the pass at its next safe step: a wait at once, a transfer on its way once it is answered. */
  #halt(): void {
    if (!this.#running) {
      return;
    }
    if (this.#inFlight) {
      this.#stopSoon = true;
    } else {
      this.#controller?.abort();
    }
  }

  /** The page is going away (`pagehide`): stop at once. The SDK saves what is in hand before it resolves. */
  leave(): void {
    this.#left = true;
    this.#controller?.abort();
    this.#deps.clearTimeout(this.#timer);
    this.#deps.clearTimeout(this.#watch);
  }

  /** Another tab runs a pass: watch it, and try here once it has said nothing for {@link WATCH_MS}. */
  watch(): void {
    this.#deps.clearTimeout(this.#watch);
    this.#watch = this.#deps.setTimeout(() => {
      this.#watch = undefined;
      this.#deps.quiet?.();
      this.poke();
    }, WATCH_MS);
  }

  async #run(): Promise<void> {
    this.#running = true;
    this.#again = false;
    this.#deps.clearTimeout(this.#timer);
    const controller = new AbortController();
    this.#controller = controller;
    this.#inFlight = false;
    this.#stopSoon = false;
    this.#moving = false;
    // a pass that fails in a way it does not handle tries again in a minute
    let end: PassEnd = { nextMs: undefined, trouble: true, soon: false, again: false };
    let ran = true;
    try {
      ran = await this.#deps.lock(async () => {
        end = await this.#deps.pass({ signal: controller.signal, step: (step) => this.#step(step, controller) });
      });
    } catch {
      // as above
    }
    await this.#asleep();
    this.#running = false;
    this.#controller = undefined;
    if (!ran) {
      this.#again = false;
      this.#deps.busy?.();
      this.watch();
      return;
    }
    if (!this.#left) {
      this.#plan(end);
    }
    if (this.#again || end.again) {
      this.#again = false;
      this.poke();
    }
  }

  /** Sets the timer for the next pass: at the soonest rotation, or in a minute with work left over. */
  #plan(end: PassEnd): void {
    let delay: number | undefined;
    if (end.soon) {
      delay = SOON_MS;
    } else if (end.trouble) {
      delay = RETRY_MS;
    } else if (end.nextMs !== undefined) {
      const wait = end.nextMs + ROTATION_SLACK_MS - this.#deps.now();
      delay = wait > 0 ? wait : LATE_MS;
    }
    if (delay !== undefined) {
      this.#timer = this.#deps.setTimeout(() => {
        this.#timer = undefined;
        this.poke();
      }, Math.min(delay, MAX_TIMER_MS));
    }
  }

  #step(step: SubmitProgress['step'], controller: AbortController): void {
    if (step === 'waiting' || step === 'sending') {
      void this.#awake();
    }
    if (step === 'sending') {
      this.#inFlight = true;
    } else if (step === 'answered' || step === 'refused') {
      this.#inFlight = false;
      if (this.#stopSoon) {
        controller.abort();
      }
    }
  }

  /**
   * Asks for the Screen Wake Lock while hops go, when the page shows the
   * run; a refusal, or a browser without it, changes nothing.
   */
  async #awake(): Promise<void> {
    this.#moving = true;
    if (this.#sentinel !== undefined || this.#asking !== undefined || this.#deps.hidden() || this.#deps.shows?.() === false) {
      return;
    }
    const wake = this.#deps.wakeLock();
    if (wake === undefined) {
      return;
    }
    const asking = (async () => {
      try {
        const sentinel = await wake.request('screen');
        sentinel.addEventListener('release', () => {
          if (this.#sentinel === sentinel) {
            this.#sentinel = undefined;
          }
        });
        this.#sentinel = sentinel;
      } catch {
        // refused (battery saver, a policy) or unsupported: the run goes on
      }
    })();
    this.#asking = asking;
    await asking;
    this.#asking = undefined;
  }

  /** Lets the screen sleep again: at the end of a pass, and on an abort. */
  async #asleep(): Promise<void> {
    await this.#asking;
    const sentinel = this.#sentinel;
    this.#sentinel = undefined;
    this.#moving = false;
    await sentinel?.release().catch(() => undefined);
  }
}


// ---------- keeping: one pass ----------

/** A mint the purse holds, as a pass reaches it. */
export interface KeptMint {
  mint: string;
  /** Its server's host, for the words. */
  host: string;
}

/** The mints of one server, which share one SDK purse. */
export interface KeptServer {
  server: string;
  mints: KeptMint[];
}

/** The SDK purse, as far as a pass uses it. */
export interface KeepPurse {
  settle(spacing: Spacing, options: SubmitOptions): Promise<Submitted>;
  planRefresh(mint: string): Promise<ScheduledTransfer[]>;
  submit(transfers: ScheduledTransfer[], options: SubmitOptions): Promise<Submitted>;
}

/** Why a mint's tokens cannot be kept alive now, in the words Holdings shows under the field. */
export interface Trouble {
  words: string;
  /** `unreachable` and `store` clear at the next pass that reaches the mint and saves; `blocked` once the mint rotates. */
  kind: 'unreachable' | 'blocked' | 'store';
  /** For `blocked`: the first layer's generation when the hop was refused. */
  oldest?: bigint;
  /** It came from a read the mint did not answer: it stands in for Holdings' own read error, and the next read that answers clears it. */
  read?: boolean;
  /** It came from a paused mint: the read that says so takes over. */
  paused?: boolean;
}

/** The shown mint's hops in this pass, for the back row: their value, how many are done, and whether they go a few seconds apart. */
export interface Shown {
  amount: number;
  done: number;
  total: number;
  spaced: boolean;
}

/** An entry a pass adds to the purse's log (records.ts `LogEntry`): what was kept, what faded, what was lost. */
export interface KeptEntry {
  t: number;
  kind: 'faded' | 'kept' | 'lost';
  mint: string;
  amount: number;
  /** The new notes' serials, for `kept`. */
  serials: string[];
  /** For `kept`: the mint's first layer when they moved, so that the runs of one rotation make one entry. */
  round?: string;
  /** For `kept`: the notes the run moved that the purse held before it, so a merged entry counts each once. */
  spent?: Moved[];
}

/** What a pass needs of the SDK and the page. */
export interface KeepingDeps {
  /** The servers the purse knows, with their mints: the shown mint's server first, and the shown mint first in it. */
  servers(): KeptServer[];
  /** The mint Holdings shows, whose hops the back row counts. */
  shown(): string | undefined;
  /** What the purse holds at a mint, once what faded is dropped (and logged). Rejects as the SDK does. */
  read(server: string, mint: string): Promise<Read>;
  /** What the purse saved of a mint's first tokens to fade, for words when it has no read (`null`: nothing fades; undefined: never saved). */
  saved(mint: string): LastFade | null | undefined;
  /** The server's outgoing transfers, and the notes its sends spend. */
  outgoing(server: string): Promise<{ outgoing: string[]; sendInputs: Set<string> }>;
  /** Runs `work` on the server's SDK purse as one change, so the purse is not opened again in the middle. */
  keepServer<R>(server: string, work: (purse: KeepPurse) => Promise<R>): Promise<R>;
  /** Adds an entry to the log and tells the other tabs; never rejects. */
  log(entry: KeptEntry): Promise<void>;
  /** The shown mint's run moved on, or ended (`undefined`). */
  progress(shown: Shown | undefined): void;
  /** A hop of the shown mint was answered: read again, and slide its note on. */
  hop(spent: Spent): void;
  /** A mint's trouble came, went or changed its words. */
  troubled(): void;
  /** A mint's work in this pass is done: the tabs watching stop showing its run. */
  kept(mint: string): void;
  /** Says words in the live region. */
  announce(words: string): void;
  /** The pass is over: read the shown mint again, and say what was kept of it (0 for nothing). */
  finish(kept: number): Promise<void>;
  /** Has each server's SDK purse read its store again. */
  reopen(): void;
  now(): number;
  random(): number;
}

/** A read under this old is used again, rather than asking the mint again. */
const FRESH_MS = 5000;

/** A hop answered in a pass, and the notes its answer held. */
interface Answered {
  hop: Spent;
  /** Its place among the transfers answered in its submit, to find its notes. */
  slot: number;
  notes: string[];
  /** The serial of the note it brought, once its submit is done. */
  brought: string | undefined;
}

/** Why the notes of a mint did not move this pass, which decides the words. */
type StuckWhy = 'paused' | 'unreachable' | 'unchecked' | 'layer_full' | 'policy';

/** What one pass knows as it goes. */
interface PassState {
  run: Run;
  end: PassEnd;
  shown: string | undefined;
  progress: Shown;
  /** The shown mint's hops the back row counts, by `spentKey`: those whose note is in a layer the mint still keeps. */
  counted: Set<string>;
  /** The notes the answered hops brought, by serial: a hop that moves one on again keeps no more tokens. */
  brought: Set<string>;
  /** The hops answered, by `spentKey`. */
  answered: Set<string>;
  /** Whether the live region has said the run started. */
  started: boolean;
  /** The value of the shown mint's tokens kept. */
  kept: number;
  /** The mints the pass planned a refresh for. */
  planned: Set<string>;
  /** The mints that got new trouble this pass. */
  troubled: Set<string>;
  /** The mints whose hops did not move this pass, and why. */
  stuck: Map<string, { why: StuckWhy; host: string }>;
  /** The store changed under a change this pass. */
  changed: boolean;
}

/** Whether `error` is the reason `signal` aborted with: a call the stop cut short. */
const stopped = (signal: AbortSignal, error: unknown): boolean => signal.aborted && error === signal.reason;

/** A mint's holdings from a read, for the words. */
const holdingsOf = (read: Read): Holdings =>
  holdingsFrom(read.info, read.notes, read.total, read.firstExpiry, read.skew, new Map(), read.moving);

/** Keep-alive's passes, and what they leave: each mint's last read and its trouble. */
export class Keeping {
  /** Why a mint's tokens cannot be kept alive now. */
  readonly trouble = new Map<string, Trouble>();
  readonly #deps: KeepingDeps;
  /** Each mint's last read, and when: kept from one visit to the next, for the words when the mint does not answer. */
  readonly #seen = new Map<string, { read: Read; at: number }>();
  /** The mints this page visit has read: one the visit has not is read before a pass acts on it. */
  readonly #visit = new Set<string>();
  /** The last pass met a store that changed under it. */
  #changed = false;

  constructor(deps: KeepingDeps) {
    this.#deps = deps;
  }

  /**
   * A read Holdings made, kept for the next pass, and what it calls for:
   * `due`, fading notes to move, or `moving`, notes on their way that
   * neither trouble nor a pause holds back, which a pass settles.
   */
  saw(mint: string, read: Read): 'due' | 'moving' | undefined {
    this.#seen.set(mint, { read, at: this.#deps.now() });
    this.#visit.add(mint);
    this.#update(mint, read);
    if (due(this.#dueInput(mint, read))) {
      return 'due';
    }
    return read.moving.length > 0 && !read.info.paused && !this.trouble.has(mint) ? 'moving' : undefined;
  }

  /** The last read of a mint, of this visit or an earlier one. */
  last(mint: string): Read | undefined {
    return this.#seen.get(mint)?.read;
  }

  /** A new visit: every mint is read again before a pass acts on it. What the last reads said stays, for the words. */
  forget(): void {
    this.#visit.clear();
  }

  /** Whether the purse can keep this mint's tokens alive now: it is not paused, and nothing stands in the way. */
  canKeep(mint: string, paused: boolean): boolean {
    return !paused && !this.trouble.has(mint);
  }

  /** One pass over every server, the shown mint's first. */
  async pass(run: Run): Promise<PassEnd> {
    const state: PassState = {
      run,
      end: { nextMs: undefined, trouble: false, soon: false, again: false },
      shown: this.#deps.shown(),
      progress: { amount: 0, done: 0, total: 0, spaced: true },
      counted: new Set(),
      brought: new Set(),
      answered: new Set(),
      started: false,
      kept: 0,
      planned: new Set(),
      troubled: new Set(),
      stuck: new Map(),
      changed: false,
    };
    try {
      for (const server of this.#deps.servers()) {
        if (run.signal.aborted) {
          break;
        }
        await this.#server(server, state);
      }
    } finally {
      if (state.progress.total > 0) {
        this.#deps.progress(undefined);
      }
    }
    if (!state.changed) {
      this.#changed = false;
    }
    const nexts = [...this.#seen.values()].flatMap(({ read }) => {
      const rotation = read.info.paused ? undefined : read.info.rotation;
      return rotation === undefined ? [] : [(Number(rotation.nextAt) - read.skew) * 1000];
    });
    state.end.nextMs = nexts.length === 0 ? undefined : Math.min(...nexts);
    state.end.trouble ||= [...this.trouble.values()].some((trouble) => trouble.kind !== 'blocked');
    if (!run.signal.aborted) {
      await this.#deps.finish(state.kept);
      // the read after the work found fading notes this pass did not plan for (a rotation came meanwhile): one more pass
      state.end.again = [...this.#seen].some(
        ([mint, { read }]) => !state.planned.has(mint) && !this.trouble.has(mint) && due(this.#dueInput(mint, read)),
      );
    }
    return state.end;
  }

  /** Steps 1 to 6 for one server. */
  async #server({ server, mints }: KeptServer, state: PassState): Promise<void> {
    const { signal } = state.run;
    let reached = await this.#readAll(server, mints, state, false);
    if (signal.aborted) {
      return;
    }
    let out: { outgoing: string[]; sendInputs: Set<string> };
    try {
      out = await this.#deps.outgoing(server);
    } catch (error) {
      this.#storeFailed(mints, error, state);
      return;
    }
    const answered: Answered[] = [];
    // what was outgoing before planning: settled here, never sent twice in one pass
    const before = new Set(out.outgoing.flatMap((request) => spentBy(request).map(spentKey)));
    let going = true;
    // 3. settle what is outgoing first: the mint keeps a receipt only about 128 s
    const live = mints.some(({ mint }) => this.#seen.get(mint)?.read.info.paused === false);
    if (out.outgoing.length > 0 && live) {
      const lefts = mints.flatMap(({ mint }) => {
        const read = this.#seen.get(mint)?.read;
        const left = read === undefined ? undefined : leftSecs(read.info, read.skew, this.#deps.now());
        return left === undefined ? [] : [left];
      });
      const budget = budgetSecs(lefts.length === 0 ? undefined : Math.min(...lefts));
      const spacing = spacingFor(out.outgoing.length, Math.min(budget, SETTLE_MAX_SECS));
      this.#count(out.outgoing, out.sendInputs, state, spaced(spacing));
      going = await this.#keep(server, mints, undefined, out.sendInputs, answered, state, (purse, options) =>
        purse.settle(spacing, options),
      );
      // an answer the mint kept from before a rotation lands in what is now the first layer: read again, so it moves on too
      if (going && !signal.aborted) {
        reached = await this.#readAll(server, mints, state, true);
      }
    }
    // 4 and 5. plan each due mint's refresh, spread its hops over the visit, and send them
    for (const [mint, read] of reached) {
      if (!going || signal.aborted) {
        break;
      }
      if (!due(this.#dueInput(mint, read))) {
        continue;
      }
      state.planned.add(mint);
      const budget = budgetSecs(leftSecs(read.info, read.skew, this.#deps.now()));
      going = await this.#keep(server, mints, mint, out.sendInputs, answered, state, async (purse, options) => {
        const plan = await purse.planRefresh(mint);
        const fresh = plan.filter((each) => !spentBy(each.request).some((spent) => before.has(spentKey(spent))));
        if (fresh.length === 0) {
          return undefined;
        }
        const respaced = respace(fresh, budget, this.#deps.random);
        this.#count(
          fresh.map((each) => each.request),
          out.sendInputs,
          state,
          spacedPlan(respaced),
        );
        return purse.submit(respaced, options);
      });
    }
    // what did not move: the words once every hop had its go
    for (const [mint, { why, host }] of state.stuck) {
      if (mints.some((each) => each.mint === mint)) {
        this.#stuckTrouble(mint, why, host, state);
      }
    }
    // 6. log what moved, once per mint, even after a stop
    const byMint = new Map<string, Answered[]>();
    for (const each of answered) {
      byMint.set(each.hop.mint, [...(byMint.get(each.hop.mint) ?? []), each]);
    }
    for (const [mint, hops] of byMint) {
      const { amount, serials, spent } = keptOf(hops);
      if (mint === state.shown) {
        state.kept += amount;
      }
      const round = this.#seen.get(mint)?.read.info.oldest;
      await this.#deps.log({
        t: this.#deps.now(),
        kind: 'kept',
        mint,
        amount,
        serials,
        ...(round === undefined ? {} : { round: String(round) }),
        spent,
      });
      this.#deps.kept(mint);
    }
    if (!going || signal.aborted) {
      return;
    }
    // what moved sits a layer on now: read those mints again, so their words and their date to come back by are of now
    if (byMint.size > 0) {
      await this.#readAll(server, mints.filter(({ mint }) => byMint.has(mint)), state, true);
    }
    // the pass reached these mints and saved what it did: their trouble is over, unless it waits for a rotation
    let cleared = false;
    for (const mint of reached.keys()) {
      const trouble = this.trouble.get(mint);
      if (trouble !== undefined && trouble.kind !== 'blocked' && !state.troubled.has(mint)) {
        this.trouble.delete(mint);
        cleared = true;
      }
    }
    if (cleared) {
      this.#deps.troubled();
    }
  }

  /**
   * Step 1: reads each mint of the server that may have changed, and uses a
   * fresh read again; a quiet one, read this visit and with nothing to move,
   * keeps its last read and is not asked. `force`, after a settle, reads
   * each one again. Resolves to the mints reached.
   */
  async #readAll(server: string, mints: readonly KeptMint[], state: PassState, force: boolean): Promise<Map<string, Read>> {
    const reached = new Map<string, Read>();
    for (const { mint, host } of mints) {
      if (state.run.signal.aborted) {
        break;
      }
      const now = this.#deps.now();
      const known = this.#seen.get(mint);
      const visited = this.#visit.has(mint) && known !== undefined;
      if (!force && visited && now - known.at < FRESH_MS) {
        reached.set(mint, known.read);
        continue;
      }
      if (!force && visited && !this.#asks(mint, known.read, now)) {
        continue;
      }
      try {
        const read = await this.#deps.read(server, mint);
        this.#seen.set(mint, { read, at: this.#deps.now() });
        this.#visit.add(mint);
        this.#update(mint, read);
        reached.set(mint, read);
      } catch (error) {
        if (stopped(state.run.signal, error)) {
          break;
        }
        if (storeKind(error) !== undefined) {
          this.#storeFailed([{ mint, host }], error, state);
        } else {
          this.#unreachable(mint, host, known?.read, error, state, true);
        }
      }
    }
    return reached;
  }

  /** Whether a mint read before must be read again: its rotation's time came, it has notes to move, or trouble. */
  #asks(mint: string, read: Read, now: number): boolean {
    const left = leftSecs(read.info, read.skew, now);
    return (
      (left !== undefined && left <= 0) ||
      read.notes.some((note) => note.position === 0) ||
      read.moving.length > 0 ||
      this.trouble.has(mint)
    );
  }

  #dueInput(mint: string, read: Read): ReturnType<typeof dueInput> {
    const trouble = this.trouble.get(mint);
    return dueInput(read.notes, read.moving.length, read.info, trouble?.kind === 'blocked' ? trouble.oldest : undefined);
  }

  /** Clears what a read shows is over: a block once the mint rotated, a pause the read now names, a read that failed. */
  #update(mint: string, read: Read): void {
    const trouble = this.trouble.get(mint);
    if (
      trouble !== undefined &&
      ((trouble.kind === 'blocked' && trouble.oldest !== read.info.oldest) ||
        (trouble.paused === true && read.info.paused) ||
        trouble.read === true)
    ) {
      this.trouble.delete(mint);
    }
  }

  /** Sets a mint's trouble for this pass; the page hears of it only when what it says changed, so its alert is not said again. */
  #setTrouble(mint: string, trouble: Trouble, state: PassState): void {
    const was = this.trouble.get(mint);
    this.trouble.set(mint, trouble);
    state.troubled.add(mint);
    if (
      was?.words !== trouble.words ||
      was.kind !== trouble.kind ||
      was.oldest !== trouble.oldest ||
      was.read !== trouble.read ||
      was.paused !== trouble.paused
    ) {
      this.#deps.troubled();
    }
  }

  /**
   * Adds the shown mint's hops among `requests` to the run the back row
   * counts: each once, and only one whose note is in a layer the mint keeps,
   * or the one it dropped last, where its answer would land. A note the run
   * brought and moves on again adds no tokens.
   */
  #count(requests: readonly string[], sendInputs: ReadonlySet<string>, state: PassState, isSpaced: boolean): void {
    let more = 0;
    for (const request of requests) {
      const hop = hopOf(request, sendInputs);
      if (hop === undefined || hop.mint !== state.shown || state.counted.has(spentKey(hop))) {
        continue;
      }
      const oldest = this.#seen.get(hop.mint)?.read.info.oldest;
      if (oldest !== undefined && hop.generation + 1n < oldest) {
        // its layer and the next are gone: the mint refuses it, and Recent says it faded
        continue;
      }
      state.counted.add(spentKey(hop));
      state.progress.total += 1;
      state.progress.amount += state.brought.has(hop.id) ? 0 : hop.v;
      more += 1;
    }
    if (more > 0) {
      state.progress.spaced = isSpaced;
      this.#deps.progress({ ...state.progress });
    }
  }

  /**
   * Runs one settle or one planned refresh on the server's purse, following
   * each transfer, and acts on each refusal as it comes. Resolves to false
   * when the store failed, which stops the pass for this server.
   */
  async #keep(
    server: string,
    mints: readonly KeptMint[],
    mint: string | undefined,
    sendInputs: ReadonlySet<string>,
    answered: Answered[],
    state: PassState,
    work: (purse: KeepPurse, options: SubmitOptions) => Promise<Submitted | undefined>,
  ): Promise<boolean> {
    const { run } = state;
    /** The transfers answered, in order: each answer holds one note per output. */
    const order: string[] = [];
    const mine: Answered[] = [];
    const refusals: Promise<void>[] = [];
    const onProgress = (progress: SubmitProgress): void => {
      run.step(progress.step);
      const request = progress.transfer.request;
      const hop = hopOf(request, sendInputs);
      const shown = hop !== undefined && hop.mint === state.shown;
      const counted = hop !== undefined && state.counted.has(spentKey(hop));
      if (progress.step === 'answered') {
        order.push(request);
        if (hop !== undefined) {
          mine.push({ hop, slot: order.length - 1, notes: [], brought: undefined });
          state.answered.add(spentKey(hop));
        }
        if (shown) {
          this.#deps.hop(hop);
        }
        if (counted) {
          state.progress.done += 1;
          this.#deps.progress({ ...state.progress });
        }
      } else if (progress.step === 'refused' && progress.refusal !== undefined) {
        if (counted) {
          state.progress.done += 1;
          this.#deps.progress({ ...state.progress });
        }
        refusals.push(this.#refused(hop, progress.refusal, mints, state));
      } else if ((progress.step === 'waiting' || progress.step === 'sending') && counted && !state.started) {
        state.started = true;
        this.#deps.announce(keepingStart(state.progress.amount, state.progress.total, state.progress.spaced));
      }
    };
    let submitted: Submitted | undefined;
    try {
      submitted = await this.#deps.keepServer(server, (purse) => work(purse, { onProgress, signal: run.signal }));
    } catch (error) {
      await Promise.all(refusals);
      if (stopped(run.signal, error)) {
        return false;
      }
      if (mint !== undefined && storeKind(error) === undefined) {
        // planning read the mint's info, and the mint did not answer
        const host = mints.find((each) => each.mint === mint)?.host ?? '';
        this.#unreachable(mint, host, this.#seen.get(mint)?.read, error, state, false);
        return true;
      }
      this.#storeFailed(mints, error, state);
      return false;
    }
    await Promise.all(refusals);
    if (submitted !== undefined) {
      const notes: string[][] = [];
      let at = 0;
      for (const request of order) {
        const n = outputsOf(request);
        notes.push(submitted.notes.slice(at, at + n));
        at += n;
      }
      for (const each of mine) {
        each.notes = notes[each.slot] ?? [];
        each.brought = broughtBy([each.hop], each.notes)[0];
        if (each.brought !== undefined) {
          state.brought.add(each.brought);
        }
      }
    }
    answered.push(...mine);
    return true;
  }

  /** Acts on a refused transfer as it comes (section 12.2): logs what went, and says what did not move. */
  async #refused(hop: Spent | undefined, refusal: Refused, mints: readonly KeptMint[], state: PassState): Promise<void> {
    const { signal } = state.run;
    if (stopped(signal, refusal.error)) {
      // the stop: it stays outgoing, and the next visible pass settles it
      return;
    }
    if (refusal.outcome === 'kept') {
      state.end.trouble = true;
    }
    if (hop === undefined) {
      // a send's transfer: kept for the next settle, and the Send screen's to tell
      return;
    }
    const { mint } = hop;
    const { code } = fieldsOf(refusal.error);
    const read = this.#seen.get(mint)?.read;
    const now = this.#deps.now();
    if (refusal.outcome === 'dropped') {
      if (code === 'spent') {
        // the mint applied it before, and no longer keeps its answer
        const amount = refusal.atStake === undefined ? hop.v : Number(refusal.atStake);
        await this.#deps.log({ t: now, kind: 'lost', mint, amount, serials: [] });
        if (mint === state.shown) {
          this.#deps.announce(lostWords(amount, 1));
        }
      } else {
        // its layer went before the hop landed, or after an answer was lost: either way the tokens went with it
        const t = read === undefined ? now : fadedAt(hop.generation, read.info, read.skew, now);
        await this.#deps.log({ t, kind: 'faded', mint, amount: hop.v, serials: [] });
      }
      return;
    }
    let why: StuckWhy;
    if (refusal.outcome === 'cancelled') {
      // held again at layer 0; planned again it would be refused again until its layer goes
      why = code === 'policy' ? 'policy' : 'layer_full';
    } else if (code === 'paused') {
      why = 'paused';
    } else {
      why = transient(refusal.error) ? 'unreachable' : 'unchecked';
    }
    const host = mints.find((each) => each.mint === mint)?.host ?? '';
    state.stuck.set(mint, { why, host });
    this.#stuckTrouble(mint, why, host, state, hop);
  }

  /**
   * The words for a mint whose hops did not move: every note of it that has
   * not moved this pass, fading or on its way, whatever hop was refused, so
   * the words stay the same from one refusal to the next and from one pass
   * to the next while the trouble lasts.
   */
  #stuckTrouble(mint: string, why: StuckWhy, host: string, state: PassState, refused?: Spent): void {
    const read = this.#seen.get(mint)?.read;
    if (read === undefined) {
      return;
    }
    const waiting: Spent[] = [
      ...read.notes
        .filter((note) => note.position === 0)
        .map((note) => ({ mint, id: noteId(note.serial), v: Number(note.amount), generation: note.serial.generation })),
      ...read.moving.filter((note) => note.generation + 1n >= read.info.oldest).map((note) => ({ mint, ...note })),
    ].filter((each) => !state.answered.has(spentKey(each)));
    const notes = waiting.length > 0 ? waiting : refused === undefined ? [] : [refused];
    if (notes.length === 0) {
      return;
    }
    const stuck = this.#stuck(notes, read);
    let trouble: Trouble;
    switch (why) {
      case 'layer_full':
      case 'policy':
        trouble = { words: why === 'policy' ? policyWords(host, stuck) : layerFullWords(host, stuck), kind: 'blocked', oldest: read.info.oldest };
        break;
      case 'paused':
        trouble = { words: pausedKeepWords(host, notes.reduce((total, each) => total + each.v, 0)), kind: 'unreachable', paused: true };
        break;
      case 'unreachable':
        trouble = { words: unreachableWords(host, stuck), kind: 'unreachable', paused: false };
        break;
      case 'unchecked':
        trouble = { words: uncheckedWords(host, stuck), kind: 'unreachable', paused: false };
        break;
    }
    this.#setTrouble(mint, trouble, state);
  }

  /** What did not move, for the words: the notes' values, the lowest of their layers, and the mint's holdings. */
  #stuck(notes: readonly Spent[], read: Read): Stuck {
    const now = this.#deps.now();
    const lowest = notes.reduce((low, each) => (each.generation < low ? each.generation : low), notes[0]?.generation ?? read.info.oldest);
    return {
      values: notes.map((each) => each.v),
      pos: Math.max(0, Number(lowest - read.info.oldest)),
      holdings: holdingsOf(read),
      now,
    };
  }

  /**
   * The mint did not answer a read or the plan's read of its info. The
   * words say what fades and when, from its last read, or what the purse
   * saved of it; with nothing to fade, they are the read's own.
   */
  #unreachable(mint: string, host: string, last: Read | undefined, error: unknown, state: PassState, fromRead: boolean): void {
    const said = transient(error)
      ? unreachableFor(host, last === undefined ? undefined : holdingsOf(last), this.#deps.saved(mint), this.#deps.now())
      : undefined;
    this.#setTrouble(mint, { words: said ?? say(error, 'open', { host }).words, kind: 'unreachable', read: fromRead }, state);
    state.end.trouble = true;
  }

  /**
   * The store failed: another tab holds it in an older version, the browser
   * refused to save, or another change saved first. That last once is
   * nothing to say: one more pass after a second, on a purse read again.
   */
  #storeFailed(mints: readonly KeptMint[], error: unknown, state: PassState): void {
    const kind = storeKind(error);
    if (kind === 'changed') {
      this.#deps.reopen();
      state.changed = true;
      if (!this.#changed) {
        this.#changed = true;
        state.end.soon = true;
        return;
      }
    }
    const words = kind === 'blocked' ? say(error, 'open', { host: '' }).words : keepStoreWords;
    for (const { mint } of mints) {
      this.#setTrouble(mint, { words, kind: 'store' }, state);
    }
    state.end.trouble = true;
  }
}
