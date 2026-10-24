// Everything the purse asks of the SDK. Loaded with a dynamic import once
// the shell has painted, since importing the SDK fetches and compiles its
// wasm first.
//
// One SDK Purse and one IndexedDbStore per server, the store named after
// the server's URL as the SDK's Client writes it, so every mint on that
// server goes through that one purse. The store is made once; after
// another tab changed it, a new Purse is opened on it (see current.ts).
// The purse takes no lock of its own.

import { Client, type FaucetInfo, type HeldNote, type MintInfo, Purse, type RequestOptions } from 'worgl';
import { IndexedDbStore } from 'worgl/browser';

import { Current } from './current.ts';
import { noteId } from './holdings.ts';
import type { MintLink } from './link.ts';

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
  return { info, skew, notes, total: balance.total, firstExpiry };
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
