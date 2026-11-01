// Offline: what the purse shows and does without its mints. Each mint's
// last info answer is kept (purses.ts keeps it in Cache Storage); while a
// mint cannot be reached, Holdings and Mints read what the purse holds by
// that answer, and Receive asks by it. Nothing a mint must sign or answer
// runs on it: sending, taking in and keep-alive wait for the mint. No DOM
// or SDK here, so it can be tested.

import { when } from './format.ts';

/** Whether `url` is a mint's info route, as the SDK's Client asks it. */
export const isInfoUrl = (url: string): boolean => /\/v1\/mints\/[0-9a-f]{64}\/info$/.test(url);

/**
 * The mint's clock less this device's, in ms, from the `Date` of an answer
 * and when it came. `Date` counts whole seconds, so under two seconds is
 * none, as Holdings' reads take it; and an answer with no `Date` the purse
 * can read says nothing of the mint's clock.
 */
export function skewOf(date: string | null, at: number): number {
  const answered = date === null ? Number.NaN : Date.parse(date);
  if (Number.isNaN(answered)) {
    return 0;
  }
  const raw = answered - at;
  return Math.abs(raw) < 2000 ? 0 : Math.round(raw / 1000) * 1000;
}

/**
 * Whether what a mint's saved info says still holds at `mintNow` (by the
 * mint's clock): until its next scheduled rotation, after which the layers
 * it lists are not the mint's, and no clock moves them (the SDK reads a
 * note's layer from its info alone). A mint that rotates by hand, or is
 * paused, rotates no sooner than its operator says, so what it said holds
 * until it says otherwise; the purse shows it with when it was read.
 */
export function holdsAt(rotation: { nextAt: bigint } | undefined, paused: boolean, mintNow: number): boolean {
  return rotation === undefined || paused || mintNow < Number(rotation.nextAt) * 1000;
}

/** Holdings' line over the total, while what it shows was read from saved info: when the purse last reached the mint. */
export const asAtWords = (name: string, at: number, now: number): string => `${name} · as at ${when(at, now)}`;

/** Whether this device says it has no network: the browser's word, which is sure only when it says so. */
export const offlineNow = (): boolean => (globalThis as { navigator?: { onLine?: boolean } }).navigator?.onLine === false;
