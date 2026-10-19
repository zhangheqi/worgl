import { type Client, MintError, isTransient } from './client.ts';
import {
  Holder,
  type Scheduled,
  checkPayment,
  noteIssuer,
  paymentMint,
  paymentTransfers,
  requestAmount,
  requestMint,
  requestNotes,
  verifyNotes,
} from './core.ts';
import { MemoryStore, type Store } from './store.ts';

/**
 * How far apart, in seconds, to submit the transfers of one payment, or to
 * resend outgoing transfers. Each gap is drawn at random from the range.
 * Closer spacing is faster but lets the mint guess, from timing alone, that
 * the notes had one owner.
 */
export interface Spacing {
  minSecs: number;
  maxSecs: number;
}

/** Gaps of 2 to 60 seconds. */
export const DEFAULT_SPACING: Spacing = { minSecs: 2, maxSecs: 60 };

/** No gaps: for tests, or when timing does not matter. */
export const NO_SPACING: Spacing = { minSecs: 0, maxSecs: 0 };

/**
 * How a purse resends a transfer that failed on the way: a lost connection,
 * a timeout, or a mint that was paused, busy or could not store it.
 * Resending is safe, since the mint answers an identical transfer with
 * identical notes, before or after a rotation, for as long as it remembers
 * the transfer (see {@link Purse.settle}).
 *
 * When the mint answers `busy`, the next wait is as long as its
 * `Retry-After` asks, up to 30 seconds, if that is longer; the waits after
 * it double from where they were. A `Retry-After` on any other answer, such
 * as a proxy's, changes no wait. The mint answers `busy` only to a request
 * it did not apply, so with the default and a 30-second request timeout,
 * the client's default, the last try still ends within 127 seconds of any
 * the mint may have applied, inside the 128 seconds a mint keeps each
 * answer by default, and is answered as before.
 */
export interface Retry {
  /** Tries in all, the first included. */
  attempts: number;
  /** Wait before the second try, in milliseconds; it doubles after. */
  firstDelayMs: number;
}

/** Four tries, one, two and four seconds apart. */
export const DEFAULT_RETRY: Retry = { attempts: 4, firstDelayMs: 1000 };

/** Longest wait honored before submitting a transfer: a year, in seconds. */
export const MAX_DELAY_SECS = 365 * 24 * 60 * 60;

/** Longest a mint's `Retry-After` makes a purse wait before a retry, in milliseconds. */
const MAX_RETRY_AFTER_MS = 30_000;

/** A signed transfer, as JSON, and when to submit it. */
export interface ScheduledTransfer {
  /** Seconds to wait, counted from when submitting starts. */
  afterSecs: number;
  /** The transfer, as JSON. */
  request: string;
}

/** Value held at one mint, by what it can do now. */
export interface Balance {
  /** Can be spent now. */
  spendable: bigint;
  /** Frozen in the last layer until the next rotation. */
  frozen: bigint;
  /** Dies at the next rotation unless refreshed (spendable or frozen). */
  expiring: bigint;
  /** In notes newer than the mint's info. */
  unknown: bigint;
  /** In transfers sent and not yet seen answered. */
  outgoing: bigint;
  /** Asked for and not yet received. */
  expected: bigint;
}

/** A transfer that did not go through, and why. */
export interface Refused {
  /** The transfer, as JSON. */
  request: string;
  /** The last error; {@link isTransient} tells whether to try again later. */
  error: unknown;
}

/** What submitting a set of transfers came to. */
export interface Submitted {
  /** Value the purse took in: payments to it and its own change. */
  value: bigint;
  /** Transfers the mint answered. */
  answered: number;
  /** Every note in the answers, the counterparty's included, to hand on. */
  notes: string[];
  /** Transfers the mint refused, or that kept failing on the way. */
  refused: Refused[];
}

/**
 * What a payee asks to be paid: one fresh key of its own for each note.
 * Send {@link json} to the payer, who reads it with {@link PaymentRequest.from}.
 */
export class PaymentRequest {
  /** The request as JSON, to send. */
  readonly json: string;
  /** The issuer key of the mint whose notes it accepts. */
  readonly mint: string;
  /** The total asked for. */
  readonly amount: bigint;
  /** Each note asked for: its value and the payee's one-time key. */
  readonly notes: { amount: bigint; holder: string }[];

  private constructor(json: string) {
    this.json = json;
    this.mint = requestMint(json);
    this.amount = requestAmount(json);
    this.notes = requestNotes(json).map((note) => {
      const read = { amount: note.amount, holder: note.holder };
      note.free();
      return read;
    });
  }

  /** Reads a request; throws if it is not one. */
  static from(json: string): PaymentRequest {
    return new PaymentRequest(json);
  }
}

/**
 * The payer's answer to a {@link PaymentRequest}: signed transfers, each
 * with a time to submit it. Send {@link json} to the payee.
 */
export class Payment {
  /** The payment as JSON, to send. */
  readonly json: string;
  /** The issuer key of the mint the transfers go to. */
  readonly mint: string;
  /** The transfers and their times. */
  readonly transfers: ScheduledTransfer[];

  private constructor(json: string) {
    this.json = json;
    this.mint = paymentMint(json);
    this.transfers = read(paymentTransfers(json));
  }

  /** Reads a payment; throws if it is not one. */
  static from(json: string): Payment {
    return new Payment(json);
  }
}

/** How a {@link Purse} is set up. */
export interface PurseOptions {
  /** Where it saves itself; in memory by default. */
  store?: Store;
  /** How it retries failed transfers. */
  retry?: Retry;
}

/**
 * A holder connected to a server and a {@link Store}: it reads mint info
 * itself, submits transfers at their scheduled times, retries what failed
 * on the way, and saves after every change.
 *
 * The privacy rules come from the Rust core: a fresh key for every note,
 * one note per transfer, and transfers of one payment, refreshes and
 * resends spread out in time.
 *
 * Nothing that depends on a change leaves the purse before the change is
 * saved. If a save fails, the change is undone and the call rejects.
 * Changes are applied and saved one at a time, so calls may overlap.
 */
export class Purse {
  /** The client requests go through. */
  readonly client: Client;
  /** The store saves go to. */
  readonly store: Store;
  readonly #holder: Holder;
  readonly #retry: Retry;
  #changes: Promise<unknown> = Promise.resolve();

  private constructor(client: Client, holder: Holder, store: Store, retry: Retry) {
    this.client = client;
    this.#holder = holder;
    this.store = store;
    this.#retry = retry;
  }

  /** A purse loaded from its store, or a new empty one. */
  static async open(client: Client, options: PurseOptions = {}): Promise<Purse> {
    const store = options.store ?? new MemoryStore();
    const saved = await store.load();
    const holder = saved === undefined ? new Holder() : Holder.fromJson(saved);
    return new Purse(client, holder, store, options.retry ?? DEFAULT_RETRY);
  }

  /** The whole purse as JSON, secret keys included. */
  toJson(): string {
    return this.#holder.toJson();
  }

  /** The notes held, of every mint. */
  notes(): string[] {
    return JSON.parse(this.#holder.notes()) as string[];
  }

  /** Value held at `mint`, from its info. */
  async balance(mint: string): Promise<Balance> {
    const info = await this.client.infoText(mint);
    const balance = this.#holder.balance(info, now());
    try {
      return {
        spendable: balance.spendable,
        frozen: balance.frozen,
        expiring: balance.expiring,
        unknown: balance.unknown,
        outgoing: balance.outgoing,
        expected: balance.expected,
      };
    } finally {
      balance.free();
    }
  }

  /**
   * Asks for `amount` from `mint`: a payment request with a fresh key per
   * note, saved before it is returned. Send it to the payer.
   */
  async request(mint: string, amount: bigint): Promise<PaymentRequest> {
    const info = await this.client.infoText(mint);
    const json = await this.#change(
      () => this.#holder.request(info, now(), amount),
      (json) => this.#holder.withdraw(json),
    );
    return PaymentRequest.from(json);
  }

  /** Like {@link request}, in notes of at most `largest`, for a payer whose notes are small. */
  async requestAtMost(mint: string, amount: bigint, largest: bigint): Promise<PaymentRequest> {
    const info = await this.client.infoText(mint);
    const json = await this.#change(
      () => this.#holder.requestAtMost(info, now(), amount, largest),
      (json) => this.#holder.withdraw(json),
    );
    return PaymentRequest.from(json);
  }

  /**
   * Asks a playground faucet for a note and keeps it. The faucet's mint must
   * be on this purse's server; the faucet may share its URL. The note goes to
   * a fresh key, saved before the faucet is asked, as for any payment
   * request. Resolves to the faucet's mint and the value kept.
   */
  async takeFromFaucet(faucet: Client): Promise<{ mint: string; value: bigint }> {
    const about = await faucet.faucet();
    const request = await this.requestAtMost(about.mint, about.amount, about.amount);
    const [output, ...rest] = request.notes;
    if (output === undefined || rest.length > 0) {
      await this.withdraw(request);
      throw new Error(
        `the faucet hands out notes of ${about.amount}, which its mint cannot make as one note`,
      );
    }
    try {
      const note = await faucet.askFaucet(output.holder);
      return { mint: about.mint, value: await this.accept([note]) };
    } catch (error) {
      await this.withdraw(request);
      throw error;
    }
  }

  /** Stops expecting the notes of a request that will not be paid. */
  async withdraw(request: PaymentRequest): Promise<void> {
    await this.#change(
      () => this.#holder.withdraw(request.json),
      () => {},
    );
  }

  /**
   * Answers a payment request, after reading the mint's info. The notes
   * spent and the keys for the change are saved before the payment is
   * returned. Send it to the payee, then {@link settle} later.
   */
  async pay(request: PaymentRequest, spacing: Spacing = DEFAULT_SPACING): Promise<Payment> {
    const info = await this.client.infoText(request.mint);
    const json = await this.#change(
      () =>
        this.#holder.pay(
          info,
          now(),
          request.json,
          BigInt(spacing.minSecs),
          BigInt(spacing.maxSecs),
        ),
      (json) => {
        for (const transfer of Payment.from(json).transfers) {
          this.#holder.cancel(transfer.request);
        }
      },
    );
    return Payment.from(json);
  }

  /**
   * Takes a payment for `request`: checks that it pays what was asked, then
   * submits each transfer at its time and takes in the notes. Resolves once
   * the last transfer is answered or refused. The payer chose the times.
   */
  async receive(request: PaymentRequest, payment: Payment): Promise<Submitted> {
    checkPayment(request.json, payment.json);
    return this.submit(payment.transfers);
  }

  /**
   * Takes in notes the counterparty handed over from the mint's answers,
   * after checking each one's signature against its mint's keys. Notes for
   * keys this purse never handed out are ignored. Returns the value taken in.
   */
  async accept(notes: string[]): Promise<bigint> {
    const byMint = new Map<string, string[]>();
    for (const note of notes) {
      const mint = noteIssuer(JSON.stringify(note));
      byMint.set(mint, [...(byMint.get(mint) ?? []), note]);
    }
    for (const [mint, own] of byMint) {
      verifyNotes(await this.client.keysText(mint), JSON.stringify(own));
    }
    return this.#change(
      () => this.#holder.absorb(JSON.stringify(notes)),
      () => {},
    );
  }

  /**
   * Resends the transfers this purse made and has not seen answered, at
   * random gaps, and takes in their change. Call it once a payee has had
   * time to submit a payment, or after a restart.
   *
   * The mint keeps the answers to its most recent transfers only, and each
   * only until the notes it issued expire: settle before many more have
   * passed and while the change is live, or a transfer already applied is
   * refused as `spent` or `expired` and its change is lost.
   */
  async settle(spacing: Spacing = DEFAULT_SPACING): Promise<Submitted> {
    const transfers = read(this.#holder.resend(BigInt(spacing.minSecs), BigInt(spacing.maxSecs)));
    return this.submit(transfers);
  }

  /**
   * Plans the refresh of `mint`'s notes that die at the next rotation, after
   * reading its info, and saves the plan. Submit it with {@link submit}.
   */
  async planRefresh(mint: string): Promise<ScheduledTransfer[]> {
    const info = await this.client.infoText(mint);
    return this.#change(
      () => read(this.#holder.refresh(info, now())),
      (transfers) => {
        for (const transfer of transfers) {
          this.#holder.cancel(transfer.request);
        }
      },
    );
  }

  /**
   * Refreshes `mint`'s notes that die at the next rotation, each at a random
   * moment before it. Resolves once the last is done, which may be most of
   * a rotation period away.
   */
  async refresh(mint: string): Promise<Submitted> {
    return this.submit(await this.planRefresh(mint));
  }

  /** Drops `mint`'s expired notes and returns the value lost. */
  async prune(mint: string): Promise<bigint> {
    const info = await this.client.infoText(mint);
    return this.#change(
      () => this.#holder.prune(info, now()),
      () => {},
    );
  }

  /**
   * Submits each transfer at its time, counted from now, retrying failures
   * on the way, and records each answer. A refused transfer does not stop
   * the others; it is listed in {@link Submitted.refused}. Rejects only if a
   * save fails.
   */
  async submit(transfers: ScheduledTransfer[]): Promise<Submitted> {
    const order = [...transfers].sort((a, b) => a.afterSecs - b.afterSecs);
    const start = Date.now();
    const submitted: Submitted = { value: 0n, answered: 0, notes: [], refused: [] };
    for (const transfer of order) {
      await sleepUntil(start + Math.min(transfer.afterSecs, MAX_DELAY_SECS) * 1000);
      let notes: string[];
      try {
        notes = await this.#send(transfer.request);
      } catch (error) {
        submitted.refused.push({ request: transfer.request, error });
        continue;
      }
      submitted.value += await this.#change(
        () => this.#holder.settle(transfer.request, JSON.stringify(notes)),
        () => {},
      );
      submitted.answered += 1;
      submitted.notes.push(...notes);
    }
    return submitted;
  }

  /** Sends one transfer, retrying while the failure is transient. */
  async #send(transfer: string): Promise<string[]> {
    let delay = this.#retry.firstDelayMs;
    for (let attempt = 1; ; attempt += 1) {
      try {
        return await this.client.transfer(transfer);
      } catch (error) {
        if (!isTransient(error) || attempt >= this.#retry.attempts) {
          throw error;
        }
        await sleep(nextWait(delay, error));
        delay *= 2;
      }
    }
  }

  /**
   * Changes the holder and saves it, after any change before it is saved.
   * If the save fails, `undo` takes the change back and the call rejects.
   */
  #change<T>(apply: () => T, undo: (value: T) => void): Promise<T> {
    const run = async (): Promise<T> => {
      const value = apply();
      try {
        await this.store.save(this.#holder.toJson());
      } catch (error) {
        undo(value);
        throw error;
      }
      return value;
    };
    const result = this.#changes.then(run, run);
    this.#changes = result.catch(() => {});
    return result;
  }
}

/** Copies scheduled transfers out of WebAssembly memory. */
function read(transfers: Scheduled[]): ScheduledTransfer[] {
  return transfers.map((transfer) => {
    const copy = { afterSecs: Number(transfer.afterSecs), request: transfer.request };
    transfer.free();
    return copy;
  });
}

/** Seconds since the Unix epoch, as mints count time. */
function now(): bigint {
  return BigInt(Math.floor(Date.now() / 1000));
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * The wait before the next try after `error`, in milliseconds: `delay`, or
 * after `busy` what the mint's `Retry-After` asks if that is longer, up to
 * 30 seconds. It stretches this wait only, never the ones after.
 *
 * Only `busy` stretches it: the mint answers so only to a request it did
 * not apply. Another answer, such as a proxy's, may come after the mint
 * applied the try, and longer waits could take the last try past the time
 * the mint keeps the receipt for.
 */
function nextWait(delay: number, error: unknown): number {
  const asked =
    error instanceof MintError && error.code === 'busy' ? error.retryAfterMs : undefined;
  return asked === undefined ? delay : Math.max(delay, Math.min(asked, MAX_RETRY_AFTER_MS));
}

/** Waits until `deadline`, in milliseconds since the epoch, in steps that timers can hold. */
async function sleepUntil(deadline: number): Promise<void> {
  for (let left = deadline - Date.now(); left > 0; left = deadline - Date.now()) {
    await sleep(Math.min(left, 2 ** 31 - 1));
  }
}
