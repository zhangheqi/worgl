// What the purse's records hold: the mints added, the requests made and
// the log. Types only, apart from the database itself (records.ts), so
// the pure modules and their tests can name them without the DOM.

import type { LastFade } from './holdings.ts';

/** What the purse last read of a mint's info. */
export interface SavedInfo {
  layers: number;
  /** Seconds between rotations, if it rotates on its own. */
  periodSecs: number | undefined;
  paused: boolean;
  /** When the purse read it, by this device's clock. */
  readAt: number;
  /**
   * What fades first, and when, as the last read saw it: said when the mint
   * does not answer the next open. `null` when nothing does (no notes, or no
   * schedule); missing when no read of this version saved it.
   */
  fades?: LastFade | null;
}

/** A mint the person added. */
export interface MintRecord {
  /** Its issuer key, in hex: the record's key. */
  mint: string;
  /** The server it was added at: one SDK purse and store per server. */
  server: string;
  /** What the purse calls it: the build's playground, or the server's host until the person names it. */
  name: string;
  /** What its tokens are called. */
  unit: string;
  /** When it was added. */
  added: number;
  /** The info last read. */
  info: SavedInfo | undefined;
  /** When the person last used it: added, took from its faucet, asked, sent or took in. Holdings shows the latest. */
  used?: number;
}

/** A request this purse made: the SDK keeps its keys; this keeps what Receive shows and lists. */
export interface AskRecord {
  /** The request's id: its first note's one-time key, notes[0].holder (64 hex). */
  id: string;
  mint: string;
  server: string;
  /** Tokens asked for. */
  amount: number;
  /** The note amounts, in the request's order. */
  notes: number[];
  /** "In notes of at most", if asked so. */
  atMost: number | undefined;
  /** The request's JSON, verbatim: for its link, and for withdraw. */
  json: string;
  /** When it was made. */
  t: number;
  state: 'open' | 'taken' | 'cancelled';
  /** Tokens taken in for it so far. */
  got: number;
  /** When it was taken in full or cancelled. */
  endedAt: number | undefined;
}

/** One entry in the purse's log. */
export interface LogEntry {
  /** Its place in the log, given when it is added. */
  id?: number;
  /** When it happened. */
  t: number;
  /**
   * `faucet`: a note taken from the faucet; `faded`: notes a rotation
   * dropped; `kept`: notes moved a layer on by keep-alive (the amount moved;
   * the log shows no change); `lost`: a hop the mint applied whose answer
   * it no longer keeps; `sent`: a send to someone's request; `received`: a
   * hand-back taken in.
   */
  kind: 'faucet' | 'faded' | 'kept' | 'lost' | 'sent' | 'received';
  /** The mint. */
  mint: string;
  /**
   * Tokens it brought (faucet, received), moved on (kept) or took away
   * (faded, lost, sent: what went, or what was asked until that is known).
   */
  amount: number;
  /**
   * The serials of the notes it brought, `generation:index`, so a note can
   * say where it came from: for keep-alive, the notes it moved on; for a
   * send, its change.
   */
  serials: string[];
  /** kept: the mint's first layer (its generation) when they moved, so one rotation's runs make one entry. */
  round?: string;
  /** sent, received: the request's id. */
  ask?: string;
  /** sent: the server whose payments() keeps it. */
  server?: string;
  /** sent: what became of it. */
  state?: 'sending' | 'waiting' | 'handback' | 'partial' | 'turned' | 'lost' | 'done';
  /** sent: the request's amount. `amount` holds it too until what went is known. */
  asked?: number;
  /** sent: the value that went, once known; undefined after a cancelled transfer found by a later settle (SDK lacks 4). */
  went?: number;
  /** sent: transfers made. */
  transfers?: number;
  /** sent: serials of the notes spent. */
  spent?: string[];
  /** sent: change that came back (its notes' serials are in `serials`). */
  back?: number;
  /** sent, state lost: the value lost, the notes the dropped transfers spent: theirs and the change together. */
  lost?: number;
  /** sent, state lost: the part of `lost` that was for the receiver. */
  lostTheirs?: number;
  /** sent, state turned: whether the mint turned it away because its newest layer was full (it can go after the rotation). */
  layerFull?: boolean;
  /** sent: when it was last updated: when its change came. */
  t2?: number;
  /** sent: when each note of its change came, by serial, for the notes that came before `t2` (a send settled later). */
  changeAt?: Record<string, number>;
}
