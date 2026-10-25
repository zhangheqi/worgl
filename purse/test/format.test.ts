import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  cap,
  change,
  f2,
  hm,
  listWords,
  notesShort,
  notesWords,
  periodWords,
  plusJoin,
  shortDay,
  toks,
  when,
  word,
} from '../src/format.ts';

/** Wed 21 October 2026, 14:32, in this machine's time zone, as every time the purse shows. */
const NOW = new Date(2026, 9, 21, 14, 32).getTime();
const at = (day: number, hours: number, minutes = 0): number => new Date(2026, 9, day, hours, minutes).getTime();

describe('times', () => {
  test('are 24-hour HH:MM with leading zeros', () => {
    assert.equal(hm(at(21, 7, 5)), '07:05');
    assert.equal(hm(at(21, 16)), '16:00');
    assert.equal(hm(at(21, 0)), '00:00');
  });

  test('name the weekday when they are not today', () => {
    assert.equal(when(at(21, 16), NOW), '16:00');
    assert.equal(when(at(22, 8), NOW), 'Thu 08:00');
    assert.equal(when(at(20, 23, 59), NOW), 'Tue 23:59');
    assert.equal(when(at(26, 23), NOW), 'Mon 23:00');
  });

  test('name the date six or more days away', () => {
    assert.equal(when(at(27, 14, 32), NOW), 'Tue 27 Oct 14:32');
    assert.equal(when(at(29, 7), NOW), 'Thu 29 Oct 07:00');
    assert.equal(when(at(14, 9), NOW), 'Wed 14 Oct 09:00');
  });

  test('in Recent, are the time today, the weekday before, and the date from six days back', () => {
    assert.equal(shortDay(at(21, 9, 15), NOW), '09:15');
    assert.equal(shortDay(at(19, 9, 15), NOW), 'Mon');
    assert.equal(shortDay(at(16, 9, 15), NOW), 'Fri');
    assert.equal(shortDay(at(15, 9, 15), NOW), '15 Oct');
    assert.equal(shortDay(new Date(2026, 8, 3, 12).getTime(), NOW), '3 Sep');
  });

  test('name a period in hours, minutes or seconds', () => {
    assert.equal(periodWords(28_800), '8 h');
    assert.equal(periodWords(86_400), '24 h');
    assert.equal(periodWords(3600), '1 h');
    assert.equal(periodWords(1200), '20 min');
    assert.equal(periodWords(5400), '90 min');
    assert.equal(periodWords(300), '5 min');
    assert.equal(periodWords(90), '2 min');
    assert.equal(periodWords(45), '45 s');
  });
});

describe('numbers and words', () => {
  test('are plain integers, with words up to ten', () => {
    assert.equal(word(0), 'no');
    assert.equal(word(4), 'four');
    assert.equal(word(10), 'ten');
    assert.equal(word(11), '11');
    assert.equal(toks(1), '1 token');
    assert.equal(toks(192), '192 tokens');
    assert.equal(cap('notes of 4'), 'Notes of 4');
  });

  test('list and group notes', () => {
    assert.equal(listWords([64]), '64');
    assert.equal(listWords([16, 4]), '16 and 4');
    assert.equal(listWords([64, 32, 16, 8, 2, 1]), '64, 32, 16, 8, 2 and 1');
    assert.equal(notesWords([64]), 'one note of 64');
    assert.equal(notesWords([4, 1]), 'notes of 4 and 1');
    assert.equal(notesWords([64, 64, 64, 8]), 'three notes of 64 and one of 8');
    assert.equal(notesWords([64, 64]), 'two notes of 64');
    // a request lists its notes in any order; the words name the largest first
    assert.equal(notesWords([1, 64, 4]), 'notes of 64, 4 and 1');
    assert.equal(notesWords([8, 64, 64]), 'two notes of 64 and one of 8');
  });

  test('name a request’s notes short: the values alone when each differs, else grouped', () => {
    assert.equal(notesShort([16, 4]), '16 and 4');
    // in any order, the largest first
    assert.equal(notesShort([1, 64, 4]), '64, 4 and 1');
    assert.equal(notesShort([64, 64]), 'two notes of 64');
    assert.equal(notesShort([64, 64, 32, 64, 16, 8, 4, 2, 1]), 'three notes of 64, one of 32, one of 16, one of 8, one of 4, one of 2 and one of 1');
    assert.equal(notesShort([5]), 'one note of 5');
  });

  test('join a slice’s notes with plus signs, in the order given', () => {
    assert.equal(plusJoin([16, 4]), '16 + 4');
    assert.equal(plusJoin([4, 64, 1]), '4 + 64 + 1');
    assert.equal(plusJoin([8]), '8');
    assert.equal(plusJoin([]), '');
  });

  test('write a change with a plus or a minus sign', () => {
    assert.equal(change(64), '+64');
    assert.equal(change(-5), '−5');
    assert.equal(change(undefined), '—');
    assert.equal(change(0), '—');
  });

  test('round geometry to the hundredth', () => {
    assert.equal(f2(4.662), 4.66);
    assert.equal(f2(1 / 3), 0.33);
  });
});
