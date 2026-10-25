import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import type { HeldNote, MintInfo, Refused, ScheduledTransfer, Spacing, SubmitOptions, SubmitProgress, Submitted } from 'worgl';

import {
  type KeepPurse,
  Keeper,
  type KeeperDeps,
  Keeping,
  type KeepingDeps,
  type KeptEntry,
  type PassEnd,
  RETRY_MS,
  ROTATION_SLACK_MS,
  type Run,
  type Shown,
  SOON_MS,
  WATCH_MS,
  type WakeLockLike,
} from '../src/keeper.ts';
import type { Read } from '../src/purses.ts';
import { when } from '../src/format.ts';
import { keepStoreWords, keepingStart, lostWords } from '../src/words.ts';
import { ISSUER, OTHER, decodeNoteParts, noteText, transfer } from './notes.ts';

/** Lets every pending promise and microtask run. */
async function flush(): Promise<void> {
  for (let i = 0; i < 10; i++) {
    await new Promise((resolve) => setImmediate(resolve));
  }
}

/** Fake timers on a fake clock. */
class Clock {
  now = 1_000_000;
  #id = 0;
  readonly timers = new Map<number, { run: () => void; at: number; ms: number }>();
  setTimeout = (run: () => void, ms: number): unknown => {
    const id = ++this.#id;
    this.timers.set(id, { run, at: this.now + ms, ms });
    return id;
  };
  clearTimeout = (timer: unknown): void => {
    this.timers.delete(timer as number);
  };
  /** The delays of the timers waiting, soonest first. */
  delays(): number[] {
    return [...this.timers.values()].map((timer) => timer.ms).sort((a, b) => a - b);
  }
  /** Moves the clock on, running each timer that comes due. */
  advance(ms: number): void {
    this.now += ms;
    for (const [id, timer] of [...this.timers]) {
      if (timer.at <= this.now) {
        this.timers.delete(id);
        timer.run();
      }
    }
  }
}

/** A stand-in pass the test ends, or that ends when its signal aborts. */
interface Call {
  run: Run;
  end: (end?: Partial<PassEnd>) => void;
}

function passes(): { pass: KeeperDeps['pass']; calls: Call[]; most: () => number } {
  const calls: Call[] = [];
  let live = 0;
  let most = 0;
  const pass = (run: Run): Promise<PassEnd> => {
    live += 1;
    most = Math.max(most, live);
    return new Promise((resolve) => {
      let ended = false;
      const end = (more: Partial<PassEnd> = {}): void => {
        if (!ended) {
          ended = true;
          live -= 1;
          resolve({ nextMs: undefined, trouble: false, soon: false, again: false, ...more });
        }
      };
      run.signal.addEventListener('abort', () => end());
      calls.push({ run, end });
    });
  };
  return { pass, calls, most: () => most };
}

/** A stand-in Screen Wake Lock, refused if asked to be; the browser can release it, as when the page is hidden. */
function wakeLock(refuse = false): { lock: WakeLockLike; log: string[]; browserReleases: () => void } {
  const log: string[] = [];
  let listeners: (() => void)[] = [];
  const lock: WakeLockLike = {
    request: async () => {
      log.push('request');
      if (refuse) {
        throw new DOMException('battery saver', 'NotAllowedError');
      }
      return {
        release: async () => {
          log.push('release');
        },
        addEventListener: (_type: 'release', listener: () => void) => {
          listeners.push(listener);
        },
      };
    },
  };
  return {
    lock,
    log,
    browserReleases: () => {
      for (const listener of listeners) {
        listener();
      }
      listeners = [];
    },
  };
}

function keeperWith(more: Partial<KeeperDeps> = {}) {
  const clock = new Clock();
  const p = passes();
  const page = { hidden: false, granted: true };
  const heard: string[] = [];
  const keeper = new Keeper({
    pass: p.pass,
    lock: async (work) => {
      if (!page.granted) {
        return false;
      }
      await work();
      return true;
    },
    hidden: () => page.hidden,
    wakeLock: () => undefined,
    busy: () => heard.push('busy'),
    quiet: () => heard.push('quiet'),
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
    now: () => clock.now,
    ...more,
  });
  return { keeper, clock, p, page, heard };
}

describe('the keeper', () => {
  test('runs one pass at a time: pokes during a pass give exactly one more after it', async () => {
    const { keeper, p } = keeperWith();
    keeper.poke();
    await flush();
    assert.equal(p.calls.length, 1);
    assert.equal(keeper.running, true);
    keeper.poke();
    keeper.poke();
    await flush();
    assert.equal(p.calls.length, 1);
    p.calls[0]?.end();
    await flush();
    assert.equal(p.calls.length, 2);
    p.calls[1]?.end();
    await flush();
    assert.equal(p.calls.length, 2);
    assert.equal(p.most(), 1, 'never two at once');
    assert.equal(keeper.running, false);
  });

  test('runs nothing while another tab holds the lock, watches it, and tries once it says nothing for 30 s', async () => {
    const { keeper, p, page, clock, heard } = keeperWith();
    page.granted = false;
    keeper.poke();
    await flush();
    assert.equal(p.calls.length, 0);
    assert.deepEqual(heard, ['busy']);
    assert.deepEqual(clock.delays(), [WATCH_MS]);
    // the other tab says how its run stands: watch again from now
    clock.advance(20_000);
    keeper.watch();
    assert.deepEqual(clock.delays(), [WATCH_MS]);
    clock.advance(20_000);
    await flush();
    assert.equal(p.calls.length, 0);
    page.granted = true;
    clock.advance(10_000);
    await flush();
    assert.deepEqual(heard, ['busy', 'quiet']);
    assert.equal(p.calls.length, 1);
  });

  test('hidden while a transfer waits, stops at once', async () => {
    const { keeper, p, page } = keeperWith();
    keeper.poke();
    await flush();
    const run = p.calls[0]?.run;
    run?.step('waiting');
    page.hidden = true;
    keeper.hide();
    assert.equal(run?.signal.aborted, true);
  });

  test('hidden while a transfer is on its way, stops once it is answered', async () => {
    const { keeper, p, page } = keeperWith();
    keeper.poke();
    await flush();
    const run = p.calls[0]?.run;
    run?.step('waiting');
    run?.step('sending');
    page.hidden = true;
    keeper.hide();
    assert.equal(run?.signal.aborted, false);
    run?.step('answered');
    assert.equal(run?.signal.aborted, true);
  });

  test('hidden then shown while a transfer is on its way, goes on', async () => {
    const { keeper, p, page } = keeperWith();
    keeper.poke();
    await flush();
    const run = p.calls[0]?.run;
    run?.step('sending');
    page.hidden = true;
    keeper.hide();
    page.hidden = false;
    keeper.show();
    run?.step('answered');
    run?.step('waiting');
    assert.equal(run?.signal.aborted, false);
  });

  test('stops at once when the page goes, even mid-transfer', async () => {
    const { keeper, p } = keeperWith();
    keeper.poke();
    await flush();
    const run = p.calls[0]?.run;
    run?.step('sending');
    keeper.leave();
    assert.equal(run?.signal.aborted, true);
    await flush();
    keeper.poke();
    await flush();
    assert.equal(p.calls.length, 1, 'nothing runs once the page is going');
  });

  test('stopped for a send, hurries the pass and waits for its end, then runs passes as before', async () => {
    const { keeper, p } = keeperWith();
    keeper.poke();
    await flush();
    const run = p.calls[0]?.run;
    run?.step('sending');
    // poked before the stop: no pass follows it
    keeper.poke();
    let over = false;
    const stopping = keeper.stop().then(() => {
      over = true;
    });
    await flush();
    assert.equal(run?.hurry.aborted, true, 'what the pass has in hand goes at once');
    assert.equal(run?.signal.aborted, false, 'and nothing is cut short, so no hop is left unsent');
    run?.step('answered');
    run?.step('sending');
    run?.step('answered');
    assert.equal(run?.signal.aborted, false);
    await flush();
    assert.equal(over, false, 'the stop waits for the pass to end');
    p.calls[0]?.end();
    await stopping;
    assert.equal(keeper.running, false, 'the lock is free once the stop resolves');
    await flush();
    assert.equal(p.calls.length, 1);
    keeper.poke();
    await flush();
    assert.equal(p.calls.length, 2, 'unlike a page that goes, later pokes run');
    assert.equal(p.calls[1]?.run.signal.aborted, false);
    assert.equal(p.calls[1]?.run.hurry.aborted, false, 'the next pass goes at its own pace');
  });

  test('hurried for a send, stops at a hop the mint did not answer, but not at one it refused for good', async () => {
    for (const [outcome, stops] of [
      ['kept', true],
      ['dropped', false],
      ['cancelled', false],
    ] as const) {
      const { keeper, p } = keeperWith();
      keeper.poke();
      await flush();
      const run = p.calls[0]?.run;
      run?.step('sending');
      void keeper.stop();
      run?.step('refused', outcome);
      assert.equal(run?.signal.aborted, stops, outcome);
    }
  });

  test('shown again during a stop for a send, still ends the pass', async () => {
    const { keeper, p, page } = keeperWith();
    keeper.poke();
    await flush();
    const run = p.calls[0]?.run;
    run?.step('sending');
    page.hidden = true;
    keeper.hide();
    void keeper.stop();
    page.hidden = false;
    keeper.show();
    run?.step('answered');
    assert.equal(run?.signal.aborted, false, 'shown again: the hide is undone');
    assert.equal(run?.hurry.aborted, true, 'the send’s stop is not');
  });

  test('a pass poked while the stop for a send waits gives way to the send, and the send pokes after', async () => {
    const page = { sending: 0 };
    // as the page's passLock: a send of this tab's, counted before it stops the keeper, has the pass run nothing
    const { keeper, p } = keeperWith({
      lock: async (work) => {
        if (page.sending > 0) {
          return true;
        }
        await work();
        return true;
      },
    });
    keeper.poke();
    await flush();
    const run = p.calls[0]?.run;
    run?.step('sending');
    page.sending += 1;
    const stopping = keeper.stop();
    // the minute's poke, or the page shown again
    keeper.poke();
    keeper.show();
    run?.step('answered');
    p.calls[0]?.end();
    await stopping;
    await flush();
    assert.equal(p.calls.length, 1, 'the pass after it ran nothing');
    page.sending -= 1;
    keeper.poke();
    await flush();
    assert.equal(p.calls.length, 2);
  });

  test('stopped with no pass running, resolves at once', async () => {
    const { keeper, p } = keeperWith();
    await keeper.stop();
    keeper.poke();
    await flush();
    assert.equal(p.calls.length, 1);
  });

  test('resumes with a new pass once shown after a stop', async () => {
    const { keeper, p, page } = keeperWith();
    keeper.poke();
    await flush();
    p.calls[0]?.run.step('waiting');
    page.hidden = true;
    keeper.hide();
    await flush();
    assert.equal(keeper.running, false);
    page.hidden = false;
    keeper.show();
    await flush();
    assert.equal(p.calls.length, 2);
    assert.equal(p.calls[1]?.run.signal.aborted, false);
  });

  test('starts nothing while hidden', async () => {
    const { keeper, p, page } = keeperWith();
    page.hidden = true;
    keeper.poke();
    await flush();
    assert.equal(p.calls.length, 0);
  });

  test('keeps the screen awake while hops go, and lets it sleep after', async () => {
    const wake = wakeLock();
    const { keeper, p } = keeperWith({ wakeLock: () => wake.lock });
    keeper.poke();
    await flush();
    assert.deepEqual(wake.log, [], 'nothing asked before a hop waits');
    p.calls[0]?.run.step('waiting');
    p.calls[0]?.run.step('sending');
    await flush();
    assert.deepEqual(wake.log, ['request']);
    p.calls[0]?.end();
    await flush();
    assert.deepEqual(wake.log, ['request', 'release']);
    // a pass with nothing to send asks for nothing
    keeper.poke();
    await flush();
    p.calls[1]?.end();
    await flush();
    assert.deepEqual(wake.log, ['request', 'release']);
  });

  test('lets the screen sleep on a stop, and asks again when shown mid-pass', async () => {
    const wake = wakeLock();
    const { keeper, p, page } = keeperWith({ wakeLock: () => wake.lock });
    keeper.poke();
    await flush();
    const run = p.calls[0]?.run;
    run?.step('waiting');
    run?.step('sending');
    await flush();
    // hidden: the browser lets go of the lock itself
    page.hidden = true;
    wake.browserReleases();
    keeper.hide();
    page.hidden = false;
    keeper.show();
    await flush();
    assert.deepEqual(wake.log, ['request', 'request']);
    run?.step('answered');
    run?.step('waiting');
    page.hidden = true;
    keeper.hide();
    await flush();
    assert.equal(run?.signal.aborted, true);
    assert.deepEqual(wake.log, ['request', 'request', 'release']);
  });

  test('keeps the screen as it is for a run the page does not show', async () => {
    const wake = wakeLock();
    const view = { home: false };
    const { keeper, p } = keeperWith({ wakeLock: () => wake.lock, shows: () => view.home });
    keeper.poke();
    await flush();
    p.calls[0]?.run.step('waiting');
    p.calls[0]?.run.step('sending');
    await flush();
    assert.deepEqual(wake.log, [], 'the first run shows no run');
    // Holdings opens mid-run: the next step asks
    view.home = true;
    p.calls[0]?.run.step('answered');
    p.calls[0]?.run.step('waiting');
    await flush();
    assert.deepEqual(wake.log, ['request']);
  });

  test('runs once more when a pass found a mint due it did not plan for', async () => {
    const { keeper, p } = keeperWith();
    keeper.poke();
    await flush();
    p.calls[0]?.end({ again: true });
    await flush();
    assert.equal(p.calls.length, 2);
    p.calls[1]?.end();
    await flush();
    assert.equal(p.calls.length, 2);
  });

  test('goes on when the screen may not stay awake, or the browser has no Wake Lock', async () => {
    for (const lock of [wakeLock(true).lock, undefined]) {
      const { keeper, p, clock } = keeperWith({ wakeLock: () => lock });
      keeper.poke();
      await flush();
      p.calls[0]?.run.step('waiting');
      await flush();
      assert.equal(p.calls[0]?.run.signal.aborted, false);
      p.calls[0]?.end({ nextMs: clock.now + 60_000 });
      await flush();
      assert.equal(keeper.running, false);
      assert.deepEqual(clock.delays(), [60_000 + ROTATION_SLACK_MS]);
    }
  });

  test('runs again at the next rotation, in a minute with work left, or in a second when the store changed', async () => {
    const cases: [Partial<PassEnd>, number[]][] = [
      [{ nextMs: 1_000_000 + 3_600_000 }, [3_600_000 + ROTATION_SLACK_MS]],
      [{ nextMs: 1_000_000 + 3_600_000, trouble: true }, [RETRY_MS]],
      [{ nextMs: 1_000_000 + 3_600_000, soon: true }, [SOON_MS]],
      [{ nextMs: 1_000_000 - 10_000 }, [5000]],
      [{ nextMs: 1_000_000 + 2 ** 33 }, [2 ** 31 - 1]],
      [{}, []],
    ];
    for (const [end, delays] of cases) {
      const { keeper, p, clock } = keeperWith();
      keeper.poke();
      await flush();
      p.calls[0]?.end(end);
      await flush();
      assert.deepEqual(clock.delays(), delays, JSON.stringify(end, (_, v: unknown) => v));
    }
    // and the timer pokes
    const { keeper, p, clock } = keeperWith();
    keeper.poke();
    await flush();
    p.calls[0]?.end({ trouble: true });
    await flush();
    clock.advance(RETRY_MS);
    await flush();
    assert.equal(p.calls.length, 2);
  });
});

// ---------- a pass ----------

const SERVER = 'http://127.0.0.1:9';
const NEXT = 1_800_000_000n;
const OLDEST = 7n;

const mintInfo = (more: Partial<MintInfo> = {}): MintInfo => ({
  issuer: ISSUER,
  sequence: 1n,
  layers: 4,
  oldest: OLDEST,
  paused: false,
  rotation: { periodSecs: 28_800n, nextAt: NEXT },
  supply: { minted: 0n, destroyed: 0n, burned: 0n, outstanding: 0n },
  ...more,
});

const held = (index: number, amount: bigint, position: number): HeldNote => ({
  serial: { generation: OLDEST + BigInt(position), index },
  amount,
  state: position === 0 ? 'expiring' : position === 3 ? 'frozen' : 'spendable',
  position,
  spendableAt: undefined,
  expiresAt: NEXT + BigInt(position) * 28_800n,
});

/** The note text of a held note. */
const textOf = (note: HeldNote): string =>
  noteText({ generation: note.serial.generation, index: note.serial.index, amount: note.amount });

/** What the mint says to a transfer: its answer notes, or a refusal. */
type Answer = { notes: string[] } | { refused: Omit<Refused, 'request'> };

class MintError extends Error {
  override readonly name = 'MintError';
  readonly code: string;
  readonly status: number;
  constructor(code: string, status: number) {
    super(code);
    this.code = code;
    this.status = status;
  }
}

/** A stand-in SDK purse over one mint: what it holds, what is outgoing, and how the mint answers. */
class FakePurse implements KeepPurse {
  readonly calls: string[] = [];
  notes: HeldNote[];
  outgoing: string[] = [];
  answer: (request: string) => Answer = (request) => {
    // the same value back, one generation on, at a new index
    const spent = decodeNoteParts(request);
    return { notes: spent.map((note) => noteText({ generation: note.generation + 1n, index: 100 + note.index, amount: note.amount })) };
  };
  /** Stops the submit once this many transfers were answered, as a hidden page would, or hurries it, as a send would. */
  stopAfter: { count: number; controller: AbortController } | undefined;
  /** For each transfer sent, whether it went at once (`sendNow` had aborted). */
  readonly sentNow: boolean[] = [];

  constructor(notes: HeldNote[]) {
    this.notes = notes;
  }

  async settle(spacing: Spacing, options: SubmitOptions, holdBack: ReadonlySet<string> = new Set()): Promise<Submitted> {
    this.calls.push(`settle ${spacing.minSecs}-${spacing.maxSecs}`);
    return this.#send(
      this.outgoing
        .filter((request) => !decodeNoteParts(request).some((note) => holdBack.has(note.issuer)))
        .map((request) => ({ request, afterSecs: 0 })),
      options,
    );
  }

  async planRefresh(): Promise<ScheduledTransfer[]> {
    this.calls.push('plan');
    const fading = this.notes.filter((note) => note.position === 0);
    this.notes = this.notes.filter((note) => note.position !== 0);
    const plan = fading.map((note, i) => ({ request: transfer([textOf(note)]), afterSecs: 1000 * (i + 1) }));
    this.outgoing.push(...plan.map((each) => each.request));
    return plan;
  }

  async submit(transfers: ScheduledTransfer[], options: SubmitOptions): Promise<Submitted> {
    this.calls.push(`submit ${transfers.map((each) => each.afterSecs).join(' ')}`);
    return this.#send(transfers, options);
  }

  async #send(transfers: ScheduledTransfer[], options: SubmitOptions): Promise<Submitted> {
    const done: Submitted = { value: 0n, answered: 0, notes: [], theirs: [], refused: [], unsent: [] };
    const tell = (step: SubmitProgress['step'], each: ScheduledTransfer, index: number, refusal?: Refused): void =>
      options.onProgress?.({
        step,
        transfer: each,
        index,
        total: transfers.length,
        answered: done.answered,
        refused: done.refused.length,
        sendAt: undefined,
        refusal,
      });
    for (const [index, each] of transfers.entries()) {
      if (options.signal?.aborted === true) {
        done.unsent.push(each);
        tell('unsent', each, index);
        continue;
      }
      if (options.sendNow?.aborted !== true) {
        tell('waiting', each, index);
      }
      this.sentNow.push(options.sendNow?.aborted === true);
      tell('sending', each, index);
      const answer = this.answer(each.request);
      if ('notes' in answer) {
        done.answered += 1;
        done.notes.push(...answer.notes);
        this.outgoing = this.outgoing.filter((request) => request !== each.request);
        tell('answered', each, index);
        if (this.stopAfter !== undefined && done.answered >= this.stopAfter.count) {
          this.stopAfter.controller.abort();
        }
      } else {
        const refusal: Refused = { request: each.request, ...answer.refused };
        if (refusal.outcome !== 'kept') {
          this.outgoing = this.outgoing.filter((request) => request !== each.request);
        }
        done.refused.push(refusal);
        tell('refused', each, index, refusal);
      }
    }
    return done;
  }
}

/** A pass over one server with one mint, its SDK and page stood in for. */
function passWith(purse: FakePurse, more: Partial<KeepingDeps> = {}) {
  const now = (Number(NEXT) - 3600) * 1000;
  const logged: KeptEntry[] = [];
  const said: string[] = [];
  const shown: (Shown | undefined)[] = [];
  const hops: string[] = [];
  const kept: string[] = [];
  const finished: number[] = [];
  const reads: boolean[] = [];
  let reopened = 0;
  const read = async (): Promise<Read> => ({
    info: mintInfo(),
    skew: 0,
    notes: [...purse.notes],
    total: 0n,
    firstExpiry: undefined,
    moving: purse.outgoing.flatMap((request) =>
      decodeNoteParts(request).map((note) => ({ id: `${note.generation}:${note.index}`, v: Number(note.amount), generation: note.generation })),
    ),
  });
  const keeping = new Keeping({
    servers: () => [{ server: SERVER, mints: [{ mint: ISSUER, host: '127.0.0.1:9' }] }],
    shown: () => ISSUER,
    read,
    saved: () => undefined,
    outgoing: async () => ({ outgoing: [...purse.outgoing], sendInputs: new Set() }),
    keepServer: async (_server, work) => work(purse),
    log: async (entry) => {
      logged.push(entry);
    },
    progress: (p) => shown.push(p),
    hop: (spent) => hops.push(spent.id),
    troubled: () => undefined,
    kept: (mint) => kept.push(mint),
    announce: (words) => said.push(words),
    finish: async (n, _mint, again) => {
      finished.push(n);
      reads.push(again);
    },
    reopen: () => {
      reopened += 1;
    },
    now: () => now,
    random: () => 0,
    ...more,
  });
  const controller = new AbortController();
  const hurry = new AbortController();
  const steps: string[] = [];
  const run: Run = { signal: controller.signal, hurry: hurry.signal, step: (step) => steps.push(step) };
  return { keeping, run, controller, hurry, logged, said, shown, hops, kept, finished, reads, steps, reopened: () => reopened };
}

describe('a pass', () => {
  test('settles what is outgoing first, then moves what fades, a few seconds apart, and logs what it kept', async () => {
    const left = held(1, 64n, 0);
    const purse = new FakePurse([held(2, 64n, 0), held(3, 32n, 2)]);
    // a hop an earlier visit planned and never sent
    purse.outgoing.push(transfer([textOf(left)]));
    const t = passWith(purse);
    const end = await t.keeping.pass(t.run);
    assert.deepEqual(purse.calls, ['settle 2-5', 'plan', 'submit 1']);
    assert.equal(purse.outgoing.length, 0);
    assert.deepEqual(t.hops, ['7:1', '7:2']);
    assert.deepEqual(t.logged, [
      {
        t: (Number(NEXT) - 3600) * 1000,
        kind: 'kept',
        mint: ISSUER,
        amount: 128,
        serials: ['8:101', '8:102'],
        values: [64, 64],
        round: String(OLDEST),
        spent: [
          { id: '7:1', v: 64 },
          { id: '7:2', v: 64 },
        ],
      },
    ]);
    assert.deepEqual(t.kept, [ISSUER]);
    assert.deepEqual(t.finished, [128]);
    assert.equal(t.said[0], keepingStart(64, 1, true));
    assert.deepEqual(t.shown.at(-2), { mint: ISSUER, amount: 128, done: 2, total: 2, spaced: true });
    assert.equal(t.shown.at(-1), undefined, 'the run is over');
    assert.equal(end.trouble, false);
    assert.equal(end.nextMs, Number(NEXT) * 1000);
    assert.equal(t.keeping.canKeep(ISSUER, false), true);
  });

  test('respaces the SDK’s plan over the visit', async () => {
    const purse = new FakePurse([held(1, 64n, 0), held(2, 4n, 0), held(3, 1n, 0)]);
    const t = passWith(purse);
    await t.keeping.pass(t.run);
    // random 0: 1 s, then 2 s apart; the SDK had spread them over the hour left
    assert.deepEqual(purse.calls, ['plan', 'submit 1 3 5']);
    assert.equal(t.said[0], 'Keeping 69 alive: three transfers, a few seconds apart.');
  });

  test('reads a mint again once its notes moved, so its words and its date are of now', async () => {
    const purse = new FakePurse([held(1, 64n, 0)]);
    const answer = purse.answer;
    purse.answer = (request) => {
      purse.notes.push(held(101, 64n, 1));
      return answer(request);
    };
    const t = passWith(purse);
    await t.keeping.pass(t.run);
    const last = t.keeping.last(ISSUER);
    assert.deepEqual(last?.notes.map((note) => note.position), [1]);
    assert.deepEqual(last?.moving, []);
  });

  test('does nothing for a mint with nothing fading', async () => {
    const purse = new FakePurse([held(1, 64n, 1)]);
    const t = passWith(purse);
    await t.keeping.pass(t.run);
    assert.deepEqual(purse.calls, []);
    assert.deepEqual(t.logged, []);
    assert.deepEqual(t.finished, [0]);
  });

  test('logs a hop the mint applied whose answer it no longer keeps as lost', async () => {
    const purse = new FakePurse([held(1, 64n, 0)]);
    purse.answer = () => ({ refused: { error: new MintError('spent', 409), atStake: 64n, outcome: 'dropped' } });
    const t = passWith(purse);
    await t.keeping.pass(t.run);
    assert.deepEqual(t.logged, [{ t: (Number(NEXT) - 3600) * 1000, kind: 'lost', mint: ISSUER, amount: 64, serials: [] }]);
    assert.ok(t.said.includes(lostWords(64, 1)));
    assert.deepEqual(t.finished, [0]);
    assert.equal(t.keeping.trouble.size, 0);
  });

  test('says when the next layer is full, and plans nothing more until the mint rotates', async () => {
    const purse = new FakePurse([held(1, 4n, 0), held(2, 1n, 0)]);
    purse.answer = (request) => {
      // held again at layer 0, as the SDK does with a cancelled hop
      purse.notes.push(...decodeNoteParts(request).map((note) => held(note.index, note.amount, 0)));
      return { refused: { error: new MintError('layer_full', 503), atStake: undefined, outcome: 'cancelled' } };
    };
    const t = passWith(purse);
    const end = await t.keeping.pass(t.run);
    const trouble = t.keeping.trouble.get(ISSUER);
    assert.equal(trouble?.kind, 'blocked');
    const at = when(Number(NEXT) * 1000, (Number(NEXT) - 3600) * 1000);
    assert.equal(trouble?.words, `The mint at 127.0.0.1:9 has no room in the next layer, so notes of 4 and 1 could not move. They fade at ${at}.`);
    assert.equal(t.keeping.canKeep(ISSUER, false), false);
    assert.equal(end.trouble, false, 'a full layer waits for the rotation, not a minute');
    // the same layer: not due
    const read = { info: mintInfo(), skew: 0, notes: [...purse.notes], total: 5n, firstExpiry: undefined, moving: [] };
    assert.equal(t.keeping.saw(ISSUER, read), undefined);
    // the mint rotated: due again
    assert.equal(t.keeping.saw(ISSUER, { ...read, info: mintInfo({ oldest: OLDEST + 1n }) }), 'due');
    assert.equal(t.keeping.trouble.size, 0);
  });

  test('says when the mint does not answer a hop, keeps it outgoing and tries again in a minute', async () => {
    const purse = new FakePurse([held(1, 4n, 0), held(2, 1n, 0)]);
    purse.answer = () => ({ refused: { error: new TypeError('Failed to fetch'), atStake: undefined, outcome: 'kept' } });
    const t = passWith(purse);
    const end = await t.keeping.pass(t.run);
    assert.equal(end.trouble, true);
    assert.equal(purse.outgoing.length, 2);
    const at = when(Number(NEXT) * 1000, (Number(NEXT) - 3600) * 1000);
    assert.equal(
      t.keeping.trouble.get(ISSUER)?.words,
      `The mint at 127.0.0.1:9 did not answer, so 5 tokens could not move. They fade at ${at} unless the purse reaches it before then. The purse asks again each minute while it is on screen.`,
    );
    assert.equal(t.keeping.canKeep(ISSUER, false), false);
    // the next pass that gets through settles them, and clears it
    purse.answer = new FakePurse([]).answer;
    await t.keeping.pass(t.run);
    assert.equal(t.keeping.trouble.size, 0);
    assert.equal(purse.outgoing.length, 0);
  });

  test('reads the store again when another change saved first, and says so the second time', async () => {
    const purse = new FakePurse([held(1, 64n, 0)]);
    const changed = Object.assign(new Error('store changed'), { name: 'WorglError', code: 'store_changed' });
    const t = passWith(purse, {
      keepServer: async () => {
        throw changed;
      },
    });
    const first = await t.keeping.pass(t.run);
    assert.equal(first.soon, true);
    assert.equal(t.reopened(), 1);
    assert.equal(t.keeping.trouble.size, 0);
    const second = await t.keeping.pass(t.run);
    assert.equal(second.soon, false);
    assert.equal(second.trouble, true);
    assert.deepEqual(t.keeping.trouble.get(ISSUER), { words: keepStoreWords, kind: 'store' });
  });

  test('stopped, keeps what was answered and says nothing of the rest', async () => {
    const purse = new FakePurse([held(1, 64n, 0), held(2, 64n, 0)]);
    const t = passWith(purse);
    purse.stopAfter = { count: 1, controller: t.controller };
    const end = await t.keeping.pass(t.run);
    assert.deepEqual(t.logged.map((entry) => [entry.kind, entry.amount, entry.serials]), [['kept', 64, ['8:101']]]);
    assert.deepEqual(t.kept, [ISSUER]);
    assert.equal(purse.outgoing.length, 1, 'the unsent hop stays outgoing');
    assert.deepEqual(t.finished, [], 'no end words after a stop');
    assert.equal(end.trouble, false);
    assert.equal(t.keeping.trouble.size, 0);
  });

  test('hurried for a send, sends the hops in hand at once, plans nothing more, logs what it kept, and asks for a pass after', async () => {
    const purse = new FakePurse([held(1, 64n, 0), held(2, 32n, 0), held(3, 16n, 0)]);
    const t = passWith(purse);
    purse.stopAfter = { count: 1, controller: t.hurry };
    const end = await t.keeping.pass(t.run);
    assert.deepEqual(purse.calls, ['plan', 'submit 1 3 5']);
    assert.deepEqual(purse.sentNow, [false, true, true], 'the first at its time, the rest at once');
    assert.equal(purse.outgoing.length, 0, 'no hop left unsent');
    assert.deepEqual(t.logged.map((entry) => [entry.kind, entry.amount]), [['kept', 112]]);
    assert.deepEqual(t.finished, [112], 'what was kept is said');
    assert.deepEqual(t.reads, [false], 'and the lock goes to the send without a read');
    assert.equal(t.keeping.at, undefined, 'at no server once over');
    assert.equal(end.again, false, 'its mint was planned');
    // hurried in the settle: the fading notes wait for the pass after the send
    const left = held(4, 8n, 0);
    const second = new FakePurse([held(1, 64n, 0)]);
    second.outgoing.push(transfer([textOf(left)]));
    const u = passWith(second);
    second.stopAfter = { count: 1, controller: u.hurry };
    const after = await u.keeping.pass(u.run);
    assert.deepEqual(second.calls, ['settle 2-5']);
    assert.equal(after.again, true, 'one more pass for the mint it did not plan');
  });

  test('moves on, in the same pass, a note a resend brought into the first layer, and counts its tokens once', async () => {
    // a hop sent before the rotation, its answer lost: its note's layer is gone now, and the answer lands in the first
    const before = held(1, 64n, 0);
    const resent = { ...before, serial: { generation: OLDEST - 1n, index: 1 } };
    const purse = new FakePurse([held(2, 64n, 1)]);
    purse.outgoing.push(transfer([textOf(resent)]));
    const answer = purse.answer;
    purse.answer = (request) => {
      const notes = answer(request);
      if ('notes' in notes) {
        // the SDK keeps the answer's note: one generation on, now the first layer
        for (const note of decodeNoteParts(request)) {
          purse.notes.push(held(100 + note.index, note.amount, Number(note.generation + 1n - OLDEST)));
        }
      }
      return notes;
    };
    const t = passWith(purse);
    const end = await t.keeping.pass(t.run);
    assert.deepEqual(purse.calls, ['settle 2-5', 'plan', 'submit 1']);
    assert.deepEqual(t.hops, ['6:1', '7:101']);
    assert.deepEqual(
      t.logged.map((entry) => [entry.kind, entry.amount, entry.serials, entry.spent]),
      [['kept', 64, ['8:201'], [{ id: '6:1', v: 64 }]]],
    );
    assert.deepEqual(t.finished, [64]);
    assert.deepEqual(t.shown.at(-2), { mint: ISSUER, amount: 64, done: 2, total: 2, spaced: true });
    assert.equal(end.again, false);
  });

  test('asks for one more pass when its last read finds fading notes it did not plan for', async () => {
    const purse = new FakePurse([held(1, 64n, 1)]);
    const t = passWith(purse, {
      finish: async () => {
        // a rotation came during the pass: the read after it finds the note in the first layer
        t.keeping.saw(ISSUER, { info: mintInfo(), skew: 0, notes: [held(1, 64n, 0)], total: 64n, firstExpiry: undefined, moving: [] });
      },
    });
    const end = await t.keeping.pass(t.run);
    assert.equal(end.again, true);
    // planned this pass: no more
    const second = new FakePurse([held(1, 64n, 0)]);
    const u = passWith(second, {
      finish: async () => {
        u.keeping.saw(ISSUER, { info: mintInfo(), skew: 0, notes: [held(1, 64n, 0)], total: 64n, firstExpiry: undefined, moving: [] });
      },
    });
    assert.equal((await u.keeping.pass(u.run)).again, false);
  });

  test('counts in the run only the hops whose note is in a layer the mint keeps, or the one it dropped last', async () => {
    const purse = new FakePurse([held(1, 64n, 1)]);
    // its layer and the next are gone: the mint refuses it as expired, and Recent says it faded
    const gone = { ...held(2, 32n, 0), serial: { generation: OLDEST - 2n, index: 2 } };
    const doubt = { ...held(3, 64n, 0), serial: { generation: OLDEST - 1n, index: 3 } };
    purse.outgoing.push(transfer([textOf(gone)]), transfer([textOf(doubt)]));
    purse.answer = (request) =>
      decodeNoteParts(request)[0]?.generation === OLDEST - 2n
        ? { refused: { error: new MintError('expired', 409), atStake: undefined, outcome: 'dropped' } }
        : new FakePurse([]).answer(request);
    const t = passWith(purse);
    await t.keeping.pass(t.run);
    assert.deepEqual(t.shown[0], { mint: ISSUER, amount: 64, done: 0, total: 1, spaced: true });
    assert.deepEqual(t.shown.at(-2), { mint: ISSUER, amount: 64, done: 1, total: 1, spaced: true });
    assert.deepEqual(
      t.logged.map((entry) => [entry.kind, entry.amount]),
      [
        ['faded', 32],
        ['kept', 64],
      ],
    );
    // the note that faded, so the purse never logs the fade before it came
    assert.deepEqual(t.logged[0]?.ids, [`${OLDEST - 2n}:2`]);
  });

  test('says the same words from one refused hop to the next and from one pass to the next', async () => {
    const purse = new FakePurse([held(1, 64n, 0), held(2, 64n, 0)]);
    purse.answer = () => ({ refused: { error: new TypeError('Failed to fetch'), atStake: undefined, outcome: 'kept' } });
    let told = 0;
    const t = passWith(purse, {
      troubled: () => {
        told += 1;
      },
    });
    await t.keeping.pass(t.run);
    const words = t.keeping.trouble.get(ISSUER)?.words;
    assert.match(words ?? '', /so 128 tokens could not move/);
    assert.equal(told, 1, 'said once, not once per hop');
    // a minute on: both resent, both refused again
    await t.keeping.pass(t.run);
    assert.equal(t.keeping.trouble.get(ISSUER)?.words, words);
    assert.equal(told, 1, 'nothing new to say');
  });

  test('says what fades and when when the mint does not answer, from the last read of an earlier visit, or what was saved', async () => {
    const purse = new FakePurse([held(1, 64n, 1)]);
    let down = false;
    const read = async (): Promise<Read> => {
      if (down) {
        throw new TypeError('Failed to fetch');
      }
      return { info: mintInfo(), skew: 0, notes: [...purse.notes], total: 64n, firstExpiry: NEXT + 28_800n, moving: [] };
    };
    const t = passWith(purse, { read });
    await t.keeping.pass(t.run);
    // hidden and shown again: a new visit, and the mint does not answer
    t.keeping.forget();
    down = true;
    await t.keeping.pass(t.run);
    const now = (Number(NEXT) - 3600) * 1000;
    assert.equal(
      t.keeping.trouble.get(ISSUER)?.words,
      `The mint at 127.0.0.1:9 did not answer. As the purse last saw them, 64 tokens fade at ${when(Number(NEXT + 28_800n) * 1000, now)} unless the purse reaches it before then. The purse asks again each minute while it is on screen.`,
    );
    // a fresh open, with only what the purse saved
    const u = passWith(purse, { read, saved: () => ({ at: Number(NEXT) * 1000, amount: 4 }) });
    await u.keeping.pass(u.run);
    assert.match(u.keeping.trouble.get(ISSUER)?.words ?? '', /As the purse last saw them, 4 tokens fade at /);
  });

  test('leaves a send’s transfers out of the count and the log', async () => {
    const sent = held(5, 32n, 1);
    const purse = new FakePurse([held(1, 64n, 1)]);
    purse.outgoing.push(transfer([textOf(sent)], 2));
    const key = `${ISSUER}/${sent.serial.generation}:${sent.serial.index}`;
    const t = passWith(purse, { outgoing: async () => ({ outgoing: [...purse.outgoing], sendInputs: new Set([key]) }) });
    await t.keeping.pass(t.run);
    assert.deepEqual(purse.calls, ['settle 2-5']);
    assert.equal(purse.outgoing.length, 0, 'settled');
    assert.deepEqual(t.logged, []);
    assert.deepEqual(t.shown, []);
    assert.deepEqual(t.hops, []);
  });

  test('leaves a mint alone while Send shows a plan of its notes, until its rotation is near', async () => {
    const left = held(3, 16n, 0);
    const purse = new FakePurse([held(1, 64n, 0), held(2, 32n, 2)]);
    // a hop an earlier pass left unsent: landing it would change the plan too
    purse.outgoing.push(transfer([textOf(left)]));
    const t = passWith(purse, { showsPlan: (mint) => mint === ISSUER });
    const end = await t.keeping.pass(t.run);
    assert.deepEqual(purse.calls, [], 'nothing settled, nothing planned');
    assert.equal(purse.outgoing.length, 1);
    assert.deepEqual(t.logged, []);
    assert.equal(end.trouble, true, 'tries again in a minute');
    assert.equal(end.again, false, 'not at once');
    assert.equal(t.keeping.canKeep(ISSUER, false), true, 'no trouble words');
    // ten minutes before the rotation, keeping the tokens alive comes first
    const near = passWith(purse, { showsPlan: () => true, now: () => (Number(NEXT) - 600) * 1000 });
    await near.keeping.pass(near.run);
    assert.deepEqual(purse.calls, ['settle 2-5', 'plan', 'submit 1']);
    assert.equal(purse.outgoing.length, 0);
  });

  test('while Send shows a plan of one mint, still settles what the other mints of its server have outgoing', async () => {
    const mine = held(1, 16n, 0);
    const other = { ...held(2, 8n, 0), serial: { generation: OLDEST, index: 2 } };
    const otherText = noteText({ issuer: OTHER, generation: OLDEST, index: 2, amount: 8n });
    const purse = new FakePurse([held(3, 64n, 2)]);
    purse.outgoing.push(transfer([textOf(mine)]), transfer([otherText]));
    const servers = [{ server: SERVER, mints: [{ mint: ISSUER, host: '127.0.0.1:9' }, { mint: OTHER, host: '127.0.0.1:9' }] }];
    const t = passWith(purse, { servers: () => servers, showsPlan: (mint) => mint === ISSUER });
    await t.keeping.pass(t.run);
    assert.deepEqual(purse.calls, ['settle 2-5']);
    assert.equal(purse.outgoing.length, 1, 'the plan mint’s hop waits with the plan');
    assert.deepEqual(decodeNoteParts(purse.outgoing[0] ?? '').map((note) => note.issuer), [ISSUER]);
    assert.equal(other.serial.index, 2);
  });
});
