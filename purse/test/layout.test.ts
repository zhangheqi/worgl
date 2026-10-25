import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  MAX_LANES,
  MAX_SQUARES,
  type Placed,
  capped,
  fieldPlan,
  geomAt,
  geomBase,
  laneIndex,
  laneLayer,
  laneLayout,
  placeNotes,
  shape,
  shrinks,
} from '../src/layout.ts';

let order = 0;
const note = (v: number, pos: number): Placed => ({ id: `n${order}`, v, pos, order: order++ });

/** The prototype's afternoon after the keep-alive: 69 ready in the lower ready layer, 123 resting. */
const afternoon = (): Placed[] => [
  ...[64, 4, 1].map((v) => note(v, 2)),
  ...[64, 32, 16, 8, 2, 1].map((v) => note(v, 3)),
];

/** The same, before it: 5 fading, 64 ready. */
const fading = (): Placed[] => [note(4, 0), note(1, 0), note(64, 2), ...[64, 32, 16, 8, 2, 1].map((v) => note(v, 3))];

const busy = (notes: Placed[]): Set<number> => new Set(notes.map((n) => n.pos));

const tops = (plan: ReturnType<typeof fieldPlan>): number[][] =>
  plan.rows.map((row) => [Math.round(row.top * 100) / 100, Math.round(row.h * 100) / 100]);

describe('a note', () => {
  test('is a block of squares at most four rows tall', () => {
    assert.deepEqual(shape(64), { w: 16, h: 4 });
    assert.deepEqual(shape(32), { w: 8, h: 4 });
    assert.deepEqual(shape(16), { w: 4, h: 4 });
    assert.deepEqual(shape(8), { w: 2, h: 4 });
    assert.deepEqual(shape(4), { w: 2, h: 2 });
    assert.deepEqual(shape(2), { w: 1, h: 2 });
    assert.deepEqual(shape(1), { w: 1, h: 1 });
    assert.deepEqual(shape(5), { w: 2, h: 4 });
    assert.deepEqual(shape(256), { w: 64, h: 4 });
  });

  test('larger than 256 tokens draws 256 squares, however large it is', () => {
    assert.equal(MAX_SQUARES, 256);
    assert.equal(capped(256), false);
    assert.equal(capped(512), true);
    assert.deepEqual(shape(2 ** 40), { w: 64, h: 4 });
    for (const [width, vh] of [[328, 800], [1320, 900]] as const) {
      const big = note(2 ** 40, 3);
      const t0 = performance.now();
      const plan = fieldPlan(width, vh, 4, [big, note(64, 3)], { target: 300, fill: true, busy: new Set([3]) });
      const boxes = placeNotes(plan);
      assert.ok(performance.now() - t0 < 200, 'placed at once');
      assert.equal(boxes.get(big.id)?.squares.length, 256);
      assert.ok(plan.H < 1000, `a field ${plan.H} px tall`);
    }
  });
});

describe('the lanes', () => {
  test('shrink for the fading layer and every ready layer but the last', () => {
    assert.deepEqual([0, 1, 2, 3].map((pos) => shrinks(pos, 4)), [true, true, false, false]);
    assert.deepEqual([0, 1, 2].map((pos) => shrinks(pos, 3)), [true, false, false]);
    assert.deepEqual([0, 1].map((pos) => shrinks(pos, 2)), [true, false]);
    assert.deepEqual([0, 1, 2, 3, 4, 5].map((pos) => shrinks(pos, 6)), [true, true, true, true, false, false]);
  });

  test('run notes left to right, one column apart, and wrap the one that does not fit', () => {
    const G = geomAt(geomBase(328, 800), 7, 9);
    assert.equal(G.cols, 33);
    const list = [note(64, 3), note(64, 3), note(8, 3)];
    const { at, bands } = laneLayout(list, G);
    assert.deepEqual([...at.values()], [
      { col: 0, band: 0 },
      { col: 17, band: 0 },
      { col: 0, band: 1 },
    ]);
    assert.equal(bands, 2);
  });

  test('carry a note wider than the lane over further bands, not drop it', () => {
    const G = geomAt(geomBase(328, 800), 7, 9);
    const wide = note(256, 2); // 64 columns, in a lane of 33
    const after = note(1, 2);
    const { at, bands } = laneLayout([note(4, 2), wide, after], G);
    assert.deepEqual(at.get(wide.id), { col: 0, band: 1 });
    // 64 columns: 33 on its first band, 31 on the next, then a gap
    assert.deepEqual(at.get(after.id), { col: 32, band: 2 });
    assert.equal(bands, 3);
    const plan = fieldPlan(328, 800, 4, [wide], {});
    const box = placeNotes(plan).get(wide.id);
    assert.ok(box !== undefined);
    assert.equal(box.squares.length, 256);
    assert.equal(box.parts.length, 2);
    assert.ok(box.parts.every((part) => part.x + part.w <= plan.G.areaW + 0.5));
  });
});

describe('the field', () => {
  test('at 360 x 800 fills its height as the prototype does', () => {
    const notes = afternoon();
    const plan = fieldPlan(328, 800, 4, notes, { target: 247, fill: true, busy: busy(notes) });
    assert.equal(plan.G.p, 6.3);
    assert.equal(plan.G.s, 4.66);
    assert.equal(plan.G.pad, 30.47);
    assert.equal(plan.G.cols, 37);
    assert.equal(plan.G.areaW, 232);
    assert.equal(plan.rulerH, 36);
    assert.equal(plan.H, 247);
    assert.deepEqual(tops(plan), [[36, 20], [56, 20], [76, 84.5], [160.5, 84.5]]);
    assert.deepEqual(plan.rows.map((row) => row.collapsed), [true, true, false, false]);
  });

  test('at 360 x 640 keeps its squares and shortens the ruler', () => {
    const notes = afternoon();
    const plan = fieldPlan(328, 640, 4, notes, { target: 242, fill: true, busy: busy(notes) });
    assert.equal(plan.G.p, 6.3);
    assert.equal(plan.rulerH, 28);
    assert.equal(plan.G.pad, 31.22);
    assert.equal(plan.H, 242);
  });

  test('at 1440 x 900 uses the desktop squares', () => {
    const notes = afternoon();
    const plan = fieldPlan(1320, 900, 4, notes, { target: 275, fill: true, busy: busy(notes) });
    assert.equal(plan.G.desk, true);
    assert.equal(plan.G.p, 13);
    assert.equal(plan.G.s, 10.14);
    assert.equal(plan.G.cols, 85);
    assert.equal(plan.G.areaW, 1106);
    assert.equal(plan.G.pad, 23.18);
    assert.equal(plan.rulerH, 38);
    assert.equal(plan.H, 275);
  });

  test('opens the fading lane to its tallest note', () => {
    const notes = fading();
    const plan = fieldPlan(328, 800, 4, notes, { target: 247, fill: true, busy: busy(notes) });
    assert.equal(plan.G.pad, 21.82);
    assert.deepEqual(tops(plan), [[36, 54.6], [90.6, 20], [110.6, 67.2], [177.8, 67.2]]);
    assert.equal(plan.rows[0]?.nr, 2);
  });

  test('lets one note take larger squares', () => {
    const notes = [note(64, 2)];
    const plan = fieldPlan(328, 800, 4, notes, { target: 335, fill: true, busy: busy(notes) });
    assert.equal(plan.G.p, 8);
    assert.equal(plan.G.pad, 49.29);
    assert.equal(plan.H, 335);
  });

  test('never draws a note into the label column', () => {
    const notes = [...afternoon(), note(64, 3), note(64, 3)];
    const plan = fieldPlan(328, 800, 4, notes, { target: 247, fill: true, busy: busy(notes) });
    for (const box of placeNotes(plan).values()) {
      for (const part of box.parts) {
        assert.ok(part.x + part.w <= plan.G.areaW + 0.5, `${box.note.v} at ${part.x}`);
      }
    }
  });

  test('drawn as a slice keeps every lane open and four rows tall', () => {
    const notes = fading();
    const plan = fieldPlan(328, 800, 4, notes, { ruler: false, shrink: false });
    assert.deepEqual(plan.rows.map((row) => row.collapsed), [false, false, false, false]);
    assert.deepEqual(plan.rows.map((row) => row.nr), [4, 4, 4, 4]);
    const heights = plan.rows.map((row) => row.h);
    assert.ok(heights.every((each) => each === heights[0]), `lanes ${heights.join(', ')} px tall`);
    // the same notes on Holdings: the empty lane collapses and the fading one opens only to its tallest note
    const home = fieldPlan(328, 800, 4, notes, { ruler: false });
    assert.equal(home.rows[1]?.collapsed, true);
    assert.equal(home.rows[0]?.nr, 2);
  });

  test('drawn as a slice opens its bands for a tag between them, and places the notes the same', () => {
    // the Ready lane of a two-lane slice at 360 px, wrapped onto two bands
    const notes = [note(64, 0), note(8, 0), note(32, 0), note(1, 0), note(1, 0), note(4, 0), note(64, 0)];
    const plain = fieldPlan(328, 780, 2, notes, { ruler: false, shrink: false });
    const open = fieldPlan(328, 780, 2, notes, { ruler: false, shrink: false, bandGap: 28 });
    assert.equal(plain.rows[0]?.bands, 2);
    assert.ok(plain.G.bandGap < 28);
    assert.equal(open.G.bandGap, 28);
    assert.equal(open.H - plain.H, 28 - plain.G.bandGap);
    const before = placeNotes(plain);
    // the first band stays where it was; the second moves down by what the gap grew
    for (const [id, box] of placeNotes(open)) {
      const was = before.get(id);
      const second: boolean = (was?.y ?? 0) > (plain.rows[0]?.by ?? 0) + 0.5;
      assert.equal(box.x, was?.x, id);
      assert.equal(Math.round((box.y - (was?.y ?? 0)) * 100) / 100, second ? 28 - plain.G.bandGap : 0, id);
    }
    // a gap smaller than the slice's own changes nothing
    assert.deepEqual(fieldPlan(328, 780, 2, notes, { ruler: false, shrink: false, bandGap: 2 }).G, plain.G);
  });

  test('has a lane for every layer of the mint', () => {
    const plan = fieldPlan(328, 800, 6, [note(8, 5), note(4, 2)], {});
    assert.equal(plan.rows.length, 6);
    assert.deepEqual(plan.rows.map((row) => row.collapsed), [true, true, false, true, false, false]);
    assert.deepEqual(plan.rows.map((row) => row.pos), [0, 1, 2, 3, 4, 5]);
  });

  test('of a mint with more layers than lanes shares the lanes between among the ready layers', () => {
    assert.equal(MAX_LANES, 8);
    const layers = 65_536;
    assert.equal(laneIndex(0, layers), 0);
    assert.equal(laneIndex(1, layers), 1);
    assert.equal(laneIndex(layers - 2, layers), 6);
    assert.equal(laneIndex(layers - 1, layers), 7);
    for (let lane = 0; lane < MAX_LANES; lane++) {
      const first = laneLayer(lane, layers);
      assert.equal(laneIndex(first, layers), lane, `lane ${lane} starts at ${first}`);
      if (lane > 0 && lane < MAX_LANES - 1) {
        assert.equal(laneIndex(first - 1, layers), lane - 1);
      }
    }
    const notes = [note(4, 0), note(8, 5000), note(16, 40_000), note(64, layers - 1)];
    const t0 = performance.now();
    const plan = fieldPlan(328, 800, layers, notes, { target: 400, fill: true, busy: new Set(notes.map((n) => n.pos)) });
    assert.ok(performance.now() - t0 < 200, 'placed at once');
    assert.equal(plan.rows.length, MAX_LANES);
    assert.ok(plan.H < 1000, `a field ${plan.H} px tall`);
    assert.deepEqual(plan.rows.map((row) => row.list.map((each) => each.v)), [[4], [8], [], [], [16], [], [], [64]]);
    assert.equal(plan.rows[0]?.pos, 0);
    assert.equal(plan.rows[7]?.pos, layers - 1);
    // with as many layers as lanes, or fewer, each layer has its own
    assert.deepEqual([0, 1, 2, 3, 4, 5, 6, 7].map((pos) => laneIndex(pos, 8)), [0, 1, 2, 3, 4, 5, 6, 7]);
  });
});
