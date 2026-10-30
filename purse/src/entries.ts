// What the purse's records hold: the mints added, the requests made and
// the log. Types only, and the log's length, apart from the database
// itself (records.ts), so the pure modules and their tests can name them
// without the DOM.

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
  /** What the last backup holds of it: the newest this purse saved, or restored from when that is newer. */
  backup?: LastBackup;
}

/**
 * What a backup holds of a mint: the notes it held then. A note that moves
 * after that (a send, a hand-back, keep-alive) is spent, and the notes it
 * becomes are paid to keys the backup never saw, so it covers only the
 * notes held now that it names.
 */
export interface LastBackup {
  /** When it was saved: for a file restored, when the file says it last changed. */
  t: number;
  /** The serials of its notes of this mint, `generation:index`. */
  ids: string[];
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

/** How many log entries the purse keeps, past a send still on its way: Holdings shows the last few, History all. */
export const LOG_KEPT = 200;

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
   * hand-back taken in; `backup`: a backup saved (the amount it holds; the
   * log shows no change); `restored`: a backup restored; `checked`: notes
   * a restore brought, asked about at the mint (the amount it found spent).
   */
  kind: 'faucet' | 'faded' | 'kept' | 'lost' | 'sent' | 'received' | 'backup' | 'restored' | 'checked';
  /** The mint. */
  mint: string;
  /**
   * Tokens it brought (faucet, received), moved on (kept) or took away
   * (faded, lost, sent: what went, or what was asked until that is known;
   * checked: the notes the mint said were spent; lost: what was expected of
   * the transfer). backup: what it holds of the mint. restored: what it
   * changed the total by, with what it brought that had faded already,
   * which a `faded` entry takes away after it; or, with `turnedBack`, what
   * a send it brought would have paid.
   */
  amount: number;
  /**
   * The serials of the notes it brought, `generation:index`, so a note can
   * say where it came from: for keep-alive, the notes it moved on; for a
   * send, its change.
   */
  serials: string[];
  /**
   * faucet, received, kept, restored: the values of the notes in `serials`,
   * in their order, where the entry was written with them. backup: the
   * values of the notes it holds; checked: of those the mint said are live.
   */
  values?: number[];
  /**
   * faded: the values of the notes that faded, where they were read.
   * checked: of the notes the mint said were spent. restored: of the notes
   * held before that the backup had spent, which the purse dropped.
   */
  gone?: number[];
  /**
   * faded: how far `t` can be trusted: missing when it is the mint's
   * scheduled rotation; `after` when that fell before the notes came (a mint
   * rotated by hand), so `t` is when the last of them came and they faded
   * some time after; `restored` when they came with a restore, after the
   * rotation that took them, so they faded in the backup and `t` is just
   * after the restore; `by` when there was no schedule to go by, so `t` is
   * when the purse saw them gone.
   */
  fadeAt?: 'after' | 'by' | 'restored';
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
  /** sent: transfers made. restored: the transfers it brought that the mint had not been seen to answer, which the purse sends again. */
  transfers?: number;
  /** backup: when it was sealed, which may be before the entry was logged (a download said saved later). restored: when the backup was sealed, where its file says. */
  saved?: number;
  /** restored: the notes its transfers spend, which came with it: a fade of one is placed after it. */
  moved?: string[];
  /**
   * restored: written for a send the restore brought, not for the restore:
   * how many of its transfers the mint turned away, whose notes came back
   * (`serials`), with what they would have paid (`amount`).
   */
  turnedBack?: number;
  /** lost: of a send a restore brought, which this purse never made: its change, which the mint no longer kept. */
  brought?: true;
  /** sent: serials of the notes spent. */
  spent?: string[];
  /** sent: the values of the notes spent, in the order of `spent`, where the entry was written with them. */
  spentValues?: number[];
  /** sent: change that came back (its notes' serials are in `serials`). */
  back?: number;
  /**
   * sent: the value lost, the notes the transfers the mint dropped spent,
   * theirs and the change together, in whatever state: a send that lost
   * any is `lost` once nothing of it is on its way.
   */
  lost?: number;
  /** sent: the part of `lost` that was for the receiver. */
  lostTheirs?: number;
  /** sent: how many of its transfers the mint dropped (`lost`). */
  dropped?: number;
  /** sent: how many of its transfers the mint turned away: their notes came back. */
  turnedAway?: number;
  /**
   * sent: the purse never saw what some of its transfers came to: its first
   * submit ended after the page closed, before its end was written, and a
   * settle found fewer of them than it made. What it changed is not known.
   */
  blind?: boolean;
  /** sent, state turned: whether the mint turned it away because its newest layer was full (it can go after the rotation). */
  layerFull?: boolean;
  /** sent: when it was last updated: when its change came. */
  t2?: number;
  /** sent: when each note of its change came, by serial, for the notes that came before `t2` (a send settled later). */
  changeAt?: Record<string, number>;
}
