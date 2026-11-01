// What Mints says, without the DOM, so it can be tested: each mint's
// timetable and clock, its faucet, its whole key to compare with the one
// its people give, the name a new mint starts with, and what a typed name,
// server or key lacks. mints.ts builds these into elements.

import { periodWords } from './format.ts';
import type { Schedule } from './holdings.ts';
import { type ServerProblem, hostOf, parseServer, shortKey } from './link.ts';
import { rotatesQuickly, visitSpan } from './words.ts';

/** How many rotations a mint's timetable shows. */
export const TIMETABLE = 4;

/** The next {@link TIMETABLE} rotations of a mint that rotates on its own, by this device's clock: the next first. */
export function timetable(schedule: Schedule | undefined): number[] {
  if (schedule === undefined || !(schedule.periodMs > 0)) {
    return [];
  }
  return Array.from({ length: TIMETABLE }, (_, k) => schedule.nextMs + k * schedule.periodMs);
}

/** A mint's rotation, as its card says it over its timetable: `Every 8 h · 4 layers`, `By hand · 4 layers`, `Paused · 4 layers`. */
export function rotatesRow(layers: number, periodSecs: number | undefined, paused: boolean): string {
  const how = paused ? 'Paused' : periodSecs === undefined ? 'By hand' : `Every ${periodWords(periodSecs)}`;
  return `${how} · ${layers} ${layers === 1 ? 'layer' : 'layers'}`;
}

/** The line under a mint's timetable: what a rotation does, and how long a visit keeps its tokens; or why there is no timetable. */
export function clockWords(periodSecs: number | undefined, paused: boolean): string {
  if (paused) {
    return 'Paused: it takes no transfers and does not rotate until its operator resumes it.';
  }
  if (periodSecs === undefined) {
    return 'It rotates when its operator says, so there are no times to show. Each rotation drops the first layer.';
  }
  const quick = rotatesQuickly(periodSecs) ? ', so it suits services more than people' : '';
  return `Each rotation drops the first layer. A visit keeps this mint’s tokens for ${visitSpan(periodSecs)}${quick}.`;
}

/** A faucet, as its card's row says it: `64 a tap · 10 taps an hour`. */
export function faucetRow(amount: number, perHour: number): string {
  return `${amount} a tap · ${perHour} ${perHour === 1 ? 'tap' : 'taps'} an hour`;
}

/**
 * A whole key in eight groups of eight digits, in four pairs: four groups to
 * a line, or two on a narrow phone, so a line never breaks inside a pair. The
 * face and the first eight tell mints apart, but only the whole key is the
 * mint, so this is what to compare with the key its people give.
 */
export function keyPairs(key: string): [string, string, string, string] {
  const groups = key.match(/.{1,8}/g) ?? [];
  const pair = (i: number): string => groups.slice(i, i + 2).join(' ');
  return [pair(0), pair(2), pair(4), pair(6)];
}

/** The most characters a mint's name may have. */
export const NAME_MAX = 40;

/** A name typed for a mint, as it is kept: composed (NFC), its spaces run together, trimmed. */
export const nameOf = (text: string): string => text.normalize('NFC').replace(/\s+/g, ' ').trim();

/** Why a name typed for a mint will not do, if it will not. */
export function nameProblem(text: string): string | undefined {
  const name = nameOf(text);
  if (name === '') {
    return 'Give it a name: a word or two you will know it by.';
  }
  if ([...name].length > NAME_MAX) {
    return `Keep the name to ${NAME_MAX} characters or fewer.`;
  }
  return undefined;
}

/**
 * The name a mint starts with, before the person gives it one: the build's
 * playground's, else its server's host; with the key's first eight digits
 * after it when another mint the purse holds has that name already.
 */
export function startName(mint: string, server: string, playground: boolean, taken: readonly string[]): string {
  const name = playground ? 'Playground' : hostOf(server);
  return taken.includes(name) ? `${name} · ${shortKey(mint)}` : name;
}

/** A server's address typed by hand: `https://` before it when it names none, so `mint.example.org` will do. */
export function typedServer(text: string): { ok: true; server: string } | { ok: false; words: string } {
  const trimmed = text.trim();
  if (trimmed === '') {
    return { ok: false, words: serverWords('server') };
  }
  const parsed = parseServer(/^[a-z][a-z0-9+.-]*:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`);
  return parsed.ok ? parsed : { ok: false, words: serverWords(parsed.problem) };
}

/** What is wrong with a server's address typed by hand. */
export function serverWords(problem: ServerProblem): string {
  return problem === 'insecure'
    ? 'This purse reaches mints only over https. Type the address that starts with https://'
    : 'Type the server’s address, such as https://mint.example.org';
}

/** A key typed by hand: its 64 hex digits in lower case, spaces and line breaks between them dropped; or what is wrong with it. */
export function typedKey(text: string): { ok: true; key: string } | { ok: false; words: string } {
  const digits = text.replace(/\s+/g, '');
  if (digits === '') {
    return { ok: false, words: 'Type the mint’s key: 64 hex digits, 0 to 9 and a to f.' };
  }
  if (!/^[0-9a-fA-F]+$/.test(digits)) {
    return { ok: false, words: 'A mint’s key has only hex digits: 0 to 9 and a to f.' };
  }
  if (digits.length !== 64) {
    return { ok: false, words: `The key has ${digits.length} hex ${digits.length === 1 ? 'digit' : 'digits'}; a mint’s key has 64.` };
  }
  return { ok: true, key: digits.toLowerCase() };
}

/**
 * What Mints says of a mint a link or a key names that the purse holds
 * already: under its name, at the server it knows. The purse never moves a
 * mint it holds to another server on a link's word, since trust follows the
 * key, not the address.
 */
export function heldWords(name: string, heldAt: string, named: string): string {
  return heldAt === named
    ? `This purse holds this mint already, as ${name}.`
    : `This purse holds this mint already, as ${name}, at ${hostOf(heldAt)}, and keeps it there. This names ${hostOf(named)}.`;
}

/** Said once a mint is added: Holdings shows the mint used last, which is now this one. */
export const addedWords = (name: string): string => `Added ${name}. Holdings shows it now.`;

/** Said once a mint is renamed. */
export const renamedWords = (name: string): string => `Renamed it ${name}.`;
