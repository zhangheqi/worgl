import { type Backup, type BackupMint, mintList, seal } from './backup.ts';
import {
  type Client,
  MintError,
  type NoteState,
  type RequestOptions,
  isTransient,
} from './client.ts';
import {
  Holder,
  type Scheduled,
  breaksPolicy,
  checkPayment,
  noteIssuer,
  paymentMint,
  paymentTransfers,
  requestAmount,
  requestMint,
  requestNotes,
  transferMint,
  verifyNotes,
} from './core.ts';
import { type WorglError, isWorglError, worglError } from './error.ts';
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
 * the transfer (see {@link Purse.settle}). The mint's keys, which the purse
 * reads to check an answer with, are read again the same way.
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

/**
 * Value a purse has at one mint, in parts that do not overlap: each unit of
 * value counts in exactly one of `held` (`spendable`, `frozen` and
 * `unknown`), `sending`, `change`, `refreshing` and `requested`.
 *
 * `total`, what is held plus change and refreshing, is what the purse has
 * there. Paying lowers it by the amount paid, which counts as sending until
 * the transfers are answered; submitting them, settling their answers,
 * refreshing and asking to be paid leave it as it is. Taking in a payment
 * raises it by the value that counted as requested, and a rotation lowers
 * it by the notes held in the layer it drops. Cancelling a transfer raises
 * it by what the transfer paid others, since the note it spends is back;
 * forgetting one lowers it by its change. Either way the change moves to
 * unconfirmed.
 *
 * `expiring` is part of what is held, `unchecked` part of the total,
 * `outgoing` and `expected` sum some of the parts, and `unconfirmed` may
 * come instead of value held, so a screen adds none of them in.
 */
export interface Balance {
  /** Held now, or on its way back as change or refreshing. */
  total: bigint;
  /** Held now: spendable, frozen and unknown together. */
  held: bigint;
  /** Held, and can be spent now. */
  spendable: bigint;
  /** Held frozen in the last layer until the next rotation. */
  frozen: bigint;
  /** Held, and dies at the next rotation unless refreshed. */
  expiring: bigint;
  /**
   * Part of the total that came from another copy of the purse in a
   * {@link Purse.restore}, which may have spent it since: the notes
   * {@link Purse.unchecked} lists, and change and refreshes paid to keys
   * that copy handed out. It counts until the mint says otherwise: see
   * {@link Purse.checkNotes}.
   */
  unchecked: bigint;
  /** Held in notes newer than the mint's info. */
  unknown: bigint;
  /** What transfers not yet answered pay others: the payees' notes. */
  sending: bigint;
  /** What transfers not yet answered that pay others too pay back: their change. */
  change: bigint;
  /**
   * What transfers not yet answered that pay this purse alone pay it back,
   * such as the refreshes {@link Purse.planRefresh} plans.
   */
  refreshing: bigint;
  /** Asked for in payment requests and not yet received. */
  requested: bigint;
  /**
   * Change of transfers dropped or cancelled, which comes only if the
   * counterparty hands it over: see {@link Purse.forget}. It is in neither
   * `held` nor `total`: after a {@link Purse.cancel}, it comes only if the
   * mint spent the note taken back.
   */
  unconfirmed: bigint;
  /**
   * In notes spent by transfers not yet answered: sending, change and
   * refreshing together, plus any of their notes for this purse taken in
   * before they were settled, such as change handed over in part.
   */
  outgoing: bigint;
  /** Expected from keys handed out and not yet paid: requested, change and refreshing together. */
  expected: bigint;
}

/**
 * A note a purse holds and where it stands at its mint, from the mint's info
 * alone: see {@link Purse.notesAt}. It holds no more of the note than its
 * serial and value. Times are Unix seconds by the mint's clock, from the next
 * rotation its info names. Both are `undefined` when the mint has no schedule
 * or is paused, or when the note is `expired` or `unknown`, and `spendableAt`
 * is `undefined` too for a note that is not `frozen`.
 */
export interface HeldNote {
  /** Its serial, by which the mint knows it. */
  serial: { generation: bigint; index: number };
  /** Its value. */
  amount: bigint;
  /**
   * What it can do now:
   *
   * - `spendable`: it can be spent, and outlives the next rotation.
   * - `expiring`: it can be spent, and dies at the next rotation unless it
   *   moves first: {@link Purse.refresh} pays it back to the purse.
   * - `frozen`: it sits in the last layer, where the notes of a transfer
   *   usually land, and can move only after the next rotation.
   * - `expired`: the mint has dropped its layer, so it is worth nothing;
   *   {@link Purse.prune} drops it.
   * - `unknown`: it is newer than the mint's info, which cannot place it, as
   *   when the info is a copy saved before the rotation that opened its
   *   layer.
   */
  state: 'spendable' | 'expiring' | 'frozen' | 'expired' | 'unknown';
  /**
   * The position of its layer, 0 for the first, unless it is expired or
   * unknown. Untouched, a note at position `p` dies `p + 1` rotations from
   * now.
   */
  position: number | undefined;
  /**
   * For a frozen note, when it can move: the time of the next rotation.
   * `undefined` for any other note.
   */
  spendableAt: bigint | undefined;
  /**
   * When the rotation that drops its layer comes, unless it moves first.
   * `undefined` for an expired or unknown note, and for every note at a mint
   * with no schedule or paused.
   */
  expiresAt: bigint | undefined;
}

/**
 * What a purse did with a transfer the mint refused.
 *
 * - `kept`: nothing. A transfer this purse made stays outgoing, for the next
 *   {@link Purse.settle}. So it is after a refusal that may pass later, one
 *   {@link isTransient} accepts or `frozen`, and after one that shows nothing
 *   about the transfer: `unknown_mint` or `unissued` from a server that is
 *   not the mint's or lost its state, `spent` or `expired` with another
 *   status than the mint's 409, `layer_full` with another than its 503, or
 *   `unauthorized` or `duplicate_input`, which the mint never answers to a
 *   transfer a holder signed. So it is, too, after an answer the purse kept
 *   none of (see {@link Submitted.refused}).
 * - `dropped`: refused as `spent` or `expired`, with the mint's status 409,
 *   so the note it spends is gone and the purse forgot the transfer
 *   ({@link Purse.forget}). Change the counterparty hands over later is still
 *   taken in by {@link Purse.accept}.
 * - `cancelled`: refused as `layer_full`, with the mint's status 503, or as
 *   `policy`, when the policy the mint publishes now refuses it too. The
 *   purse cancelled it and took its note back ({@link Purse.cancel}). The
 *   mint checks the note before the layer the new notes would land in, so
 *   after `layer_full` the note is live and the transfer was never applied,
 *   and it never will be: that layer gets no bits back before the note's
 *   own is dropped.
 */
export type Outcome = 'kept' | 'dropped' | 'cancelled';

/** A transfer that did not go through, or whose answer was not kept, and why. */
export interface Refused {
  /** The transfer, as JSON. */
  request: string;
  /** The last error; {@link isTransient} tells whether to try again later. */
  error: unknown;
  /**
   * For a transfer refused as `spent` or `expired` that this purse made or
   * is paid by, what it may lose, as it stood before the transfer was
   * dropped: the transfer may have been applied, with an answer the mint no
   * longer keeps. Only a counterparty that submitted it can then hand the
   * notes over, for {@link Purse.accept}. `undefined` otherwise.
   */
  atStake: bigint | undefined;
  /** What the purse did with the transfer. */
  outcome: Outcome;
}

/** What submitting a set of transfers came to. */
export interface Submitted {
  /** Value the purse took in: payments to it and its own change. */
  value: bigint;
  /** Transfers the mint answered. */
  answered: number;
  /** Every note in the answers, this purse's own included. */
  notes: string[];
  /**
   * The notes in the answers for neither a note this purse holds nor a key
   * it expects: the counterparty's. Hand them over for its
   * {@link Purse.accept}: a payee hands the payer its change, and a payer
   * that submitted a payment hands the payee its notes. The payer's purse
   * keeps those for the payee too, until {@link Purse.forgetOwed} says they
   * are handed over.
   */
  theirs: string[];
  /**
   * Transfers the mint refused, that kept failing on the way, or whose answer
   * the purse kept none of: one that is not the mint's, or that it could not
   * read the mint's keys to check. A transfer on its way when
   * {@link SubmitOptions.signal} aborted is here too, as one whose answer was
   * lost.
   */
  refused: Refused[];
  /**
   * Transfers not sent, because {@link SubmitOptions.signal} aborted before
   * their turn, in the order they would have gone, as they were given. Those
   * this purse made stay outgoing, for {@link Purse.settle}; submit the others
   * again to carry them out.
   */
  unsent: ScheduledTransfer[];
}

/**
 * Where one transfer of a {@link Purse.submit} stands, as
 * {@link SubmitOptions.onProgress} hears of it.
 */
export interface SubmitProgress {
  /**
   * What just happened to the transfer:
   *
   * - `waiting`: it waits for its time, `sendAt`. A transfer whose time has
   *   come, or every one after {@link SubmitOptions.sendNow}, skips this step.
   * - `sending`: it is on its way to the mint, retries included; the purse
   *   then checks the answer against the mint's keys and keeps it.
   * - `answered`: the mint answered it and the purse kept the answer.
   * - `refused`: it did not go through, or its answer was not kept, and it
   *   is listed in {@link Submitted.refused}; `refusal` says why, and
   *   whether the purse keeps it for later.
   * - `unsent`: it is listed in {@link Submitted.unsent}.
   */
  step: 'waiting' | 'sending' | 'answered' | 'refused' | 'unsent';
  /** The transfer, as it was given. */
  transfer: ScheduledTransfer;
  /** Its place in the order the transfers are sent in, from 0. */
  index: number;
  /** Transfers in all. */
  total: number;
  /** Transfers answered so far. */
  answered: number;
  /** Transfers refused so far. */
  refused: number;
  /**
   * For `waiting`, when the transfer is sent unless the waits are cut short:
   * milliseconds since the epoch by this machine's clock, as `Date.now()`
   * counts them. `undefined` for the other steps.
   */
  sendAt: number | undefined;
  /**
   * For `refused`, the transfer's entry in {@link Submitted.refused}: the
   * error, and with {@link Refused.outcome} whether the purse kept it for a
   * later {@link Purse.settle}, as after a lost answer or a mint that is
   * busy, or dropped or cancelled it for good. `undefined` for the other
   * steps.
   */
  refusal: Refused | undefined;
}

/**
 * How a screen follows a {@link Purse.submit} and cuts it short.
 * {@link Purse.receive}, {@link Purse.settle} and {@link Purse.refresh}, which
 * submit too, take the same.
 */
export interface SubmitOptions {
  /**
   * Called as each transfer moves on, in the order they are sent in: it is
   * `waiting` for its time, then `sending`, then `answered` or `refused`, or
   * `unsent` once submitting stopped before its turn (see
   * {@link SubmitProgress}). Each call carries the counts so far, for a
   * screen to draw "2 of 3" and when the next transfer goes. It is called
   * synchronously, so it should return quickly. The purse goes on past an
   * exception it throws and reports it as uncaught, as an event listener's
   * is: a browser logs it, and Node ends the process, as for any uncaught
   * exception, unless the application handles `uncaughtException`.
   */
  onProgress?: (progress: SubmitProgress) => void;
  /**
   * Cuts the waits short once it aborts: each transfer not yet sent goes at
   * once, one after another. This gives up the spacing between them, so the
   * mint can tell from when they arrive that their notes had one owner (see
   * the {@link https://github.com/zhangheqi/worgl/blob/main/site/src/content/docs/docs/privacy.md#timing | Privacy model}):
   * offer it as "send the rest now" to a person who would rather be quick.
   * A retry after a failure still waits as {@link Retry} says.
   */
  sendNow?: AbortSignal;
  /**
   * Stops submitting once it aborts, as when the person leaves the page: no
   * wait goes on, no transfer is sent after it, and the call resolves with
   * what was done, once a change being saved is saved, so the holder is
   * never left half-saved. The transfer on its way, if any, is abandoned
   * and treated as one whose answer was lost, since the mint may have
   * applied it: it is listed in {@link Submitted.refused} with outcome
   * `kept` and the signal's reason as its error, by default an `AbortError`
   * that {@link isTransient} accepts, and one this purse made stays
   * outgoing, for {@link Purse.settle} to send again. An answer the mint
   * has given already is not abandoned, since a resend gets it again only
   * while the mint keeps it: the purse still reads the mint's keys to check
   * it, within the client's timeout and as {@link Retry} says, and keeps
   * it. The transfers not yet sent are listed in {@link Submitted.unsent}.
   */
  signal?: AbortSignal;
}

/**
 * What {@link Purse.restore} took in from a backup, what the backup showed
 * was done with, and what only the mint can settle. Notes and transfers are
 * JSON.
 *
 * What only the backup had may be out of date: since it was sealed, another
 * copy of the purse, such as this one before it lost its storage, may have
 * spent its notes, or settled its transfers and spent their change. So what
 * this purse knew nothing of comes in unchecked ({@link Purse.unchecked}),
 * and counts in {@link Balance.unchecked} until the mint says otherwise. A
 * backup restored into a new purse is all such.
 */
export interface Restored {
  /**
   * Notes held now that were not before, from the backup. Those this purse
   * knew nothing of are unchecked. A backup a day old mostly holds spent
   * notes on a mint that rotates every few hours: {@link Purse.prune} drops
   * the expired ones from the mint's info alone, and
   * {@link Purse.checkNotes} asks the mint about the rest.
   */
  notes: string[];
  /**
   * Notes held before that the backup spent, in a transfer it settled or
   * forgot: no longer this purse's, they are dropped.
   */
  spent: string[];
  /**
   * Transfers outgoing now that were not before, from the backup.
   * {@link Purse.settle} resends them: the mint carries out one it never
   * applied, answers one it applied with the notes it first gave while it
   * keeps the answer, and refuses one whose note another transfer spent,
   * which is then dropped. The notes of an answer that pay this purse are
   * unchecked, since another copy may have settled the transfer and spent
   * them. Cancel one that should not go ({@link Purse.cancel}): another copy
   * may have cancelled it.
   */
  outgoing: string[];
  /**
   * Transfers outgoing before that the backup showed done with: settled,
   * cancelled or forgotten there, or answered, by holding or spending every
   * note they pay this purse. They are ended here too, keeping what the
   * backup did with their notes.
   */
  ended: string[];
  /**
   * Transfers outgoing now that spend a note another one spends: this purse
   * and the backup each spent it, in a transfer of its own. The mint applies
   * at most one, and {@link Purse.settle} tells which; until then that
   * note's value counts in both.
   */
  conflicts: string[];
  /**
   * The mints of the backup left out, each with the server the backup names
   * for it: restore those into a purse of that server.
   */
  others: BackupMint[];
}

/** What {@link Purse.checkNotes} found. */
export interface Checked {
  /** Value this purse held in the notes the mint reported spent or expired, now discarded. */
  value: bigint;
  /** The notes the mint reported spent or expired. This purse holds none of them now. */
  discarded: string[];
  /** The notes the mint reported live or unissued. They are kept, and no longer unchecked. */
  kept: string[];
  /** The notes the mint could not be asked about, each with the last error. They are kept. */
  unanswered: { note: string; error: unknown }[];
}

/** What {@link Purse.backup} seals with a purse. */
export interface BackupOptions {
  /**
   * Purses of other servers to seal in the same backup, so that one backup
   * and one passphrase hold them all: each names its own server for its
   * mints, and {@link Purse.restore} on a purse of each server takes its
   * part back. A mint may be in only one of the purses.
   */
  purses?: Purse[];
  /**
   * Entries for the backup's mint list, such as the person's mints with the
   * name and unit given to each in {@link BackupMint.extra}, which every
   * reader gives back. A mint one of the purses holds anything of is named
   * with that purse's server, keeping the other members of its entry here;
   * a mint none holds is sealed as listed, if it names a server.
   */
  mints?: BackupMint[];
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

/**
 * A payment a purse made and keeps: while one of its transfers is outgoing,
 * or while it keeps notes owed to the payee.
 */
export interface MadePayment {
  /** The request it answers. */
  request: PaymentRequest;
  /**
   * The payment, to send again to a payee who lost it. A transfer cancelled
   * or forgotten leaves it, so it is never handed out again; the payment
   * then no longer pays the whole request, and the payee would refuse it.
   */
  payment: Payment;
  /**
   * The payee's notes from the mint's answers this purse got itself: each
   * time it settles, refreshes or submits one of the payment's transfers.
   * The purse cannot tell whether the payee submitted the payment and has
   * them already, so it keeps them until they are handed over
   * ({@link Purse.forgetOwed}) or their layer is dropped ({@link Purse.prune}).
   */
  owed: string[];
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
 * on the way, checks each answer against the mint's keys, and saves after
 * every change.
 *
 * The privacy rules come from the Rust core: a fresh key for every note,
 * one note per transfer, and transfers of one payment, refreshes and
 * resends spread out in time.
 *
 * Nothing that depends on a change leaves the purse before the change is
 * saved. If a save fails, the change is undone and the call rejects.
 * Changes are applied and saved one at a time, so calls may overlap.
 *
 * Several purses can share a store that has a {@link Store.lock | lock},
 * as the tabs of a browser share an `IndexedDbStore` from `worgl/browser`.
 * Before each change the purse takes the lock and reads the holder saved
 * there, makes the change to that, and saves before letting go, so a note
 * another purse took in meanwhile is kept, not saved over. It never holds
 * the lock across a request to the mint. {@link settle} resends the
 * transfers of the holder saved now, and {@link backup} seals that holder.
 * Reads such as {@link notes} and {@link balance} show the holder as this
 * purse last read or saved it.
 */
export class Purse {
  /** The client requests go through. */
  readonly client: Client;
  /** The store saves go to. */
  readonly store: Store;
  #holder: Holder;
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

  /**
   * The notes held that came from another copy of the purse in a
   * {@link restore}, which may have spent them since, or that a transfer
   * the restore brought back paid to this purse: see {@link Restored}. They
   * count, in {@link Balance.unchecked}, until {@link checkNotes} asks the
   * mint about them, and only the mint can tell.
   */
  unchecked(): string[] {
    return JSON.parse(this.#holder.unchecked()) as string[];
  }

  /** The transfers this purse made and has not seen answered, as JSON. */
  outgoing(): string[] {
    return this.#holder.outgoing();
  }

  /**
   * The payments this purse made and keeps: each while one of its transfers
   * is outgoing, or while it keeps notes owed to the payee.
   */
  payments(): MadePayment[] {
    return this.#holder.payments().map((made) => {
      const read = {
        request: PaymentRequest.from(made.request),
        payment: Payment.from(made.payment),
        owed: JSON.parse(made.owed) as string[],
      };
      made.free();
      return read;
    });
  }

  /** Value this purse has at `mint`, in parts that do not overlap, from its info. */
  async balance(mint: string): Promise<Balance> {
    const info = await this.client.infoText(mint);
    const balance = this.#holder.balance(info);
    try {
      return {
        total: balance.total,
        held: balance.held,
        spendable: balance.spendable,
        frozen: balance.frozen,
        expiring: balance.expiring,
        unchecked: balance.unchecked,
        unknown: balance.unknown,
        sending: balance.sending,
        change: balance.change,
        refreshing: balance.refreshing,
        requested: balance.requested,
        unconfirmed: balance.unconfirmed,
        outgoing: balance.outgoing,
        expected: balance.expected,
      };
    } finally {
      balance.free();
    }
  }

  /**
   * The notes held at `mint`, in the order {@link notes} lists them, each
   * with its value, its layer, what it can do now, when it can move and when
   * it dies unless moved. Like {@link balance}, it reads the mint's info and
   * nothing else: the mint learns nothing about any one note, and what it
   * reports is what this purse holds already. {@link Client.verify}, by
   * contrast, shows the mint each note it is asked about, so asking about a
   * purse's notes one after another invites the guess that they have one
   * owner.
   */
  async notesAt(mint: string): Promise<HeldNote[]> {
    const info = await this.client.infoText(mint);
    return this.#holder.notesAt(info).map((note) => {
      const read = {
        serial: { generation: note.generation, index: note.index },
        amount: note.amount,
        state: note.state,
        position: note.position,
        spendableAt: note.spendableAt,
        expiresAt: note.expiresAt,
      };
      note.free();
      return read;
    });
  }

  /**
   * When `mint` first takes a note of this purse's unless it moves first:
   * the Unix time, in seconds by the mint's clock, of the scheduled rotation
   * that drops a note it holds, or the note one of its outgoing transfers
   * spends, whichever comes first. A transfer whose note the last rotation
   * dropped counts at the next, which drops the notes it made if the mint
   * applied it. `undefined` when the mint has no schedule or is paused, or
   * when none of these is left. It reads the mint's info and nothing else.
   *
   * An application that keeps notes alive only while it runs, with
   * {@link refresh}, should run again before it, less a margin of its own:
   * the date to come back by. Once it has passed, read it again, since the
   * mint may not have made that rotation yet.
   */
  async firstExpiry(mint: string): Promise<bigint | undefined> {
    return this.#holder.firstExpiry(await this.client.infoText(mint));
  }

  /**
   * Asks for `amount` from `mint`: a payment request with a fresh key per
   * note, saved before it is returned. Send it to the payer.
   */
  async request(mint: string, amount: bigint): Promise<PaymentRequest> {
    const info = await this.client.infoText(mint);
    const json = await this.#change(
      () => this.#holder.request(info, amount),
      (json) => this.#holder.withdraw(json),
    );
    return PaymentRequest.from(json);
  }

  /** Like {@link request}, in notes of at most `largest`, for a payer whose notes are small. */
  async requestAtMost(mint: string, amount: bigint, largest: bigint): Promise<PaymentRequest> {
    const info = await this.client.infoText(mint);
    const json = await this.#change(
      () => this.#holder.requestAtMost(info, amount, largest),
      (json) => this.#holder.withdraw(json),
    );
    return PaymentRequest.from(json);
  }

  /**
   * Asks a playground faucet for a note and keeps it. The faucet's mint must
   * be on this purse's server; the faucet may share its URL. The note goes to
   * a fresh key, saved before the faucet is asked, as for any payment
   * request. Resolves to the faucet's mint and the value kept. A faucet whose
   * notes its mint cannot make as one note is refused with `faucet_amount`.
   */
  async takeFromFaucet(faucet: Client): Promise<{ mint: string; value: bigint }> {
    const about = await faucet.faucet();
    const request = await this.requestAtMost(about.mint, about.amount, about.amount);
    const [output, ...rest] = request.notes;
    if (output === undefined || rest.length > 0) {
      await this.withdraw(request);
      throw worglError(
        'faucet_amount',
        { amount: about.amount },
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
   * spent, the keys for the change and the payment itself (see
   * {@link payments}) are saved before the payment is returned. Send it to
   * the payee, who submits it and can hand back the change for
   * {@link accept}; or {@link settle} later to take it in. A request for a
   * note the mint would not make, of an amount outside its denominations, or
   * to a key this purse has paid already, such as the same request twice, is
   * refused before anything is signed. Refusals are {@link WorglError}s,
   * such as `unaffordable` when too little can be spent now, or `fragmented`
   * when enough can but in notes too small for those asked for, with the
   * largest notes a request for the amount may ask for in its details.
   */
  async pay(request: PaymentRequest, spacing: Spacing = DEFAULT_SPACING): Promise<Payment> {
    const info = await this.client.infoText(request.mint);
    const json = await this.#changeOrRestore(() =>
      this.#holder.pay(info, request.json, BigInt(spacing.minSecs), BigInt(spacing.maxSecs)),
    );
    return Payment.from(json);
  }

  /**
   * Takes back the note of an outgoing transfer the mint never applied,
   * keeping the keys of its change in case it did, and saves. Resolves to
   * whether the transfer was outgoing.
   *
   * A payee that holds the transfer can still submit it: if the mint applies
   * it, or already had, the payee's notes and the note taken back claim the
   * same value, and the mint refuses whichever is spent second.
   */
  async cancel(transfer: string): Promise<boolean> {
    return this.#changeOrRestore(() => this.#holder.cancel(transfer));
  }

  /**
   * Drops an outgoing transfer whose note is gone, keeping the keys of its
   * change so that change handed over later is still taken in, and saves.
   * Resolves to whether the transfer was outgoing.
   */
  async forget(transfer: string): Promise<boolean> {
    return this.#changeOrRestore(() => this.#holder.forget(transfer));
  }

  /**
   * Stops keeping notes owed to payees, once they are handed over, and
   * saves. The notes of a payment are in {@link payments}: send them where
   * the payee gets them, then forget them. Resolves to the value it stopped
   * keeping.
   */
  async forgetOwed(notes: string[]): Promise<bigint> {
    return this.#changeOrRestore(() => this.#holder.forgetOwed(JSON.stringify(notes)));
  }

  /**
   * Takes a payment for `request`: checks that it pays what was asked, then
   * submits each transfer at its time and takes in the notes. Resolves once
   * the last transfer is answered or refused, or once `options.signal`
   * aborts (see {@link submit}). The payer chose the times. Hand
   * {@link Submitted.theirs}, the payer's change, back to the payer.
   */
  async receive(
    request: PaymentRequest,
    payment: Payment,
    options: SubmitOptions = {},
  ): Promise<Submitted> {
    checkPayment(request.json, payment.json);
    return this.submit(payment.transfers, options);
  }

  /**
   * Takes in notes the counterparty handed over from the mint's answers,
   * after checking each one's signature against its mint's keys, and
   * settles the outgoing transfers they answer: those that pay this purse
   * notes, once it holds them all. Notes for keys this purse never handed
   * out are ignored. A note whose layer the mint has dropped no longer
   * verifies and rejects the call. Returns the value taken in.
   *
   * The signatures show that the mint issued the notes, not that it applied
   * the transfers they settle: a counterparty holding a transfer can pay the
   * same keys from notes of its own and leave the transfer unsubmitted. A
   * hand-back settles a transfer only as surely as the counterparty
   * submitted it.
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
      () => this.#holder.takeAnswers(JSON.stringify(notes)),
      () => {},
    );
  }

  /**
   * Resends the transfers this purse made and has not seen answered, at
   * random gaps, and takes in their change. Call it once a payee has had
   * time to submit a payment, or after a restart. With a store that has a
   * lock, the transfers are those of the holder saved now, so they include
   * any another purse on the store made. The answer to a payment's
   * transfer keeps the payee's notes for it, in case the payee has not
   * submitted the payment and the resend carried it out (see
   * {@link MadePayment.owed}), and a transfer refused for good is dropped or
   * cancelled (see {@link Refused.outcome}).
   *
   * The mint answers a resent transfer with the same notes only while it
   * keeps its answer, and at the latest until the notes it issued expire
   * (see
   * {@link https://github.com/zhangheqi/worgl/blob/main/site/src/content/docs/docs/error-codes.md#sending-a-request-again | Error codes}).
   * After that, a transfer already applied is refused as `spent` or
   * `expired`, with {@link Refused.atStake} set, and its change can only
   * come from the counterparty, for {@link accept}. {@link refresh} resends
   * the transfers whose chance the next rotation ends.
   *
   * `options` follow it and cut it short as they do {@link submit}.
   */
  async settle(
    spacing: Spacing = DEFAULT_SPACING,
    options: SubmitOptions = {},
  ): Promise<Submitted> {
    const transfers = await this.#current(async () =>
      read(this.#holder.resend(BigInt(spacing.minSecs), BigInt(spacing.maxSecs))),
    );
    return this.submit(transfers, options);
  }

  /**
   * Plans the refresh of `mint`'s notes that die at the next rotation, after
   * reading its info, and saves the plan. The outgoing transfers to resend
   * before that rotation join it; if the save fails, only the refreshes are
   * undone. Submit it with {@link submit}.
   *
   * The times are planned by the server's clock, from the `Date` of its info
   * answer ({@link Client.infoTextAndTime}), so this machine's clock, slow
   * or fast, moves none of them past the rotation. When that `Date` cannot
   * be read, as in a browser calling a mint on another origin that does not
   * expose it, they are planned by this machine's clock instead. Once the
   * rotation's time has come and the mint has not made it yet, they are all
   * due at once.
   */
  async planRefresh(mint: string): Promise<ScheduledTransfer[]> {
    return this.#planRefresh(await this.client.infoTextAndTime(mint));
  }

  /**
   * Refreshes `mint`'s notes that die at the next rotation, each at a random
   * moment before it, and resends the outgoing transfers that must land
   * before it. Resolves once the last is done, which may be most of a
   * rotation period away, or once `options.signal` aborts (see
   * {@link submit}). The refreshes it did not send then stay outgoing, for
   * {@link settle} or the next refresh. Once the signal has aborted before
   * the mint's info is read, it plans none.
   */
  async refresh(mint: string, options: SubmitOptions = {}): Promise<Submitted> {
    const { signal } = options;
    let answer: { info: string; time: bigint } | undefined;
    if (!signal?.aborted) {
      try {
        answer = await this.client.infoTextAndTime(mint, requestOptions(signal));
      } catch (error) {
        // Stopped before anything was planned, it has nothing to submit.
        if (!stoppedBy(signal, error)) {
          throw error;
        }
      }
    }
    const plan = answer === undefined || signal?.aborted ? [] : await this.#planRefresh(answer);
    return this.submit(plan, options);
  }

  /** {@link planRefresh}, from a mint's info and the time it was read. */
  async #planRefresh({ info, time }: { info: string; time: bigint }): Promise<ScheduledTransfer[]> {
    const { refreshes, resends } = await this.#changeOrRestore(() => {
      // Before the refreshes join the outgoing transfers.
      const resends = read(this.#holder.dueResends(info, time));
      return { refreshes: read(this.#holder.refresh(info, time)), resends };
    });
    return [...refreshes, ...resends].sort((a, b) => a.afterSecs - b.afterSecs);
  }

  /**
   * Drops `mint`'s expired notes and returns the value lost. It also stops
   * waiting for unconfirmed change that would sit in a dropped layer, and
   * stops keeping notes owed to payees once their layer is dropped. The info
   * alone decides, not this machine's clock: a note goes, with its key, only
   * once the mint has dropped its layer.
   */
  async prune(mint: string): Promise<bigint> {
    const info = await this.client.infoText(mint);
    return this.#change(
      () => this.#holder.prune(info),
      () => {},
    );
  }

  /**
   * Submits each transfer at its time, counted from now, retrying failures
   * on the way, and records each answer, keeping the payee's notes for it
   * when the transfer is part of a payment this purse made. A payer that
   * submits its own payment hands them to the payee from
   * {@link Submitted.theirs}, then calls {@link forgetOwed} with them.
   *
   * Each answer is checked first, against the keys of the transfer's mint,
   * read after it: it must hold one note for each of the transfer's outputs,
   * that output's, signed by the mint. The purse keeps none of an answer that
   * is not the mint's, such as one from a server that is not the mint's, nor
   * of one it cannot read the keys to check.
   *
   * With a store that has a lock, what the mint answered is kept even when
   * its save is refused with `store_changed`, as when another purse takes
   * the lock meanwhile: the purse makes the change again on what is saved
   * now, since a resend gets an answer back only while the mint keeps it.
   *
   * A refused transfer does not stop the others; it is listed in
   * {@link Submitted.refused}, as is one whose answer the purse did not keep.
   * One this purse made that the mint refused for good is dropped or
   * cancelled ({@link Refused.outcome}); the rest stay outgoing. To confirm a
   * `policy` refusal, the purse reads the mint's info again. Rejects only if
   * the store fails: a save, or the read of a store with a lock before a
   * change.
   *
   * With `options`, a screen follows each transfer as it waits, goes and is
   * answered, can cut the waits short, and can stop the call, as when the
   * person leaves the page: see {@link SubmitOptions}. Stopped, it resolves
   * with each transfer answered and kept, or listed in
   * {@link Submitted.refused} or {@link Submitted.unsent}.
   */
  async submit(transfers: ScheduledTransfer[], options: SubmitOptions = {}): Promise<Submitted> {
    const { onProgress, sendNow, signal } = options;
    const order = [...transfers].sort((a, b) => a.afterSecs - b.afterSecs);
    const start = Date.now();
    const submitted: Submitted = {
      value: 0n,
      answered: 0,
      notes: [],
      theirs: [],
      refused: [],
      unsent: [],
    };
    const report = (
      step: SubmitProgress['step'],
      index: number,
      { sendAt, refusal }: { sendAt?: number; refusal?: Refused } = {},
    ): void => {
      if (onProgress === undefined) {
        return;
      }
      const progress: SubmitProgress = {
        step,
        transfer: order[index]!,
        index,
        total: order.length,
        answered: submitted.answered,
        refused: submitted.refused.length,
        sendAt,
        refusal,
      };
      try {
        onProgress(progress);
      } catch (error) {
        // Reported as an event listener's would be, without stopping here.
        queueMicrotask(() => {
          throw error;
        });
      }
    };
    const request = requestOptions(signal);
    for (const [index, transfer] of order.entries()) {
      const sendAt = start + Math.min(transfer.afterSecs, MAX_DELAY_SECS) * 1000;
      if (!signal?.aborted && !sendNow?.aborted && sendAt > Date.now()) {
        report('waiting', index, { sendAt });
        await sleepUntil(sendAt, signal, sendNow);
      }
      if (signal?.aborted) {
        submitted.unsent.push(transfer);
        report('unsent', index);
        continue;
      }
      report('sending', index);
      let notes: string[];
      try {
        notes = await this.#retrying(() => this.client.transfer(transfer.request, request), signal);
      } catch (error) {
        // Stopped on its way, it is as a transfer whose answer was lost: the
        // mint may have applied it, so it stays outgoing.
        const { atStake, outcome } = stoppedBy(signal, error)
          ? { atStake: undefined, outcome: 'kept' as const }
          : await this.#refused(transfer.request, error, signal);
        const refusal: Refused = { request: transfer.request, error, atStake, outcome };
        submitted.refused.push(refusal);
        report('refused', index, { refusal });
        continue;
      }
      // The answer is in hand, so it is checked and kept even once the
      // signal has aborted: a resend gets it again only while the mint
      // keeps it.
      const taken = await this.#takeAnswer(transfer.request, notes);
      if ('error' in taken) {
        // Nothing changed: a transfer this purse made stays outgoing, for the
        // next settle.
        const refusal: Refused = {
          request: transfer.request,
          error: taken.error,
          atStake: undefined,
          outcome: 'kept',
        };
        submitted.refused.push(refusal);
        report('refused', index, { refusal });
        continue;
      }
      submitted.value += taken.value;
      submitted.answered += 1;
      submitted.notes.push(...notes);
      submitted.theirs.push(...taken.theirs);
      report('answered', index);
    }
    return submitted;
  }

  /**
   * Records the answer to `transfer` once it checks out against the keys of
   * the transfer's mint, and resolves to the value taken in and the
   * counterparty's notes, or to why the answer was not kept: it is not the
   * mint's, or the keys could not be read. Rejects only if the store fails.
   */
  async #takeAnswer(
    transfer: string,
    notes: string[],
  ): Promise<{ value: bigint; theirs: string[] } | { error: unknown }> {
    let keys: string;
    try {
      const mint = transferMint(transfer);
      // Read after the answer, so that they know the generation its notes sit
      // in, which a rotation may have opened since an earlier read.
      keys = await this.#retrying(() => this.client.keysText(mint));
    } catch (error) {
      return { error };
    }
    const json = JSON.stringify(notes);
    // Set while the answer is checked: a rejection then is the answer's, and
    // any other, before or after, is the store's.
    let checking = false;
    const take = (): { value: bigint; theirs: string[] } => {
      checking = true;
      const value = this.#holder.settle(keys, transfer, json);
      checking = false;
      return { value, theirs: JSON.parse(this.#holder.theirs(json)) as string[] };
    };
    try {
      // Without a lock, an answer whose save fails stays in the holder, for
      // the next save to keep. With one, the next change reads the store
      // again, so the answer is taken out as any change is.
      return await this.#untilSaved(() =>
        this.store.lock === undefined ? this.#change(take, () => {}) : this.#changeOrRestore(take),
      );
    } catch (error) {
      if (!checking) {
        throw error;
      }
      return { error };
    }
  }

  /**
   * Drops or cancels a transfer the mint refused, when the refusal shows it
   * can never be applied. A transfer that is not outgoing is left as it is,
   * and so is one whose refusal could be confirmed only after `signal`
   * aborted.
   */
  async #refused(
    transfer: string,
    error: unknown,
    signal: AbortSignal | undefined,
  ): Promise<{ atStake: bigint | undefined; outcome: Outcome }> {
    const code = error instanceof MintError ? error.code : undefined;
    if (code === 'spent' || code === 'expired') {
      // The mint answers both with 409 Conflict. Any other answer, such as a
      // gateway's, shows nothing about the note.
      if (!(error instanceof MintError && error.status === 409)) {
        return { atStake: this.#holder.atStake(transfer), outcome: 'kept' };
      }
      return this.#untilSaved(() =>
        this.#changeOrRestore(() => {
          // Before the transfer is dropped.
          const atStake = this.#holder.atStake(transfer);
          const outcome: Outcome = this.#holder.forget(transfer) ? 'dropped' : 'kept';
          return { atStake, outcome };
        }),
      );
    }
    let cancel: boolean;
    if (code === 'layer_full') {
      // The mint answers so with 503, after finding every note spent live.
      // Any other answer shows nothing about the notes.
      cancel = error instanceof MintError && error.status === 503;
    } else if (code === 'policy') {
      // The mint checks its policy before its receipts, so a transfer applied
      // under a looser policy is refused too: its change keys stay. Cancel
      // only if the policy published now agrees.
      try {
        const info = await this.client.infoText(transferMint(transfer), requestOptions(signal));
        cancel = breaksPolicy(info, transfer);
      } catch {
        cancel = false;
      }
    } else {
      // Anything else may pass later or shows nothing about the transfer:
      // `unknown_mint` or `unissued` from a server that is not the mint's or
      // lost its state, or `unauthorized` or `duplicate_input`, which the
      // mint never answers to a transfer a holder signed, so they come from
      // another server or version.
      return { atStake: undefined, outcome: 'kept' };
    }
    if (!cancel) {
      return { atStake: undefined, outcome: 'kept' };
    }
    const cancelled = await this.#untilSaved(() =>
      this.#changeOrRestore(() => this.#holder.cancel(transfer)),
    );
    return { atStake: undefined, outcome: cancelled ? 'cancelled' : 'kept' };
  }

  /**
   * Seals this purse into a backup under `passphrase`, in turn after the
   * changes before it, naming this purse's server for each of its mints,
   * with the purses of other servers and the mint list `options` names. The
   * backup is the purse file of the `worgl` command line, which opens it as
   * a purse; {@link Backup.open} opens it here, and {@link restore} merges
   * it into a purse. With a store that has a lock, each purse is sealed as
   * saved now, read under its store's lock, so the backup holds what other
   * purses on the store saved, and no lock is held while the key is derived.
   *
   * It holds the secret key of every note: anyone with the backup and the
   * passphrase can spend them. Its key is derived from the passphrase with
   * Argon2id over 64 MiB, which takes a moment and blocks the thread
   * meanwhile, and a fresh salt each time. An empty passphrase is refused
   * with the {@link WorglError} `empty_passphrase`, and a mint two of the
   * purses hold with a `TypeError`.
   */
  async backup(passphrase: string, options: BackupOptions = {}): Promise<string> {
    const purses = [...new Set([this, ...(options.purses ?? [])])];
    const parts = await Promise.all(
      purses.map(async (purse) => ({
        server: purse.client.baseUrl,
        json: await purse.#snapshot(),
      })),
    );
    const holder = new Holder();
    try {
      const held: { server: string; mints: string[] }[] = [];
      for (const { server, json } of parts) {
        const part = Holder.fromJson(json);
        try {
          held.push({ server, mints: part.mints() });
          holder.join(part);
        } finally {
          part.free();
        }
      }
      return seal(holder, passphrase, mintList(held, options.mints ?? []));
    } finally {
      holder.free();
    }
  }

  /**
   * Merges `backup`, a copy of this purse such as an older backup or
   * another device's, into it and saves. What is the purse's in either is
   * kept, and nothing is counted twice that the two can tell apart: see
   * {@link Restored} for what only the mint can tell. A backup of another
   * purse merges as the union of the two.
   *
   * It takes the part of the backup that is of `mints`: by default every
   * mint it names this purse's server for, or no server, as in a purse file
   * of the `worgl` command line. The others are listed in
   * {@link Restored.others}. The mint is asked nothing.
   */
  async restore(backup: Backup, mints?: string[]): Promise<Restored> {
    const here = this.client.baseUrl;
    const taken =
      mints ??
      backup.mints
        .filter((m) => m.server === undefined || m.server === here)
        .map((m) => m.mint);
    const whole = Holder.fromJson(backup.toJson());
    let part: Holder;
    try {
      part = whole.only(JSON.stringify(taken));
    } finally {
      whole.free();
    }
    try {
      const restored = await this.#changeOrRestore(() => {
        const merged = this.#holder.merge(part);
        try {
          return {
            notes: JSON.parse(merged.notes) as string[],
            spent: JSON.parse(merged.spent) as string[],
            outgoing: merged.outgoing,
            ended: merged.ended,
            conflicts: merged.conflicts,
          };
        } finally {
          merged.free();
        }
      });
      return { ...restored, others: backup.mints.filter((m) => !taken.includes(m.mint)) };
    } finally {
      part.free();
    }
  }

  /**
   * Asks the mint about each of `notes`, by default the {@link unchecked}
   * ones, spaced by random gaps of `spacing`, then saves. Those it reports
   * spent or expired are discarded; those it reports live or unissued are
   * kept, and no longer unchecked. An unissued note may come back, as after
   * the mint restored its state from a backup. A note this purse does not
   * hold is only reported.
   *
   * The mint's answers are not signed, so ask only this purse's server, the
   * mint's own. Each question shows the mint one note, from this machine's
   * address and at its time, so asking about notes one after another
   * invites the guess that they have one owner. Ask only when that is
   * acceptable, with gaps as wide as you can wait for; or ask nothing, and
   * let the transfers that next spend the notes find the spent ones, which
   * the mint refuses. {@link prune} drops the expired ones without asking.
   */
  async checkNotes(
    notes: string[] = this.unchecked(),
    spacing: Spacing = DEFAULT_SPACING,
  ): Promise<Checked> {
    const checked: Checked = { value: 0n, discarded: [], kept: [], unanswered: [] };
    for (const [index, note] of notes.entries()) {
      if (index > 0) {
        await sleep(gap(spacing) * 1000);
      }
      let state: NoteState;
      try {
        state = await this.#retrying(() => this.client.verify(note));
      } catch (error) {
        checked.unanswered.push({ note, error });
        continue;
      }
      if (state.state === 'spent' || state.state === 'expired') {
        checked.discarded.push(note);
      } else {
        checked.kept.push(note);
      }
    }
    if (checked.discarded.length > 0 || checked.kept.length > 0) {
      const gone = JSON.stringify(checked.discarded);
      const kept = JSON.stringify(checked.kept);
      checked.value = await this.#changeOrRestore(() => {
        this.#holder.keep(kept);
        return this.#holder.discard(gone);
      });
    }
    return checked;
  }

  /**
   * Makes the request `call` makes, again while the failure is transient, as
   * {@link Retry} says. Once `signal` aborts, a wait between tries ends and
   * no try follows: a transient failure then rejects with the signal's
   * reason, as does `call` when the signal stopped it, while a refusal
   * rejects as itself.
   */
  async #retrying<T>(call: () => Promise<T>, signal?: AbortSignal): Promise<T> {
    let delay = this.#retry.firstDelayMs;
    for (let attempt = 1; ; attempt += 1) {
      try {
        return await call();
      } catch (error) {
        if (!isTransient(error)) {
          throw error;
        }
        signal?.throwIfAborted();
        if (attempt >= this.#retry.attempts) {
          throw error;
        }
        await sleep(nextWait(delay, error), signal);
        signal?.throwIfAborted();
        delay *= 2;
      }
    }
  }

  /**
   * Changes the holder as saved now and saves it, after any change before
   * it, holding the store's lock throughout if it has one. If the save
   * fails, `undo` takes the change back and the call rejects.
   */
  #change<T>(apply: () => T, undo: (value: T) => void): Promise<T> {
    return this.#current(async () => {
      const value = apply();
      try {
        await this.store.save(this.#holder.toJson());
      } catch (error) {
        undo(value);
        throw error;
      }
      return value;
    });
  }

  /**
   * Runs `use` after any change before it. With a store that has a lock, it
   * runs holding it, on the holder saved now: read again if it is not this
   * purse's, as when another purse saved since this one last read or saved.
   */
  #current<T>(use: () => Promise<T>): Promise<T> {
    const run = (): Promise<T> => {
      if (this.store.lock === undefined) {
        return use();
      }
      return this.store.lock(async () => {
        const saved = await this.store.load();
        // Read in only if it differs: writing a holder out to compare takes
        // a small part of the time reading one in does, and leaves no copy
        // of its keys behind.
        if (saved !== undefined && saved !== this.#holder.toJson()) {
          const holder = Holder.fromJson(saved);
          this.#holder.free();
          this.#holder = holder;
        }
        return use();
      });
    };
    const result = this.#changes.then(run, run);
    this.#changes = result.catch(() => {});
    return result;
  }

  /**
   * The holder as JSON, in turn after the changes before it: with a store
   * that has a lock, the holder saved now, read under the lock.
   */
  #snapshot(): Promise<string> {
    return this.#current(async () => this.#holder.toJson());
  }

  /** Like {@link #change}, but a failed save puts the holder back as it was before `apply`. */
  async #changeOrRestore<T>(apply: () => T): Promise<T> {
    let before = '';
    return this.#change(
      () => {
        before = this.#holder.toJson();
        return apply();
      },
      () => {
        const restored = Holder.fromJson(before);
        this.#holder.free();
        this.#holder = restored;
      },
    );
  }

  /**
   * Makes `change` again for as long as the store refuses its save with
   * `store_changed`, as when another purse takes the lock meanwhile or the
   * store is cleared, each time on the holder saved now. It is for a change
   * that keeps what the mint answered, which the refusal does not make
   * wrong and which would be lost with the call.
   */
  async #untilSaved<T>(change: () => Promise<T>): Promise<T> {
    for (;;) {
      try {
        return await change();
      } catch (error) {
        if (!(isWorglError(error) && error.code === 'store_changed')) {
          throw error;
        }
      }
    }
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

/** The options that send a request with `signal`, if there is one. */
function requestOptions(signal: AbortSignal | undefined): RequestOptions {
  return signal === undefined ? {} : { signal };
}

/** Whether `error` is the reason `signal` aborted with: a call it stopped. */
function stoppedBy(signal: AbortSignal | undefined, error: unknown): boolean {
  return signal?.aborted === true && error === signal.reason;
}

/** Waits `ms`, or until one of `signals` aborts if that comes first. */
function sleep(ms: number, ...signals: (AbortSignal | undefined)[]): Promise<void> {
  const watched = signals.filter((signal) => signal !== undefined);
  return new Promise((resolve) => {
    if (watched.some((signal) => signal.aborted)) {
      resolve();
      return;
    }
    const done = (): void => {
      clearTimeout(timer);
      for (const signal of watched) {
        signal.removeEventListener('abort', done);
      }
      resolve();
    };
    const timer = setTimeout(done, ms);
    for (const signal of watched) {
      signal.addEventListener('abort', done);
    }
  });
}

/** A gap drawn at random from `spacing`, in seconds. */
function gap(spacing: Spacing): number {
  const [drawn = 0] = crypto.getRandomValues(new Uint32Array(1));
  return spacing.minSecs + (drawn / 2 ** 32) * Math.max(0, spacing.maxSecs - spacing.minSecs);
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

/**
 * Waits until `deadline`, in milliseconds since the epoch, in steps that
 * timers can hold, or until one of `signals` aborts if that comes first.
 */
async function sleepUntil(
  deadline: number,
  ...signals: (AbortSignal | undefined)[]
): Promise<void> {
  for (
    let left = deadline - Date.now();
    left > 0 && !signals.some((signal) => signal?.aborted);
    left = deadline - Date.now()
  ) {
    await sleep(Math.min(left, 2 ** 31 - 1), ...signals);
  }
}
