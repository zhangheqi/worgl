// Runs the purse's planner (src/plan.ts) against the SDK's own pay() on a
// live worgl-server, and says where they part. Optional, and not run in CI:
// `npm test` mirrors the Rust code paths, and this asks the real thing.
//
// Needs worgl-server and worgl-faucet built (`cargo build -p worgl-server
// -p worgl-faucet`), or WORGL_SERVER and WORGL_FAUCET_BINARY pointing at
// them, as the SDK's own tests do. Run with `npm run plan-check`.
//
// It is a .mjs script so that tsc does not type-check the SDK's sources
// under the purse's settings; Node strips the types of what it imports.

import { MemoryStore, NO_SPACING, Purse, isWorglError } from '../../sdk/typescript/src/index.ts';
import { TestServer } from '../../sdk/typescript/test/server.ts';
import { plan, policyFrom } from '../src/plan.ts';

/** The requests: an amount, and the largest note it may ask for. */
const REQUESTS = [
  { amount: 5n },
  { amount: 20n },
  { amount: 64n },
  { amount: 69n },
  { amount: 100n },
  { amount: 128n },
  { amount: 255n, atMost: 64n },
];

/** How each purse is funded before it is asked: steps of [amount, in notes of at most]. */
const FUNDINGS = [
  { name: '64x4', steps: [[256n, 64n]] },
  { name: '4096x2+64x4', steps: [[8192n, 4096n], [256n, 64n]] },
  { name: 'mixed, after a rotation', steps: [[96n, 32n], 'rotate', [37n, 64n], [128n, 64n]] },
];

const sum = (values) => values.reduce((a, b) => a + b, 0);
const sorted = (values) => [...values].sort((a, b) => (typeof a === 'number' ? a - b : String(a).localeCompare(String(b))));
const id = (serial) => `${serial.generation}:${serial.index}`;

/** The planner's answer, in the shape the SDK's is read into. */
function planned(notes, request, info) {
  const { policy, layers } = policyFrom(JSON.parse(info));
  const ready = notes
    .filter((note) => (note.state === 'spendable' || note.state === 'expiring') && note.position !== undefined)
    .map((note) => ({ id: id(note.serial), v: Number(note.amount), pos: note.position }));
  const p = plan(ready, request.notes.map((note) => Number(note.amount)), policy, layers);
  if (p === undefined) {
    return { says: 'nothing (past a safe integer)' };
  }
  if (p.ok) {
    return {
      spent: sorted(p.parts.map((part) => part.note.id)),
      change: sorted(p.parts.flatMap((part) => part.change)),
    };
  }
  if (p.why === 'fragmented') {
    return { error: 'fragmented', largest: p.largest, atMost: p.atMost };
  }
  if (p.why === 'unaffordable') {
    return { error: 'unaffordable', spendable: p.spendable, needed: p.needed };
  }
  return { error: p.why, maxOutputs: p.maxOutputs };
}

/** What pay() did: the notes that left and the change it signed, or the error it threw. */
async function sdkDid(sender, request) {
  const before = (await sender.notesAt(request.mint)).map((note) => id(note.serial));
  const theirs = new Set(request.notes.map((note) => note.holder));
  let made;
  try {
    made = await sender.pay(request, NO_SPACING);
  } catch (error) {
    if (!isWorglError(error)) {
      throw error;
    }
    const d = error.details;
    if (error.code === 'fragmented') {
      return { error: 'fragmented', largest: Number(d.largest), atMost: d.atMost === undefined ? undefined : Number(d.atMost) };
    }
    if (error.code === 'unaffordable') {
      return { error: 'unaffordable', spendable: Number(d.spendable), needed: Number(d.needed) };
    }
    if (error.code === 'too_many_outputs') {
      return { error: 'too_many_outputs', maxOutputs: d.maxOutputs };
    }
    return { error: error.code };
  }
  const after = new Set((await sender.notesAt(request.mint)).map((note) => id(note.serial)));
  const change = [];
  for (const transfer of made.transfers) {
    for (const output of outputsOf(JSON.parse(transfer.request))) {
      if (!theirs.has(output.holder)) {
        change.push(Number(output.amount));
      }
    }
  }
  return { spent: sorted(before.filter((each) => !after.has(each))), change: sorted(change) };
}

/** The outputs of a transfer as the SDK writes it: each with its amount and holder key. */
function outputsOf(transfer) {
  const outputs = transfer.outputs ?? transfer.transfer?.outputs;
  if (!Array.isArray(outputs)) {
    throw new Error(`a transfer with no outputs list: ${JSON.stringify(transfer).slice(0, 200)}`);
  }
  return outputs.map((output) => ({ amount: BigInt(output.amount), holder: output.holder ?? output.key }));
}

const words = (answer) =>
  answer.says ??
  (answer.error !== undefined
    ? `${answer.error}${answer.largest === undefined ? '' : ` largest ${answer.largest} atMost ${answer.atMost ?? '-'}`}${answer.spendable === undefined ? '' : ` ${answer.spendable}/${answer.needed}`}${answer.maxOutputs === undefined ? '' : ` max ${answer.maxOutputs}`}`
    : `spends ${answer.spent.length} (change ${answer.change.join(' ') || 'none'})`);

/** Funds a fresh sender as `funding` says, asks for `ask` from a fresh receiver, then plans the send and has the SDK sign it. */
async function check(server, funding, ask) {
  const sender = await Purse.open(server.client(), { store: new MemoryStore() });
  const receiver = await Purse.open(server.client(), { store: new MemoryStore() });
  for (const step of funding.steps) {
    if (step === 'rotate') {
      await server.rotate();
    } else {
      await server.fund(sender, step[0], step[1]);
    }
  }
  const request =
    ask.atMost === undefined
      ? await receiver.request(server.mint, ask.amount)
      : await receiver.requestAtMost(server.mint, ask.amount, ask.atMost);
  const info = await server.client().infoText(server.mint);
  const notes = await sender.notesAt(server.mint);
  const mine = planned(notes, request, info);
  const theirs = await sdkDid(sender, request);
  const same = JSON.stringify(mine) === JSON.stringify(theirs);
  if (!same) {
    console.error(`plan-check: ${funding.name}, ${ask.amount}: the planner says ${JSON.stringify(mine)}, the SDK did ${JSON.stringify(theirs)}`);
  }
  return {
    funding: funding.name,
    request: `${ask.amount}${ask.atMost === undefined ? '' : ` at most ${ask.atMost}`}`,
    held: sum(notes.filter((note) => note.state !== 'expired').map((note) => Number(note.amount))),
    planner: words(mine),
    sdk: words(theirs),
    agree: same ? 'yes' : 'NO',
  };
}

const start = () => TestServer.start({ mint_layer: 2 }, undefined, 65536);
const rows = [];
const shared = await start();
try {
  for (const funding of FUNDINGS) {
    for (const ask of REQUESTS) {
      if (!funding.steps.includes('rotate')) {
        // a sender of its own each time, on one mint: pay() spends
        rows.push(await check(shared, funding, ask));
        continue;
      }
      // a rotation moves every purse's notes, so this funding takes a mint of its own each time
      const own = await start();
      try {
        rows.push(await check(own, funding, ask));
      } finally {
        await own.stop();
      }
    }
  }
} finally {
  await shared.stop();
}
const mismatches = rows.filter((row) => row.agree !== 'yes').length;

console.table(rows);
console.log(`plan-check: ${rows.length - mismatches} of ${rows.length} agree`);
process.exit(mismatches === 0 ? 0 : 1);
