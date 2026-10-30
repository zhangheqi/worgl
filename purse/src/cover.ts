// What Backup works out and says: how much of what the purse holds the
// last backup covers, the passphrase's rule, the file's name, which mints
// a backup restores to which servers, and the lines of the screen. Pure,
// so it is tested: the view (backup.ts) builds them into elements, and
// the app (app.ts) does what they plan through purses.ts.
//
// A backup holds the notes as they were when it was saved. A note that
// moves after that (a send, a hand-back taken in, keep-alive) is spent,
// and the notes it becomes are paid to keys the backup never saw: so a
// backup covers exactly the notes held now that it names, by serial.

import { cap, hm, notesWords, toks, when, word } from './format.ts';
import { parseServer } from './link.ts';

// ---------- Save ----------

/** The fewest characters a passphrase may have. */
export const PASS_MIN = 8;

/** What is wrong with the passphrase typed twice. */
export type PassProblem = 'short' | 'differ';

/**
 * A passphrase as it is sealed with: composed (NFC), so an accented letter
 * typed as one character or as a letter and a mark seals alike, on any
 * device and keyboard.
 */
export const passOf = (typed: string): string => typed.normalize('NFC');

/** What is wrong with the passphrase typed twice, if anything. Counted in characters, not UTF-16 units, once composed. */
export function passProblem(first: string, again: string): PassProblem | undefined {
  if ([...passOf(first)].length < PASS_MIN) {
    return 'short';
  }
  return passOf(first) === passOf(again) ? undefined : 'differ';
}

/** What a passphrase problem says. */
export const passWords = (problem: PassProblem): string =>
  problem === 'short' ? `Use a passphrase of ${PASS_MIN} characters or more.` : 'The two passphrases differ. Type the same one twice.';

const pad = (n: number): string => String(n).padStart(2, '0');

/** The file a backup is saved as, by the local time it was sealed: `worgl-purse-2026-10-30-1415.worgl`. */
export function fileName(t: number): string {
  const d = new Date(t);
  return `worgl-purse-${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}.worgl`;
}

/** What a backup's file is saved as: no type a browser would open, so it is only kept. */
export const FILE_TYPE = 'application/octet-stream';

/**
 * The words of a save that went as a download: the browser saves it, and
 * it may ask first, or be told no. So it counts as a backup only once the
 * person says the file is there.
 */
export const pendingWords = (name: string): string =>
  `The browser is saving ${name}. Check that it is with your files, then say so below: until then it does not count as a backup.`;

/** The words of a save that went: the tokens it holds, by mint. */
export function savedWords(held: readonly { name: string; unit: string; amount: number }[], t: number): string {
  const holding = held.filter((each) => each.amount > 0);
  const what =
    holding.length === 0
      ? 'nothing yet: the purse holds no tokens now'
      : holding.length === 1 && holding[0] !== undefined
        ? amountOf(holding[0].amount, holding[0].unit)
        : holding.map((each) => `${amountOf(each.amount, each.unit)} of ${each.name}`).join(', ');
  return `Saved at ${hm(t)}. The file holds ${what}. Keep it somewhere apart from this device; you need it and the passphrase to bring them back.`;
}

/** `64 tokens`, `1 credit`: the unit as the mint's record names it, made singular for one. */
function amountOf(n: number, unit: string): string {
  if (unit === 'tokens') {
    return toks(n);
  }
  return `${n} ${n === 1 && unit.endsWith('s') ? unit.slice(0, -1) : unit}`;
}

// ---------- Coverage ----------

/** A note held now, as Backup reads it: its serial and value. */
export interface HeldNote {
  id: string;
  v: number;
}

/** What the last backup covers of one mint's notes held now. */
export interface Cover {
  covered: number;
  total: number;
  /** The notes, largest first, each with whether the backup covers it. */
  notes: { v: number; in: boolean }[];
}

/**
 * What a backup of `ids` (serials) covers of the notes held now, by value.
 * `total` is what the purse holds of the mint, which counts tokens on their
 * way back too (a send's change, keep-alive's hops): none of those is
 * covered, since none is a note the backup names.
 */
export function coverOf(notes: readonly HeldNote[], ids: readonly string[] | undefined, total: number): Cover {
  const known = new Set(ids ?? []);
  const drawn = [...notes]
    .map((note) => ({ v: note.v, in: known.has(note.id) }))
    .sort((a, b) => b.v - a.v || Number(b.in) - Number(a.in));
  const covered = drawn.reduce((sum, note) => sum + (note.in ? note.v : 0), 0);
  return { covered, total: Math.max(total, covered), notes: drawn };
}

/** Whether Backup asks for a new backup: the purse holds tokens, and the last backup covers under half of them. */
export const wantsBackup = (cover: Pick<Cover, 'covered' | 'total'>): boolean => cover.total > 0 && cover.covered * 2 < cover.total;

/** `Thu 09:15`, `09:15` today: when the last backup was saved, as Backup's large line shows it. */
export const savedWhen = (t: number, now: number): string => when(t, now);

/** The line under the last backup, for one mint (`name` only when the purse holds several). */
export function coverWords(cover: Pick<Cover, 'covered' | 'total'> | undefined, t: number | undefined, now: number, name?: string): string {
  const of = name === undefined ? '' : ` of ${name}`;
  if (t === undefined || cover === undefined) {
    return name === undefined ? 'There is no backup of this purse yet.' : `There is no backup${of} yet.`;
  }
  const since = when(t, now);
  const { covered, total } = cover;
  if (total === 0) {
    return `The purse holds nothing${of} now, so there is nothing for a backup to cover.`;
  }
  if (covered === total) {
    return `Your last backup covers all ${toks(total)}${of}.`;
  }
  if (covered === 0) {
    return `Your last backup covers 0 of ${toks(total)}${of}. Every token here has moved or arrived since ${since}.`;
  }
  return `Your last backup covers ${covered} of ${toks(total)}${of}. The rest have moved or arrived since ${since}.`;
}

/** The label at the right of "Last backup": `52 / 64 covered`, or `None`. */
export const coverLab = (cover: Pick<Cover, 'covered' | 'total'> | undefined): string =>
  cover === undefined ? 'None' : `${cover.covered} / ${cover.total} covered`;

/** Holdings' line beside the mint's key on the desktop: `Covers 52 of 64 · Thu 09:15`, or `None yet`. */
export const coverRow = (cover: Pick<Cover, 'covered' | 'total'> | undefined, t: number | undefined, now: number): string =>
  t === undefined ? 'None yet' : cover === undefined ? `Saved ${when(t, now)}` : `Covers ${cover.covered} of ${cover.total} · ${when(t, now)}`;

// ---------- Restore ----------

/** The most a backup file may weigh: a purse file is a few kilobytes, and its notes and records are capped. */
export const FILE_MAX = 8 * 1024 * 1024;

/** What a chosen file is, before any passphrase: one to open, or why not. */
export type FileProblem = 'empty' | 'too_big' | 'not_sealed';

/** What is wrong with a chosen file, from its size and text alone; `undefined` for a purse file to open. */
export function fileProblem(size: number, text: string | undefined): FileProblem | undefined {
  if (size === 0) {
    return 'empty';
  }
  if (size > FILE_MAX) {
    return 'too_big';
  }
  if (text === undefined) {
    return undefined;
  }
  try {
    const parsed = JSON.parse(text) as unknown;
    return typeof parsed === 'object' && parsed !== null && (parsed as { format?: unknown }).format === 'worgl-purse' ? undefined : 'not_sealed';
  } catch {
    return 'not_sealed';
  }
}

/** What a file problem says. */
export function fileWords(problem: FileProblem): string {
  switch (problem) {
    case 'empty':
      return 'This file is empty. Choose the backup file the purse saved.';
    case 'too_big':
      return 'This file is far larger than a purse backup. Choose the backup file the purse saved.';
    case 'not_sealed':
      return 'This is not a purse backup. Choose the file the purse saved, ending in .worgl.';
  }
}

/** A mint a backup names, as the SDK's `Backup.mints` lists it. */
export interface BackupMint {
  mint: string;
  server: string | undefined;
  extra: Record<string, unknown>;
}

/** A mint the purse keeps, as far as a restore plan reads it. */
export interface KnownMint {
  mint: string;
  server: string;
}

/**
 * A mint to restore: the server whose purse takes it. A new one is named as
 * a link's would be, by its server, whatever name or unit the file gives it:
 * a file can come from anyone, and its words are not shown as the purse's.
 */
export interface PlannedMint {
  mint: string;
  server: string;
  /** Whether the purse keeps this mint already. */
  known: boolean;
  /**
   * When the backup was sealed, as the purse that sealed it wrote it in the
   * mint's entry; `undefined` for a file without it, such as the `worgl`
   * command line's or an older purse's. A file's own time is when that copy
   * was made, which says nothing of what it holds.
   */
  saved: number | undefined;
}

/** Which mints of a backup go to which server's purse, and those no server is known for. */
export interface RestorePlan {
  mints: PlannedMint[];
  /** Mints this purse does not keep that the backup names no server for, or one the purse does not reach (plain http to another machine): added by link first. */
  unplaced: string[];
}

/** When a backup's entry for a mint says it was sealed: a time in milliseconds, or `undefined`. */
function savedOf(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

/**
 * Plans a restore: a mint the purse keeps goes to its own server's purse
 * (where its tokens are, whatever the backup says); a new one to the
 * server the backup names, as a link would name it; one with none named,
 * or one the purse would not reach from a link, cannot be placed.
 */
export function restorePlan(mints: readonly BackupMint[], known: readonly KnownMint[]): RestorePlan {
  const kept = new Map(known.map((each) => [each.mint, each.server]));
  const plan: RestorePlan = { mints: [], unplaced: [] };
  for (const each of mints) {
    const server = kept.get(each.mint);
    const named = each.server === undefined ? undefined : parseServer(each.server);
    const saved = savedOf(each.extra['saved']);
    if (server !== undefined) {
      plan.mints.push({ mint: each.mint, server, known: true, saved });
    } else if (named?.ok === true && /^[0-9a-f]{64}$/.test(each.mint)) {
      plan.mints.push({ mint: each.mint, server: named.server, known: false, saved });
    } else {
      plan.unplaced.push(each.mint);
    }
  }
  return plan;
}

/** What a restore came to at one mint, for its line. */
export interface RestoredAt {
  name: string;
  unit: string;
  /** Notes it brought that count, by value. */
  brought: number[];
  /** Notes it brought whose layer had gone already, by value: they count for nothing. */
  faded: number[];
  /** Notes held before that the backup had spent, dropped, by value. */
  dropped: number[];
  /** Transfers it brought that the mint had not been seen to answer. */
  transfers: number;
}

/** The lines a restore leaves on Backup: what came of each mint, and what could not be placed. Why a mint failed is errors.ts's to say. */
export function restoredWords(at: readonly RestoredAt[], unplaced: number): string[] {
  const lines: string[] = [];
  const several = at.length > 1;
  for (const each of at) {
    const of = several ? ` of ${each.name}` : '';
    const live = each.brought.reduce((sum, v) => sum + v, 0);
    const n = each.brought.length;
    const faded = each.faded.length;
    let line: string;
    if (n === 0 && faded === 0) {
      line = `Nothing${of} in the backup was new to this purse.`;
    } else if (n === 0) {
      line = `The backup held ${notesWords(each.faded)}${of} this purse did not, but ${faded === 1 ? 'it has' : 'they have all'} faded since.`;
    } else {
      line = `Restored ${amountOf(live, each.unit)}${of}, in ${n === 1 ? 'one note' : `${word(n)} notes`}.`;
      if (faded > 0) {
        line += ` ${cap(notesWords(each.faded))} it held had faded since.`;
      }
      line += ` The purse checks ${n === 1 ? 'it' : 'each'} with the mint before ${n === 1 ? 'it moves' : 'they move'}.`;
    }
    if (each.dropped.length > 0) {
      const v = each.dropped.reduce((sum, x) => sum + x, 0);
      line += ` The backup had spent ${v} this purse still held, so ${v === 1 ? 'that is' : 'those are'} dropped.`;
    }
    if (each.transfers > 0) {
      line += ` ${each.transfers === 1 ? 'A transfer it held goes' : `${cap(word(each.transfers))} transfers it held go`} again.`;
    }
    lines.push(line);
  }
  if (unplaced > 0) {
    lines.push(
      `The backup also holds ${unplaced === 1 ? 'a mint' : `${word(unplaced)} mints`} at no server this purse can reach from it. Add ${unplaced === 1 ? 'it' : 'each'} by its link, then restore the file again.`,
    );
  }
  return lines;
}

// ---------- Checks ----------

/** Where the checks of restored notes stand: notes asked about, and what came of them. */
export interface Checking {
  /** Notes still to ask about. */
  left: number;
  /** Notes asked about in this run. */
  done: number;
  /** Their value the mint said is live, and spent. */
  live: number;
  spent: number;
  /** Their value whose layer had gone by the time the mint was asked: dropped as a fade. */
  faded: number;
  /** Notes the mint did not answer for: asked again later. */
  unanswered: number;
}

/** The line Backup shows while restored notes are checked, and once they are. */
export function checkingWords(c: Checking): string {
  if (c.left > 0) {
    const all = c.left + c.done;
    return `Checking ${all === 1 ? 'one restored note' : `${word(all)} restored notes`} with the mint, a few seconds apart: ${c.done} done.`;
  }
  const dropped = [...(c.spent > 0 ? [`${c.spent} already spent`] : []), ...(c.faded > 0 ? [`${c.faded} faded`] : [])];
  const parts = [
    ...(c.live > 0 ? [`${c.live} live`] : []),
    ...(dropped.length === 0 ? [] : [`${dropped.join(' and ')}, ${dropped.length === 1 ? '' : 'both '}now dropped`]),
  ];
  const done = `Checked ${c.done === 1 ? 'one restored note' : `${word(c.done)} restored notes`}${parts.length === 0 ? '' : `: ${parts.join(', ')}`}.`;
  return c.unanswered === 0
    ? done
    : `${done} The mint did not answer for ${c.unanswered === 1 ? 'one' : word(c.unanswered)}; the purse asks again on its next open.`;
}

// ---------- Storage ----------

/** What Backup says of how this browser keeps the purse. */
export function storageWords(persisted: boolean | undefined, standalone: boolean): string[] {
  return [
    persisted === true
      ? 'This browser has agreed to keep the purse’s data until you clear it.'
      : 'This browser may clear the purse’s data when it runs short of space.',
    ...(standalone ? [] : ['On iPhone or iPad, add this purse to your Home Screen so Safari keeps it.']),
    'Clearing this site’s data empties the purse, and a private window forgets it when it closes. A backup brings back the notes it covers.',
  ];
}
