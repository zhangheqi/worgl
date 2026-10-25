// Everything the purse asks of the SDK. Loaded with a dynamic import once
// the shell has painted, since importing the SDK fetches and compiles its
// wasm first.
//
// One SDK Purse and one IndexedDbStore per server, the store named after
// the server's URL as the SDK's Client writes it, so every mint on that
// server goes through that one purse. The store is made once; after
// another tab changed it, a new Purse is opened on it (see current.ts).
//
// What sends transfers to a mint (a send, settling what a send kept for
// later, and a keep-alive pass) holds the purse's own Web Lock,
// `purse:transfers` (lock.ts), so two tabs never submit the same transfers
// at once. The SDK store's lock (`worgl:<server>`) is held only for each
// save, so no change waits for the purse's.

import {
  Client,
  type FaucetInfo,
  type HeldNote,
  type MintInfo,
  PaymentRequest,
  Purse,
  type Refused,
  type RequestOptions,
  type ScheduledTransfer,
  type Spacing,
  type SubmitOptions,
  type SubmitProgress,
  type Submitted,
  isTransient,
} from 'worgl';
import { IndexedDbStore } from 'worgl/browser';

import { Current } from './current.ts';
import { noteId } from './holdings.ts';
import { type Moving, answersOf, movingNotes, spentBy, spentKey } from './keep.ts';
import type { KeepPurse } from './keeper.ts';
import type { MintLink } from './link.ts';
import { withTransfersLock } from './lock.ts';
import { holdersOf, readNote, sentTo, serialsSpent, valueOf } from './notes.ts';
import { type Policy, policyFrom } from './plan.ts';

export type { Moving } from './keep.ts';

/** Info requests on their way, by URL. */
const asked = new Map<string, Promise<Response>>();

/**
 * `fetch`, with one request for a mint's info however many ask for it at
 * once. Holdings reads the notes, the balance, the first expiry and the
 * schedule together, and each of those SDK calls GETs the info: shared,
 * they all see the same rotation, and the server sees one request.
 */
const sharedFetch: typeof fetch = (input, init) => {
  const url = input instanceof Request ? input.url : String(input);
  if ((init?.method ?? 'GET') !== 'GET' || !url.endsWith('/info')) {
    return fetch(input, init);
  }
  let answer = asked.get(url);
  if (answer === undefined) {
    answer = fetch(input, init);
    asked.set(url, answer);
    const done = (): void => {
      asked.delete(url);
    };
    answer.then(done, done);
  }
  return answer.then((response) => response.clone());
};

/** A server as the purse keeps it: its client, its one store, and its current SDK purse. */
interface Server {
  client: Client;
  store: IndexedDbStore;
  purse: Current<Purse>;
}

/** Each server the purse has opened, by its URL. */
const servers = new Map<string, Server>();

/** The server at `url`, made on first use. Throws where the browser offers no IndexedDB or Web Locks. */
function serverAt(url: string): Server {
  const known = servers.get(url);
  if (known !== undefined) {
    return known;
  }
  const client = new Client(url, { fetch: sharedFetch });
  const store = new IndexedDbStore(client.baseUrl);
  const server: Server = { client, store, purse: new Current(() => Purse.open(client, { store })) };
  servers.set(url, server);
  return server;
}

/** The current SDK purse for `server`, opened from its store on first use. */
async function purseFor(server: string): Promise<Purse> {
  return serverAt(server).purse.get();
}

/**
 * Has each server's purse read its store again before its next use: after
 * another tab changed one, or a change lost the store's lock. An SDK purse
 * shows its holder as it last read or saved it.
 */
export function reopen(): void {
  for (const server of servers.values()) {
    server.purse.stale();
  }
}

/** A mint, as the purse shows it before adding it: its info, and its faucet if its server has one. */
export interface About {
  info: MintInfo;
  faucet: FaucetInfo | undefined;
}

/** The answer of a server that is not the mint's: thrown as a MintError would be, so it reads plainly. */
class NotListed extends Error {
  readonly status = 404;
  readonly code = 'unknown_mint';
  constructor() {
    super('the server does not list this mint');
    this.name = 'MintError';
  }
}

/**
 * Reads the mint a link names through the SDK: its info, which must name
 * the same key, and its keys, so that a wrong key or a server that does not
 * host it fails here, before anything is added. Then its faucet, if the
 * server answers `GET /v1/faucet` for this mint.
 */
export async function readMint(link: MintLink): Promise<About> {
  const client = new Client(link.server);
  const info = await client.info(link.mint);
  if (info.issuer !== link.mint) {
    throw new NotListed();
  }
  const keys = JSON.parse(await client.keysText(link.mint)) as unknown;
  if (typeof keys !== 'object' || keys === null) {
    throw new NotListed();
  }
  return { info, faucet: await faucetOf(client, link.mint) };
}

/** The faucet at the mint's server, if it answers and hands out this mint's notes. */
async function faucetOf(client: Client, mint: string): Promise<FaucetInfo | undefined> {
  try {
    const about = await client.faucet();
    return about.mint === mint ? about : undefined;
  } catch {
    // no faucet here, or not for this purse's origin: no Take button, and the next read asks again
    return undefined;
  }
}

/** The faucet at `server`, if it answers and hands out `mint`'s notes: asked apart from what the purse holds. */
export async function faucetAt(server: string, mint: string): Promise<FaucetInfo | undefined> {
  return faucetOf(serverAt(server).client, mint);
}

/** What the purse holds of one mint, as the SDK reports it. */
export interface Read {
  info: MintInfo;
  /** The mint's clock less this device's, in seconds, from the `Date` of its answer. */
  skew: number;
  notes: HeldNote[];
  total: bigint;
  firstExpiry: bigint | undefined;
  /** This mint's notes on their way in refresh transfers, which `notes` no longer lists and `total` still counts. */
  moving: Moving[];
}

/**
 * Reads what the purse holds at `mint` on `server`, by the mint's info and
 * clock: every part from one answer of the mint's (see `sharedFetch`).
 */
export async function read(server: string, mint: string): Promise<Read> {
  const purse = await purseFor(server);
  const sent = Date.now();
  const [clock, info, notes, balance, firstExpiry] = await Promise.all([
    purse.client.infoTextAndTime(mint).then(({ time }) => ({ time, back: Date.now() })),
    purse.client.info(mint),
    purse.notesAt(mint),
    purse.balance(mint),
    purse.firstExpiry(mint),
  ]);
  // the Date header counts whole seconds: a skew under two seconds is that, not a clock that is off
  const raw = Number(clock.time) - (sent + clock.back) / 2000;
  const skew = Math.abs(raw) < 2 ? 0 : Math.round(raw);
  const { outgoing, sendInputs } = outgoingOf(purse);
  return { info, skew, notes, total: balance.total, firstExpiry, moving: movingNotes(outgoing, sendInputs, mint) };
}

/** A purse's outgoing transfers, and the notes its sends (the SDK's payments) spend, by `spentKey`. */
function outgoingOf(purse: Purse): { outgoing: string[]; sendInputs: Set<string> } {
  const sendInputs = new Set<string>();
  for (const made of purse.payments()) {
    for (const transfer of made.payment.transfers) {
      for (const spent of spentBy(transfer.request)) {
        sendInputs.add(spentKey(spent));
      }
    }
  }
  return { outgoing: purse.outgoing(), sendInputs };
}

/**
 * The outgoing transfers of `server`'s purse, and the notes the purse's
 * payments spend: a transfer that spends one of those is a send's, not a
 * hop. Told apart by the note spent, never by the request's text, since
 * two JSON strings of one transfer need not match byte for byte.
 */
export async function outgoingAt(server: string): Promise<{ outgoing: string[]; sendInputs: Set<string> }> {
  return outgoingOf(await purseFor(server));
}

/**
 * Runs `work` on the server's current purse as one change (`Current.change`),
 * so the purse is not opened again in the middle: a settle, or a
 * `planRefresh` and the submit of its plan, respaced. A settle resends a
 * send's transfers as well as keep-alive's: `settled` hears what each of
 * the purse's payments came to, before the change is over.
 */
export async function keepServer<R>(
  server: string,
  work: (purse: KeepPurse) => Promise<R>,
  settled: (what: Settled) => Promise<void>,
): Promise<R> {
  return serverAt(server).purse.change((purse) =>
    work({
      planRefresh: (mint) => purse.planRefresh(mint),
      submit: (transfers, options) => purse.submit(transfers, options),
      settle: async (spacing, options, holdBack) => {
        const what = await settling(purse, holdBack.size === 0 ? 'all' : { holdBack }, spacing, options);
        await settled(what);
        return what.submitted;
      },
    }),
  );
}

/** Thrown when the faucet at a mint's server hands out another mint's notes by the time Take is pressed. */
class FaucetChanged extends Error {
  constructor() {
    super('the faucet hands out another mint\'s notes now');
    this.name = 'FaucetChanged';
  }
}

/**
 * The faucet at a server, for one mint: it refuses before anything is
 * asked for when the faucet names another mint, so a take never lands at a
 * mint the person did not choose.
 */
class FaucetFor extends Client {
  readonly #mint: string;

  constructor(server: string, mint: string) {
    super(server, { fetch: sharedFetch });
    this.#mint = mint;
  }

  override async faucet(options?: RequestOptions): Promise<FaucetInfo> {
    const about = await super.faucet(options);
    if (about.mint !== this.#mint) {
      throw new FaucetChanged();
    }
    return about;
  }
}

/**
 * Takes a note of `mint` from the faucet at its server and keeps it.
 * Resolves to its value and the serials of the notes that came, for the
 * log. Refused with a `FaucetChanged` when the faucet hands out another
 * mint's notes now.
 */
export async function take(server: string, mint: string): Promise<{ value: bigint; serials: string[] }> {
  return serverAt(server).purse.change(async (purse) => {
    const before = new Set((await purse.notesAt(mint)).map((note) => noteId(note.serial)));
    const { value } = await purse.takeFromFaucet(new FaucetFor(server, mint));
    // the take read the store, so notes another tab kept meanwhile are new here too: the faucet's is one note of `value`
    const fresh = (await purse.notesAt(mint)).filter((note) => !before.has(noteId(note.serial)));
    const taken = fresh.filter((note) => note.amount === value).slice(-1);
    return { value, serials: (taken.length > 0 ? taken : fresh).map((note) => noteId(note.serial)) };
  });
}

/**
 * Drops the notes whose layer the mint dropped, when there are any, and
 * resolves to the value lost and the generation of the newest of them.
 */
export async function prune(server: string, mint: string): Promise<{ lost: bigint; generation: bigint | undefined }> {
  return serverAt(server).purse.change(async (purse) => {
    const expired = (await purse.notesAt(mint)).filter((note) => note.state === 'expired');
    if (expired.length === 0) {
      return { lost: 0n, generation: undefined };
    }
    const lost = await purse.prune(mint);
    const generation = expired.reduce((top, note) => (note.serial.generation > top ? note.serial.generation : top), 0n);
    return { lost, generation };
  });
}

// ---------- Receive and Send ----------

/**
 * How far apart the transfers of one send go: a few seconds at random,
 * tighter than the SDK's default of 2 to 60 s, so a send stays quick.
 */
export const SEND_SPACING = { minSecs: 2, maxSecs: 6 } as const;

/** Whether an error may pass if tried again later: the SDK's own test. */
export const transient = (error: unknown): boolean => isTransient(error);

/** A note that came back from a send: its serial, value and layer, and when it can move (Unix seconds by the mint's clock). */
export interface Came {
  id: string;
  v: number;
  pos: number | undefined;
  spendableAt: bigint | undefined;
}

/**
 * The notes `notes` (as the mint answered them) as held at `mint`: where
 * each sits and when it can move, read from the mint's info. That read is
 * the only one, and it may fail after the answer is in: the notes are kept
 * all the same, so they come back placed nowhere (Holdings places them).
 */
async function placed(purse: Purse, mint: string, notes: readonly string[]): Promise<Came[]> {
  const read = notes.flatMap((note) => {
    const fields = readNote(note);
    return fields === undefined ? [] : [fields];
  });
  let held = new Map<string, HeldNote>();
  try {
    held = new Map((await purse.notesAt(mint)).map((note) => [noteId(note.serial), note]));
  } catch {
    // the answer is in and kept; only where its notes sit is not known here
  }
  return read.map((fields) => {
    const note = held.get(fields.id);
    return { id: fields.id, v: fields.amount, pos: note?.position, spendableAt: note?.spendableAt };
  });
}

/**
 * Runs `work` under the purse's lock (lock.ts): waiting for it, or, with
 * `ifAvailable`, resolving to `undefined` at once when it is held.
 */
const locked = <R>(work: () => Promise<R>, ifAvailable = false): Promise<R | undefined> => withTransfersLock(work, ifAvailable);

/** A request made: its JSON, its notes' amounts in its order, and its id (its first note's key). */
export interface Asked {
  json: string;
  notes: number[];
  id: string;
}

/** The id of a request: its first note's one-time key. */
const idOf = (request: PaymentRequest): string => request.notes[0]?.holder ?? '';

/**
 * Asks for `amount` of `mint`: a request with a fresh key for each note,
 * in notes of at most `atMost` when given. The SDK has saved its keys by
 * the time this resolves.
 */
export async function ask(server: string, mint: string, amount: number, atMost?: number): Promise<Asked> {
  return serverAt(server).purse.change(async (purse) => {
    const request =
      atMost === undefined
        ? await purse.request(mint, BigInt(amount))
        : await purse.requestAtMost(mint, BigInt(amount), BigInt(atMost));
    return { json: request.json, notes: request.notes.map((note) => Number(note.amount)), id: idOf(request) };
  });
}

/** Cancels a request: the SDK drops its keys, so tokens sent to it later are lost. */
export async function cancelAsk(server: string, json: string): Promise<void> {
  await serverAt(server).purse.change((purse) => purse.withdraw(PaymentRequest.from(json)));
}

/** A request as Send reads it, before anything is asked of a mint. */
export interface ReadRequest {
  mint: string;
  amount: number;
  /** The notes it asks for, in its order. */
  outputs: number[];
  id: string;
}

/** Reads a request's JSON, with no network and no store; throws when it is not one. */
export function readRequest(json: string): ReadRequest {
  const request = PaymentRequest.from(json);
  const id = idOf(request);
  if (request.notes.length === 0 || id === '') {
    throw new RangeError('a request asks for at least one note');
  }
  return { mint: request.mint, amount: Number(request.amount), outputs: request.notes.map((note) => Number(note.amount)), id };
}

/** A payment this purse made and keeps, as Send shows it. */
export interface Kept {
  /** The request's id. */
  id: string;
  mint: string;
  /** What the request asked for. */
  amount: number;
  /** The receiver's notes the purse keeps for them: the hand-back. */
  owed: string[];
  /** Whether a transfer of it is still on its way. */
  outgoing: boolean;
  /** Its transfers, as it keeps them now (one cancelled leaves it). */
  transfers: string[];
  /**
   * What those transfers pay the receiver, and this purse's change in them,
   * as they were signed: once none is outgoing, what went and what came
   * back. `undefined` when a transfer does not read.
   */
  went: number | undefined;
  back: number | undefined;
}

/** The payments `purse` keeps. */
function kept(purse: Purse): Kept[] {
  const out = new Set(purse.outgoing());
  return purse.payments().map((made) => {
    const transfers = made.payment.transfers.map((transfer) => transfer.request);
    const split = sentTo(transfers, holdersOf(made.request.json));
    return {
      id: idOf(made.request),
      mint: made.request.mint,
      amount: Number(made.request.amount),
      owed: made.owed,
      outgoing: made.payment.transfers.some((transfer) => out.has(transfer.request)),
      transfers,
      went: split?.theirs,
      back: split?.change,
    };
  });
}

/** What Send reads before it plans: what is held, the mint's policy, and the payments kept, by request. */
export interface ForSend {
  read: Read;
  policy: Policy;
  /** The layer the mint makes new notes in, when its info says. */
  mintLayer: number | undefined;
  layers: number;
  sends: Map<string, Kept>;
}

/** Reads what can go to a request for `mint`: the mint's info (for its policy) and what the purse holds. */
export async function readForSend(server: string, mint: string): Promise<ForSend> {
  const purse = await purseFor(server);
  const [info, held] = await Promise.all([purse.client.infoText(mint), read(server, mint)]);
  const { policy, mintLayer, layers } = policyFrom(JSON.parse(info) as unknown);
  return { read: held, policy, mintLayer, layers: layers || held.info.layers, sends: new Map(kept(purse).map((each) => [each.id, each])) };
}

/** A mint's policy as its info states it (the SDK's MintInfo leaves it out): for Receive's sizes of note. */
export async function policyAt(server: string, mint: string): Promise<{ policy: Policy; mintLayer: number | undefined; layers: number }> {
  const purse = await purseFor(server);
  return policyFrom(JSON.parse(await purse.client.infoText(mint)) as unknown);
}

/** The purse's own refusal to pay a request it has paid already: Send then shows that it did. */
export class AlreadySent extends Error {
  constructor() {
    super('this purse has sent to this request already');
    this.name = 'AlreadySent';
  }
}

/** What a send asks of the app on its way. */
export interface SendHooks {
  /** Under the lock, before anything is signed: plan again from what is held now. */
  check: (fresh: Read) => 'go' | 'changed' | 'short' | 'already';
  /** Once `pay` signed and saved: its transfers, their times and the notes spent. */
  signed: (made: { transfers: number; afterSecs: number[]; spent: string[] }) => Promise<void>;
  /** Each transfer's progress: called synchronously, so it must return fast. */
  progress: (progress: SubmitProgress) => void;
  /** Aborted by "Send the rest now". */
  sendNow: AbortSignal;
  /** Aborted when the person leaves the page. */
  signal: AbortSignal;
}

/**
 * What a send came to: the submit; the change that came back, its notes
 * and its value; and the value that went, which the receiver's notes in the
 * answers carry. Read from the answers themselves, never from the balance,
 * which a take in another tab or a rotation may change meanwhile.
 */
export interface Sent {
  submitted: Submitted;
  back: Came[];
  backValue: number;
  went: number;
}

/**
 * Sends to the request `json`: under the lock and in one change, checks
 * again, pays, then submits each transfer at its time. Resolves to what it
 * came to, or to why it stopped before signing. Throws {@link AlreadySent}
 * when the purse keeps a payment to this request already.
 *
 * Once `pay` has run the request counts as paid, so nothing asks the mint
 * anything until `hooks.signed` has logged the send; and once `submit` has
 * the answers, nothing after it can throw for want of the mint.
 */
export async function send(server: string, json: string, hooks: SendHooks): Promise<Sent | { stopped: 'changed' | 'short' | 'already' }> {
  const done = await locked(() =>
    serverAt(server).purse.change(async (purse): Promise<Sent | { stopped: 'changed' | 'short' | 'already' }> => {
      const request = PaymentRequest.from(json);
      const mint = request.mint;
      if (kept(purse).some((each) => each.id === idOf(request))) {
        throw new AlreadySent();
      }
      const fresh = await read(server, mint);
      const verdict = hooks.check(fresh);
      if (verdict !== 'go') {
        return { stopped: verdict };
      }
      const made = await purse.pay(request, SEND_SPACING);
      // the notes it spends, from the transfers themselves: no read of the mint before the log has it
      const spent = serialsSpent(made.transfers.map((transfer) => transfer.request));
      await hooks.signed({
        transfers: made.transfers.length,
        afterSecs: made.transfers.map((transfer) => transfer.afterSecs),
        spent,
      });
      const submitted = await purse.submit(made.transfers, {
        onProgress: hooks.progress,
        sendNow: hooks.sendNow,
        signal: hooks.signal,
      });
      // what went is what the receiver's notes carry; failing that, what the answered transfers pay them
      const unanswered = new Set([...submitted.refused, ...submitted.unsent].map((transfer) => transfer.request));
      const answered = made.transfers.map((transfer) => transfer.request).filter((transfer) => !unanswered.has(transfer));
      const went = valueOf(submitted.theirs) ?? sentTo(answered, request.notes.map((note) => note.holder))?.theirs ?? 0;
      const theirs = new Set(submitted.theirs);
      const back = await placed(purse, mint, submitted.notes.filter((note) => !theirs.has(note)));
      return { submitted, back, backValue: Number(submitted.value), went };
    }),
  );
  if (done === undefined) {
    throw new Error('the lock was not granted');
  }
  return done;
}

/**
 * Takes in a hand-back's notes for `mint`: the value taken in and the
 * serials of its notes the purse holds now. Once `accept` has kept them,
 * nothing more asks the mint: the serials come from the notes themselves.
 */
export async function acceptHandBack(server: string, mint: string, notes: string[]): Promise<{ value: number; serials: string[] }> {
  return serverAt(server).purse.change(async (purse) => {
    const value = await purse.accept(notes);
    const held = new Set(purse.notes());
    return { value: Number(value), serials: notes.filter((note) => held.has(note)).flatMap((note) => readNote(note)?.id ?? []) };
  });
}

/**
 * The ids of every request `server`'s store keeps a payment to, hand-back
 * or not: read from the store itself, not the current purse, which another
 * tab's send may be newer than. A send gone from them went with its notes,
 * when their layer was dropped.
 */
export async function sendsKept(server: string): Promise<Set<string>> {
  const { client } = serverAt(server);
  // a store of its own, read once and never saved to: the current purse's store and its lock stay as they are
  const purse = await Purse.open(client, { store: new IndexedDbStore(client.baseUrl) });
  return new Set(purse.payments().map((made) => idOf(made.request)));
}

/** The payments of `server`'s purse that keep a hand-back: owed notes not yet handed over, or not yet answered. */
export async function handBacks(server: string): Promise<Kept[]> {
  return kept(await purseFor(server)).filter((each) => each.owed.length > 0 || each.outgoing);
}

/** What one payment came to in a settle. */
export interface SettledSend {
  id: string;
  mint: string;
  /** Still kept by the purse: outgoing, or with owed notes. */
  kept: Kept | undefined;
  /** Its transfers refused in this settle. */
  refused: Refused[];
  /** Its change that came in this settle: the notes of its own transfers' answers that are not the receiver's. */
  back: Came[];
}

/** What settling a server came to. */
export interface Settled {
  submitted: Submitted;
  /** Each payment that was kept before the settle. */
  sends: SettledSend[];
}

/** `requests` to submit: the first at once, then each a gap of `spacing` after the one before, at random. */
function spread(requests: readonly string[], spacing: Spacing): ScheduledTransfer[] {
  let t = 0;
  return requests.map((request, i) => {
    if (i > 0) {
      t += spacing.minSecs + Math.random() * (spacing.maxSecs - spacing.minSecs);
    }
    return { request, afterSecs: t };
  });
}

/**
 * Resends what `purse` sent and has not seen answered, `spacing` apart:
 * all of it (keep-alive's settle); all but the transfers that spend notes of
 * the mints `holdBack` names (keep-alive's settle while Send shows a plan
 * of one of them); or only its payments' transfers ("Try again now" on
 * Send, which leaves keep-alive's hops to keep-alive, which logs them).
 * Tells what each payment came to from the answers to its own transfers,
 * never from what else changed at the mint meanwhile, such as keep-alive's
 * hops answered in the same settle.
 */
async function settling(
  purse: Purse,
  which: 'all' | 'sends' | { holdBack: ReadonlySet<string> },
  spacing: Spacing,
  options: SubmitOptions,
): Promise<Settled> {
  const before = kept(purse);
  // each payment by the notes its transfers spend: told apart by the note, never by the request's text
  const byNote = new Map<string, string>();
  for (const made of purse.payments()) {
    for (const transfer of made.payment.transfers) {
      for (const spent of spentBy(transfer.request)) {
        byNote.set(spentKey(spent), idOf(made.request));
      }
    }
  }
  const paymentOf = (request: string): string | undefined =>
    spentBy(request)
      .map((spent) => byNote.get(spentKey(spent)))
      .find((id) => id !== undefined);
  // the transfers answered, in order: each answer holds one note per output
  const answered: string[] = [];
  const onProgress = (progress: SubmitProgress): void => {
    if (progress.step === 'answered') {
      answered.push(progress.transfer.request);
    }
    options.onProgress?.(progress);
  };
  const resent = (request: string): boolean =>
    which === 'sends' ? paymentOf(request) !== undefined : !spentBy(request).some((spent) => which !== 'all' && which.holdBack.has(spent.mint));
  const submitted =
    which === 'all'
      ? await purse.settle(spacing, { ...options, onProgress })
      : await purse.submit(spread(purse.outgoing().filter(resent), spacing), { ...options, onProgress });
  const theirs = new Set(submitted.theirs);
  const change = new Map<string, string[]>();
  for (const [i, notes] of answersOf(answered, submitted.notes).entries()) {
    const id = paymentOf(answered[i] ?? '');
    if (id !== undefined) {
      change.set(id, [...(change.get(id) ?? []), ...notes.filter((note) => !theirs.has(note))]);
    }
  }
  const now = new Map(kept(purse).map((each) => [each.id, each]));
  const sends = await Promise.all(
    before.map(async (each): Promise<SettledSend> => {
      const back = change.get(each.id);
      return {
        id: each.id,
        mint: each.mint,
        kept: now.get(each.id),
        refused: submitted.refused.filter((refusal) => paymentOf(refusal.request) === each.id),
        back: back === undefined ? [] : await placed(purse, each.mint, back),
      };
    }),
  );
  return { submitted, sends };
}

/**
 * "Try again now" on Send: resends the transfers of `server`'s payments that
 * are still outgoing, waiting for the lock; keep-alive's hops stay
 * keep-alive's. `undefined` when none was outgoing by then.
 */
export async function retrySettle(server: string, signal: AbortSignal): Promise<Settled | undefined> {
  return locked(() =>
    serverAt(server).purse.change((purse) =>
      kept(purse).some((each) => each.outgoing) ? settling(purse, 'sends', SEND_SPACING, { signal }) : Promise.resolve(undefined),
    ),
  ).then((settled) => settled ?? undefined);
}
