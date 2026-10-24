// Numbers, words and times as the purse writes them. Every time is the
// device's local time, 24-hour, HH:MM with leading zeros, from milliseconds
// since the epoch; a time from a mint is turned into this device's clock
// before it gets here (see holdings.ts).

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const NUMBER_WORDS = ['no', 'one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight', 'nine', 'ten'];

const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;

/** Rounds to the hundredth, as the field's geometry and its SVG paths use it. */
export const f2 = (n: number): number => Math.round(n * 100) / 100;

const pad = (n: number): string => String(n).padStart(2, '0');

/** `14:32`. */
export function hm(t: number): string {
  const d = new Date(t);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** The weekday, `Thu`. */
export function weekday(t: number): string {
  return DAYS[new Date(t).getDay()] ?? '';
}

const sameDay = (a: number, b: number): boolean => new Date(a).toDateString() === new Date(b).toDateString();

/**
 * A time as a sentence names it: `14:32` on today's date, `Thu 07:00` on
 * another, and `Thu 29 Oct 07:00` six or more days away, where a weekday
 * alone would be ambiguous.
 */
export function when(t: number, now: number): string {
  if (sameDay(t, now)) {
    return hm(t);
  }
  if (Math.abs(t - now) >= 6 * DAY) {
    const d = new Date(t);
    return `${weekday(t)} ${d.getDate()} ${MONTHS[d.getMonth()]} ${hm(t)}`;
  }
  return `${weekday(t)} ${hm(t)}`;
}

/**
 * A time in a short column, as Recent shows it: `14:32` today, else the
 * weekday, and `29 Oct` six or more days away, as {@link when} names it.
 */
export function shortDay(t: number, now: number): string {
  if (sameDay(t, now)) {
    return hm(t);
  }
  if (Math.abs(t - now) >= 6 * DAY) {
    const d = new Date(t);
    return `${d.getDate()} ${MONTHS[d.getMonth()]}`;
  }
  return weekday(t);
}

/** A mint's period: `8 h` in whole hours (`24 h` for a day), else `20 min`, or seconds under a minute. */
export function periodWords(seconds: number): string {
  if (seconds >= 3600 && seconds % 3600 === 0) {
    return `${seconds / 3600} h`;
  }
  if (seconds >= 60 && seconds % 60 === 0) {
    return `${seconds / 60} min`;
  }
  if (seconds >= 60) {
    return `${Math.round(seconds / 60)} min`;
  }
  return `${seconds} s`;
}

/** `one` to `ten`, then digits; `no` for 0. */
export const word = (n: number): string => NUMBER_WORDS[n] ?? String(n);

/** The first letter in capitals. */
export const cap = (s: string): string => s.charAt(0).toUpperCase() + s.slice(1);

/** `5 tokens`, `1 token`. */
export const toks = (n: number): string => `${n} ${n === 1 ? 'token' : 'tokens'}`;

/** `64`, `64 and 1`, `64, 4 and 1`. */
export function listWords(items: readonly (number | string)[]): string {
  if (items.length <= 1) {
    return String(items[0] ?? '');
  }
  return `${items.slice(0, -1).join(', ')} and ${String(items[items.length - 1])}`;
}

/** Largest first, whatever order a request lists its notes in. */
const largestFirst = (values: readonly number[]): number[] => [...values].sort((a, b) => b - a);

/**
 * Notes by their values, largest first: `one note of 64`, `notes of 16 and
 * 4`, `three notes of 64 and one of 8`.
 */
export function notesWords(given: readonly number[]): string {
  const values = largestFirst(given);
  if (values.length === 1) {
    return `one note of ${values[0]}`;
  }
  const groups: { v: number; count: number }[] = [];
  for (const v of values) {
    const group = groups.find((each) => each.v === v);
    if (group === undefined) {
      groups.push({ v, count: 1 });
    } else {
      group.count++;
    }
  }
  if (groups.every((group) => group.count === 1)) {
    return `notes of ${listWords(values)}`;
  }
  return listWords(
    groups.map(
      (group, i) =>
        `${word(group.count)} ${i === 0 ? (group.count === 1 ? 'note ' : 'notes ') : ''}of ${group.v}`,
    ),
  );
}

/** Notes by their values, largest first: `16 and 4` when every one differs, else as {@link notesWords} names them. */
export const notesShort = (values: readonly number[]): string =>
  values.length > 1 && new Set(values).size === values.length ? listWords(largestFirst(values)) : notesWords(values);

/** `64 + 4 + 1`. */
export const plusJoin = (values: readonly number[]): string => values.join(' + ');

/** A change in tokens: `+64`, `−5` (with the minus sign), `—` for none. */
export function change(delta: number | undefined): string {
  if (delta === undefined || delta === 0) {
    return '—';
  }
  return `${delta > 0 ? '+' : '−'}${Math.abs(delta)}`;
}
