import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  ASK_UNWRITTEN,
  CAMERA,
  COPY,
  HANDBACK,
  HOW_IT_GOES,
  QR,
  SEND_NOW,
  SMALLER,
  TAKE_READING,
  TURNED_REQUEST_HEADING,
  type ShortInput,
  afterWords,
  amountWords,
  askHint,
  askRowWords,
  atMintWords,
  backLine,
  cameraWords,
  cancelQuestion,
  carriesWords,
  chosenWords,
  durationLine,
  estimateSecs,
  explainWords,
  keptWords,
  linkProblemWords,
  lostBody,
  madeWords,
  notesLabel,
  nothingBack,
  alreadyBody,
  partialBody,
  partialLine,
  plainText,
  receivedWords,
  recentRow,
  recvCap,
  scanCap,
  sendOfLine,
  sentLine,
  shortWords,
  takeEndWords,
  ticketWords,
  turnedBody,
  unitWords,
  unknownBody,
} from '../src/flow-words.ts';
import type { ShortCase } from '../src/plan.ts';
import type { AskRecord, LogEntry } from '../src/entries.ts';

const at = (day: number, hours: number, minutes = 0): number => new Date(2026, 9, day, hours, minutes).getTime();
const NOW = at(21, 14, 32);
const AT = '16:00';
const FORBIDDEN = /\b(wallet|cash|coin|account|pay)/i;

/** Every line said here, checked at the end for words the purse never uses. */
const said: string[] = [];
const keep = <T>(value: T): T => {
  said.push(JSON.stringify(value));
  return value;
};

const sent = (extra: Partial<LogEntry>): LogEntry => ({
  id: 1,
  t: at(21, 14, 2),
  kind: 'sent',
  mint: 'a'.repeat(64),
  amount: 5,
  asked: 5,
  serials: [],
  ...extra,
});

const input = (extra: Partial<ShortInput> = {}): ShortInput => ({
  amount: 20,
  outputs: [16, 4],
  ready: 0,
  resting: 0,
  at: AT,
  faucet: { amount: 64, mint: 'Playground', ready: false },
  ...extra,
});

/** The short words, as plain text throughout. */
function short(c: ShortCase, i: ShortInput): { h1: string; body: string; options: string[]; message: string; tag: string } {
  const words = keep(shortWords(c, i));
  return {
    h1: plainText(words.h1),
    body: plainText(words.body),
    options: words.options.map((o) => `${o.text} | ${o.small}${o.faucet === true ? ' | faucet' : ''}`),
    message: words.message,
    tag: words.tag,
  };
}

describe('Receive', () => {
  test('says how an amount comes, and what is wrong with one', () => {
    assert.equal(keep(askHint(20, [16, 4])), '20 comes as notes of 16 and 4, each to a fresh key.');
    assert.equal(keep(askHint(64, [64])), '64 comes as one note of 64, to a fresh key.');
    assert.equal(keep(askHint(128, [64, 64])), '128 comes as two notes of 64, each to a fresh key.');
    assert.equal(keep(amountWords('not_whole')), 'Type a whole number of tokens, 1 or more.');
    assert.equal(keep(amountWords('zero')), 'Type a whole number of tokens, 1 or more.');
    assert.equal(keep(amountWords('too_big')), 'Ask for 4096 or less at a time.');
    assert.equal(unitWords(1), 'token');
    assert.equal(unitWords(20), 'tokens');
    assert.equal(unitWords(undefined), 'tokens');
  });

  test('says where what is taken in lands', () => {
    assert.equal(plainText(keep(recvCap(undefined, [], AT))), 'What you take in usually rests in the last layer until the next rotation.');
    assert.equal(plainText(keep(recvCap(64, [64], AT))), 'Taken in, the 64 lands here and usually rests until 16:00.');
    assert.equal(plainText(keep(recvCap(20, [16, 4], AT))), 'Taken in, the 20 lands here as 16 + 4 and usually rests until 16:00.');
    assert.equal(plainText(keep(recvCap(255, [128, 64, 32, 16, 8, 4, 2, 1], AT))), 'Taken in, the 255 lands here as 8 notes, the first four shown, and usually rests until 16:00.');
    assert.deepEqual(recvCap(20, [16, 4], AT)[1], { b: '16:00' });
  });

  test('says when and how a request was made', () => {
    assert.equal(keep(madeWords(NOW, [64])), 'Made at 14:32 as one note, to a fresh key. Once they send, they give you a hand-back.');
    assert.equal(keep(madeWords(NOW, [4, 1])), 'Made at 14:32 as 4 and 1, each to its own key. Once they send, they give you a hand-back.');
    assert.equal(keep(madeWords(NOW, [64, 64])), 'Made at 14:32 as two notes of 64, each to its own key. Once they send, they give you a hand-back.');
  });

  test('lists open requests and asks before cancelling one', () => {
    const ask: AskRecord = {
      id: '0'.repeat(64),
      mint: 'a'.repeat(64),
      server: 'https://x',
      amount: 128,
      notes: [64, 64],
      atMost: 64,
      json: '{}',
      t: at(21, 14, 2),
      state: 'open',
      got: 0,
      endedAt: undefined,
    };
    assert.deepEqual(keep(askRowWords(ask, NOW)), {
      title: 'Ask for 128',
      small: ' · in notes of at most 64',
      label: 'Your request for 128, made 14:02. Show it',
    });
    assert.equal(askRowWords({ ...ask, atMost: undefined }, NOW).small, undefined);
    assert.equal(
      keep(cancelQuestion(20)),
      'Cancel the request for 20? If they send to it after this, those tokens are lost: no purse can take them in.',
    );
  });

  test('says how a take ended', () => {
    const o = { amount: 5, now: NOW, host: 'mint.example.org' };
    const twice = keep(takeEndWords('twice', { ...o, at: at(21, 14, 2) }));
    assert.equal(plainText(twice.h1), 'Taken in already.');
    assert.equal(twice.body, 'This purse took these 5 in at 14:02.');
    assert.equal(takeEndWords('twice', o).body, 'This purse took these tokens in already.');
    const cancelled = keep(takeEndWords('cancelled', { ...o, at: at(20, 9, 15) }));
    assert.equal(plainText(cancelled.h1), 'Nothing was taken in.');
    assert.equal(
      cancelled.body,
      'You cancelled this request at Tue 09:15, so this purse no longer holds its keys and cannot take these 5 in.',
    );
    assert.equal(
      takeEndWords('cancelled', o).body,
      'You cancelled this request, so this purse no longer holds its keys and cannot take these 5 in.',
    );
    assert.equal(plainText(keep(takeEndWords('not-ours', o)).h1), 'Not for this purse.');
    assert.match(keep(takeEndWords('unheld', o)).body, /a mint at mint\.example\.org that this purse does not hold/);
    assert.equal(keep(takeEndWords('failed', { ...o, words: 'The mint did not answer.' })).body, 'The mint did not answer.');
  });

  test('says where tokens taken in landed, in five forms', () => {
    assert.equal(keep(receivedWords(20, 0, 20, 2, AT)), 'Received 20. They rest until 16:00, then they are ready to send.');
    assert.equal(keep(receivedWords(64, 0, 64, 1, AT)), 'Received 64. It rests until 16:00, then it is ready to send.');
    assert.equal(keep(receivedWords(5, 5, 0, 2, AT)), 'Received 5. They are ready to send now.');
    assert.equal(keep(receivedWords(4, 4, 0, 1, AT)), 'Received 4. It is ready to send now.');
    assert.equal(keep(receivedWords(20, 4, 16, 2, AT)), 'Received 20: 4 ready now, 16 resting until 16:00.');
  });
});

describe('Send', () => {
  test('says what can be sent now, in four forms', () => {
    assert.equal(plainText(keep(scanCap(69, 123, AT, 64))), '69 ready to send now · 123 more at 16:00');
    assert.equal(plainText(keep(scanCap(69, 0, AT, 64))), '69 ready to send now');
    assert.equal(plainText(keep(scanCap(0, 1, AT, 64))), 'Nothing is ready to send until 16:00, when 1 is.');
    assert.equal(plainText(keep(scanCap(0, 128, AT, 64))), 'Nothing is ready to send until 16:00, when 128 are.');
    assert.equal(plainText(keep(scanCap(0, 0, AT, 64))), 'This purse is empty. Take 64 from the faucet on Holdings first.');
    assert.equal(plainText(keep(scanCap(0, 0, AT, undefined))), 'This purse is empty. Ask someone to send you some first.');
  });

  test('names a kept hand-back and the ticket', () => {
    assert.deepEqual(keep(keptWords(5, at(21, 14, 2), NOW)), { lab: 'tokens · 14:02', label: 'The hand-back for 5 tokens, sent 14:02' });
    assert.deepEqual(keep(ticketWords('camera', NOW, 20)), { lab: 'Scanned request · 14:32', b: 'for 20 tokens', qr: 'The scanned request' });
    assert.equal(ticketWords('link', NOW, 20).lab, 'Opened request · 14:32');
  });

  test('says when a request was sent to already', () => {
    assert.equal(keep(alreadyBody('kept', 5, at(21, 14, 2), NOW)), 'At 14:02, 5 went to it. If they did not get the hand-back, show it again.');
    assert.equal(
      keep(alreadyBody('outgoing', 5, at(21, 14, 2), NOW)),
      'At 14:02, this purse sent 5 to it, and the mint has not answered yet. The hand-back comes once it does.',
    );
    assert.match(keep(alreadyBody('gone', 5, undefined, NOW)), /sends to a request once/);
    // turned away: nothing went, and the same request can never go, so a new one is the way
    assert.equal(
      keep(alreadyBody('turned', 5, at(21, 14, 2), NOW)),
      'At 14:02, this purse sent to it and the mint turned the send away: nothing went, and your note is back with you. A request takes one send, so ask them for a new one.',
    );
    assert.equal(plainText(TURNED_REQUEST_HEADING), 'This request was turned away.');
  });

  test('names a mint this purse does not hold, and what to do', () => {
    assert.equal(
      keep(unknownBody(5, 'mint.example.org')),
      'This request asks for 5 tokens of a mint at mint.example.org, which this purse does not hold. Add that mint, then send.',
    );
  });

  test('offers ways out of more than this purse holds', () => {
    const c = short({ kind: 'over', has: 128 }, input({ amount: 300, outputs: [256, 32, 8, 4], ready: 64, resting: 64 }));
    assert.equal(c.h1, 'More than this purse holds.');
    assert.equal(c.body, 'This purse holds 128; the request is for 300. 64 are ready now and 64 rest until 16:00.');
    assert.deepEqual(c.options, [
      'Ask them for 128 instead. | It can go after 16:00, when all 128 are ready.',
      'Take 64 from the Playground faucet, or receive more first. | Faucet notes rest until 16:00 too. | faucet',
    ]);
    assert.equal(c.message, 'I have 128, not 300, and only 64 are ready before 16:00. Could you make a request for 128 instead?');
    assert.equal(c.tag, '172 short');
    const ready = short({ kind: 'over', has: 64 }, input({ amount: 100, outputs: [64, 32, 4], ready: 64, faucet: undefined }));
    assert.equal(ready.body, 'This purse holds 64; the request is for 100.');
    assert.deepEqual(ready.options, [
      'Ask them for 64 instead. | This purse can send that now.',
      'Receive more first. | Ask someone to send you some, then send this.',
    ]);
    assert.equal(ready.message, 'I have 64, not 100. Could you make a request for 64 instead?');
    const resting = short({ kind: 'over', has: 64 }, input({ amount: 100, outputs: [64, 32, 4], resting: 64, faucet: { amount: 64, mint: 'Playground', ready: true } }));
    assert.equal(resting.body, 'This purse holds 64; the request is for 100. All of it rests until 16:00.');
    assert.equal(resting.options[0], 'Ask them for 64 instead. | It can go after 16:00.');
    assert.equal(resting.options[1], 'Take 64 from the Playground faucet, or receive more first. | Faucet notes are ready at once. | faucet');
    assert.equal(resting.message, 'I have 64, not 100, and it is ready after 16:00. Could you make a request for 64 instead?');
  });

  test('offers ways out of an empty purse', () => {
    const c = short({ kind: 'over', has: 0 }, input());
    assert.equal(c.h1, 'This purse is empty.');
    assert.equal(c.body, 'The request is for 20.');
    // nothing rests here, so the faucet's notes do not rest "too"
    assert.deepEqual(c.options, ['Take 64 from the Playground faucet, or receive more first. | Faucet notes rest until 16:00. | faucet']);
    assert.equal(c.message, 'My purse is empty right now. I’ll send to your request for 20 once it holds enough.');
    assert.equal(c.tag, '20 short');
  });

  test('offers waiting when nothing is ready, and the faucet only when its notes land ready', () => {
    const c = short({ kind: 'none', has: 123 }, input({ resting: 123 }));
    assert.equal(c.h1, 'Nothing is ready until 16:00.');
    assert.equal(c.body, 'This purse holds 123, all resting until 16:00. Then this request can go.');
    assert.deepEqual(c.options, ['Wait until 16:00, when 123 are ready. | Then this request can go as it is.']);
    assert.equal(c.message, 'I can send it after 16:00. I’ll send to this same request then.');
    assert.equal(c.tag, '20 short now');
    const faucet = short({ kind: 'none', has: 64 }, input({ resting: 64, faucet: { amount: 64, mint: 'Playground', ready: true } }));
    assert.equal(faucet.options[0], 'Wait until 16:00, when 64 are ready. | Then this request can go as it is.');
    assert.equal(faucet.options.length, 2);
  });

  test('offers two requests when some is ready', () => {
    const c = short({ kind: 'later', has: 187 }, input({ amount: 150, outputs: [128, 16, 4, 2], ready: 123, resting: 64 }));
    assert.equal(c.h1, 'More than is ready now.');
    assert.equal(c.body, '123 are ready now and 64 more at 16:00.');
    assert.deepEqual(c.options, [
      'Ask them for 123 now and 27 after 16:00. | Two requests, each sent when it can go.',
      'Wait until 16:00, when 187 are ready. | Then this request can go as it is.',
    ]);
    assert.equal(c.message, 'I can send 123 now and 27 after 16:00. Could you make two requests, one for 123 and one for 27?');
    assert.equal(c.tag, '27 short now');
  });

  test('offers smaller notes when no ready note is big enough', () => {
    const c = short({ kind: 'notes', largest: 64, atMost: 64, maxOut: 128, waitHelps: false }, input({ amount: 128, outputs: [128], ready: 187 }));
    assert.equal(c.h1, 'No ready note is big enough.');
    assert.equal(c.body, 'The request asks for one note of 128; the largest note ready here is 64.');
    assert.deepEqual(c.options, ['Ask them to ask again in notes of at most 64. | On Receive, under Ask in smaller notes.']);
    assert.equal(c.message, 'I can’t send a note of 128 right now. Could you ask again for 128, in notes of at most 64?');
    assert.equal(c.tag, 'Needs a note of 128');
    const none = short({ kind: 'notes', largest: 3, atMost: undefined, maxOut: 4, waitHelps: true }, input({ amount: 6, outputs: [4, 2], ready: 6 }));
    assert.deepEqual(none.options, [
      'Ask them for less, in two requests. | The notes here cannot make 6 in one request.',
      'Wait until 16:00, when a larger note is ready. | Then this request can go as it is.',
    ]);
    assert.equal(none.message, 'I can’t send 6 in one go from the notes I have. Could you make two smaller requests?');
    assert.equal(none.body, 'The request asks for 4 and 2; the largest note ready here is 3.');
  });

  test('offers larger notes when the change would make too many', () => {
    const c = short({ kind: 'outputs', maxOutputs: 16 }, input({ amount: 255, outputs: Array.from({ length: 255 }, () => 1), ready: 256 }));
    assert.equal(c.h1, 'Too many small notes for one transfer.');
    assert.equal(c.body, 'One transfer here makes at most 16 notes, and this request needs more, with the change.');
    assert.deepEqual(c.options, ['Ask them to ask again in larger notes. | A new request, without Ask in smaller notes.']);
    assert.equal(c.message, 'I can’t send to a request in so many small notes. Could you ask again for 255 in larger notes?');
  });

  test('explains what comes back, and what stays ready', () => {
    assert.equal(keep(explainWords(1, [32, 16, 8, 2, 1], 'rest', AT)), 'The 59 comes back as notes of 32, 16, 8, 2 and 1, resting until 16:00.');
    assert.equal(keep(explainWords(1, [2, 1], 'ready', AT)), 'The 3 comes back as notes of 2 and 1, ready to send.');
    assert.equal(keep(explainWords(1, [], undefined, AT)), 'Nothing comes back: the note goes whole.');
    assert.equal(keep(explainWords(2, [32, 16, 8, 2, 1], 'rest', AT)), 'Two transfers, one per note, a few seconds apart. 59 comes back, resting until 16:00.');
    assert.equal(keep(explainWords(2, [2, 1], 'ready', AT)), 'Two transfers, one per note, a few seconds apart. 3 comes back, ready to send.');
    assert.equal(keep(explainWords(2, [1, 1], 'mixed', AT)), 'Two transfers, one per note, a few seconds apart. 2 comes back, part ready and part resting until 16:00.');
    assert.equal(keep(explainWords(2, [], undefined, AT)), 'Two transfers, one per note, a few seconds apart. Nothing comes back.');
    assert.equal(keep(afterWords(64, 123, AT)), 'After this, 64 are ready now and 123 more at 16:00.');
    assert.equal(keep(afterWords(64, 0, AT)), 'After this, 64 are ready.');
    assert.equal(keep(afterWords(0, 123, AT)), 'After this, nothing is ready until 16:00, when 123 are.');
    assert.equal(keep(afterWords(0, 1, AT)), 'After this, nothing is ready until 16:00, when 1 is.');
    assert.equal(keep(afterWords(0, 0, AT)), 'After this, the purse is empty.');
    assert.equal(notesLabel(1), 'One note');
    assert.equal(notesLabel(2), 'Two notes');
    assert.equal(estimateSecs(1), 1);
    assert.equal(estimateSecs(3), 9);
    assert.equal(keep(durationLine(1)), 'One transfer, at once. Keep this open while it goes.');
    assert.equal(keep(durationLine(2)), 'Keep this open about 5 s while it goes.');
  });

  test('says what is sending, and at the mint', () => {
    assert.equal(keep(chosenWords([64])), 'One note of 64 chosen');
    assert.equal(keep(chosenWords([64, 64])), 'Two notes chosen: 64 and 64');
    assert.deepEqual(keep(atMintWords(1, 0, { done: false, slow: false }, 5, 59)), { line: 'At the mint', small: 'One transfer.' });
    assert.deepEqual(keep(atMintWords(2, 1, { done: false, slow: false }, 128, 0)), {
      line: 'At the mint · 1 of 2',
      small: 'One transfer per note, a few seconds apart.',
    });
    assert.equal(atMintWords(2, 2, { done: false, slow: true }, 128, 0).small, 'Waiting for the mint’s answer.');
    assert.equal(keep(atMintWords(1, 1, { done: true, slow: false }, 5, 59)).small, 'Answered: 5 for them, 59 back to you.');
    assert.equal(atMintWords(2, 2, { done: true, slow: false }, 128, 0).small, 'Answered: 128 for them.');
  });

  test('says what came back with the hand-back', () => {
    const ready = at(21, 16);
    assert.deepEqual(keep(backLine(59, 'rest', ready, NOW)), { b: '59 came back to you', muted: ' · resting until 16:00' });
    assert.deepEqual(keep(backLine(59, 'rest', ready, at(21, 16, 5))), { b: '59 came back to you', muted: ' · rested until 16:00' });
    assert.deepEqual(keep(backLine(3, 'ready', undefined, NOW)), { b: '3 came back to you', muted: ' · ready to send' });
    // where it landed is not known (the mint did not answer the read after the send): no claim
    assert.deepEqual(keep(backLine(59, undefined, undefined, NOW)), { b: '59 came back to you', muted: '' });
    // resting, but the mint's info placed no time for it: no tail either
    assert.deepEqual(keep(backLine(59, 'rest', undefined, NOW)), { b: '59 came back to you', muted: '' });
    assert.deepEqual(keep(backLine(59, 'mixed', undefined, NOW)), { b: '59 came back to you', muted: '' });
    assert.equal(keep(nothingBack(1)), 'Nothing came back: the note went whole.');
    assert.equal(keep(nothingBack(2)), 'Nothing came back: the notes went whole.');
    assert.deepEqual(keep(partialLine(64, 128, 1)), {
      b: '64 of 128 went.',
      muted: ' The mint turned one transfer away, and its note is back with you.',
    });
    assert.equal(partialLine(64, 192, 2).muted, ' The mint turned 2 transfers away, and their notes are back with you.');
    assert.equal(keep(partialBody(64, 128)), 'It carries their 64. Ask them to make a new request for the other 64.');
    assert.equal(keep(partialBody(undefined, 128)), 'It carries part of their 128. Their purse says how much when they take it in.');
    assert.equal(keep(carriesWords(5)), 'It carries their 5. When they open it, their purse takes them in.');
  });

  test('says why a send was turned away or lost, under a label that says no more went than did', () => {
    // the same request is never sent to twice, so after the rotation it is a new request that goes
    assert.equal(
      keep(turnedBody(true, AT)),
      'Its newest layer is full, so your note is back with you and nothing went. After its next rotation, at 16:00, ask them for a new request.',
    );
    assert.doesNotMatch(turnedBody(true, AT), /try again/i);
    assert.equal(keep(sentLine(5, at(21, 14, 2), NOW)), 'Sent 5 · 14:02');
    assert.equal(keep(sendOfLine(5, at(21, 14, 2), NOW)), 'Send of 5 · 14:02');
    assert.match(keep(turnedBody(false, AT)), /^Its rules refuse this send now/);
    assert.equal(
      keep(lostBody(5, 64)),
      'The mint says the note this send spent is gone, and it no longer keeps the answer. The 5 for them and your change are lost: 64 in all.',
    );
  });
});

describe('Recent', () => {
  test('names each kind of entry, and what a row opens', () => {
    const row = (entry: LogEntry, kept = true): [string, string | undefined, number | undefined, string | undefined] => {
      const r = keep(recentRow(entry, kept, NOW));
      return [r.title, r.small, r.delta, r.opens];
    };
    assert.deepEqual(row({ t: NOW, kind: 'faucet', mint: 'm', amount: 64, serials: [] }), ['Took 64 from the faucet', undefined, 64, undefined]);
    assert.deepEqual(row({ t: NOW, kind: 'faded', mint: 'm', amount: 4, serials: [] }), ['Faded 4', undefined, -4, undefined]);
    assert.deepEqual(row({ t: NOW, kind: 'received', mint: 'm', amount: 5, serials: [] }), ['Received 5', undefined, 5, undefined]);
    assert.deepEqual(row(sent({ state: 'sending' })), ['Sending 5', undefined, -5, 'sending']);
    assert.deepEqual(row(sent({ state: 'waiting' })), ['Sent 5', ' · waiting', -5, 'waiting']);
    assert.deepEqual(row(sent({ state: 'handback', went: 5 })), ['Sent 5', ' · hand-back', -5, 'handback']);
    assert.deepEqual(row(sent({ state: 'handback', went: 5 }), false), ['Sent 5', undefined, -5, undefined]);
    assert.deepEqual(row(sent({ state: 'partial', asked: 128, went: 64 })), ['Sent 64 of 128', ' · hand-back', -64, 'handback']);
    assert.deepEqual(row(sent({ state: 'partial', asked: 128 })), ['Sent part of 128', ' · hand-back', undefined, 'handback']);
    // its hand-back gone, a partial send reads as any send that went; what went unknown, it never says all of it did
    assert.deepEqual(row(sent({ state: 'partial', asked: 128, went: 64 }), false), ['Sent 64', undefined, -64, undefined]);
    assert.deepEqual(row(sent({ state: 'partial', asked: 128 }), false), ['Sent part of 128', undefined, undefined, undefined]);
    assert.deepEqual(row(sent({ state: 'done', went: 5 })), ['Sent 5', undefined, -5, undefined]);
    assert.deepEqual(row(sent({ state: 'turned' })), ['Turned away: 5', undefined, undefined, undefined]);
    assert.deepEqual(row(sent({ state: 'lost', lost: 64 })), ['Lost 64 on a send of 5', undefined, -64, 'lost']);
  });

  test('tells assistive tech what a row opens', () => {
    assert.equal(recentRow(sent({ state: 'handback', went: 5 }), true, NOW).label, 'Sent 5 at 14:02. Show the hand-back again');
    assert.equal(recentRow(sent({ state: 'waiting' }), true, NOW).label, 'Sent 5 at 14:02. Waiting for the mint');
    assert.equal(recentRow(sent({ state: 'sending' }), true, NOW).label, 'Sending 5. Show the send');
    assert.equal(recentRow(sent({ state: 'lost', lost: 64 }), true, NOW).label, 'Lost 64 on a send of 5. Show what happened');
  });
});

describe('links and the camera', () => {
  test('say what is wrong with a link, for the kind looked for', () => {
    const take = (problem: Parameters<typeof linkProblemWords>[1], kind?: 'ask' | 'take'): string => keep(linkProblemWords('take', problem, kind));
    const ask = (problem: Parameters<typeof linkProblemWords>[1], kind?: 'ask' | 'take'): string => keep(linkProblemWords('ask', problem, kind));
    assert.equal(take('empty'), 'Paste their hand-back link first.');
    assert.equal(take('unreadable', 'ask'), 'That is a request, not a hand-back. Their hand-back has #take= in it.');
    assert.equal(take('unreadable', 'take'), 'This does not read as a hand-back. Check that you copied the whole link.');
    assert.equal(take('not_a_link'), 'This does not read as a hand-back. Check that you copied the whole link.');
    assert.equal(take('mint_link'), 'That is a mint’s link, not a hand-back. Their hand-back has #take= in it.');
    assert.equal(take('newer', 'take'), 'This link comes from a newer purse. Reload this page to update it, then open the link again.');
    assert.equal(take('server', 'take'), 'This hand-back cannot be read. Ask them to show it again.');
    assert.equal(take('too_long'), 'This link is longer than any request or hand-back. Check that you copied only the link.');
    assert.equal(ask('empty'), 'Paste their request link first.');
    assert.equal(ask('unreadable', 'take'), 'That is a hand-back, for taking tokens in. To send, open their request: a link with #ask= in it.');
    assert.equal(ask('kind'), 'That is a hand-back, for taking tokens in. To send, open their request: a link with #ask= in it.');
    assert.equal(ask('not_a_link'), 'This does not read as a request. Check that you copied the whole link.');
    assert.equal(ask('request'), 'This request cannot be read. Ask them to show it again.');
    assert.equal(ask('mint_link'), 'That is a mint’s link, not a request. To send, open their request: a link with #ask= in it.');
    assert.equal(ask('server', 'ask'), 'This request names a server this purse cannot reach: it reaches mints only over https.');
  });

  test('say why the camera does not run, by the browser’s reason', () => {
    const words = (name: string): string => keep(cameraWords({ kind: 'refused', name }));
    assert.match(words('NotAllowedError'), /^The camera is off for this purse/);
    assert.match(words('NotFoundError'), /^This device has no camera/);
    assert.match(words('OverconstrainedError'), /^This device has no camera/);
    assert.match(words('NotReadableError'), /^Another app is using the camera/);
    assert.match(words('AbortError'), /^Another app is using the camera/);
    for (const name of ['SecurityError', 'TypeError', 'Mystery']) {
      assert.equal(words(name), 'This browser offers no camera here. Paste their link instead.');
    }
    assert.equal(keep(cameraWords({ kind: 'running' })), 'Point the camera at their code.');
    assert.equal(keep(cameraWords({ kind: 'unsupported' })), 'This browser offers no camera here. Paste their link instead.');
    assert.equal(keep(cameraWords({ kind: 'no-reader' })), 'The code reader did not load. Paste their link instead.');
    assert.equal(cameraWords({ kind: 'idle' }), '');
  });
});

describe('the purse’s words', () => {
  test('never say wallet, cash, coin, account or pay', () => {
    said.push(
      JSON.stringify([SMALLER, HOW_IT_GOES, COPY, HANDBACK, QR, CAMERA, SEND_NOW, TAKE_READING, ASK_UNWRITTEN]),
      ...(['failed', 'twice', 'not-ours', 'cancelled', 'unheld'] as const).map((end) =>
        JSON.stringify(takeEndWords(end, { amount: 5, at: NOW, now: NOW, host: 'x' })),
      ),
    );
    assert.ok(said.length > 100, `${said.length} lines checked`);
    for (const line of said) {
      assert.doesNotMatch(line, FORBIDDEN, line);
    }
  });
});
