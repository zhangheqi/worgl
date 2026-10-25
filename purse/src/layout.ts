// The field's geometry: one square per token, one lane per layer of the
// mint, first layer at the top, under a ruler of the current rotation.
// Worked out from the field's own width (not the viewport's), the screen's
// height and the notes, with no DOM, so it can be tested; field.ts draws it.
//
// What a mint sends is bounded here, since a mint may hand out a note of
// any power of two and have up to 65536 layers: a note draws at most
// MAX_SQUARES squares (field.ts writes a larger one's value on it), and the
// field has at most MAX_LANES lanes, the ready layers between the first and
// the last sharing the lanes between when there are more.

import { f2 } from './format.ts';

/** The most squares a note draws: 64 columns of four. A larger note draws as many, with its value on them. */
export const MAX_SQUARES = 256;

/** The most lanes the field draws. */
export const MAX_LANES = 8;

/** How many squares a note of `v` tokens draws. */
export const squaresOf = (v: number): number => Math.max(1, Math.min(MAX_SQUARES, Math.floor(v)));

/** Whether a note of `v` tokens draws fewer squares than it has tokens. */
export const capped = (v: number): boolean => v > MAX_SQUARES;

/** How many lanes a mint of `layers` layers draws. */
export const laneCount = (layers: number): number => Math.max(1, Math.min(layers, MAX_LANES));

/**
 * The lane a layer is drawn in: its own while the mint has no more layers
 * than {@link MAX_LANES}; past that, the first and the last keep theirs and
 * the ready layers between share the lanes between, in runs of equal length.
 */
export function laneIndex(pos: number, layers: number): number {
  const lanes = laneCount(layers);
  if (layers <= lanes) {
    return Math.max(0, Math.min(lanes - 1, pos));
  }
  if (pos <= 0) {
    return 0;
  }
  if (pos >= layers - 1) {
    return lanes - 1;
  }
  return 1 + Math.floor(((pos - 1) * (lanes - 2)) / (layers - 2));
}

/** The first layer of lane `lane`, its `pos`: the last layer for the last lane. */
export function laneLayer(lane: number, layers: number): number {
  const lanes = laneCount(layers);
  if (layers <= lanes || lane <= 0) {
    return lane;
  }
  if (lane >= lanes - 1) {
    return layers - 1;
  }
  return 1 + Math.ceil(((lane - 1) * (layers - 2)) / (lanes - 2));
}

/** A note as the field places it. */
export interface Placed {
  id: string;
  /** Its value: as many squares, up to {@link MAX_SQUARES}. */
  v: number;
  /** Its layer, which decides the lane it sits in. */
  pos: number;
  /** The order it came into its lane in, earliest first. */
  order: number;
}

/** What the field's width and the screen's height decide, before any pitch is chosen. */
export interface Base {
  /** A field 760 px wide or more: the desktop field. */
  desk: boolean;
  /** A field 480 to 759 px wide. */
  mid: boolean;
  /** A phone under 800 px tall. */
  short: boolean;
  /** A phone under 700 px tall. */
  tiny: boolean;
  /** The label column on the right. */
  labW: number;
  /** Where squares go: the width less the labels and the gap before them. */
  areaW: number;
  /** The starting pitch, its floor and its ceiling. */
  p0: number;
  pMin: number;
  pMax: number;
  /** A lane's padding above and below: to start, at least, at most. */
  pad0: number;
  padMin: number;
  padMax: number;
  /** The ruler band's height. */
  rulerH: number;
  /** A collapsed lane's height. */
  collapsed: number;
  /** The side of an empty cell's dot. */
  dot: number;
}

/** {@link Base} with a pitch and padding chosen. */
export interface Geometry extends Base {
  /** The pitch: square to square. */
  p: number;
  /** A square's side. */
  s: number;
  /** Rows a band. */
  R: number;
  /** A lane's padding above and below. */
  pad: number;
  /** A band's height. */
  bh: number;
  /** The gap between the bands of a lane. */
  bandGap: number;
  /** Columns a band. */
  cols: number;
}

/** Where a note starts in its lane: its column and band. */
export interface At {
  col: number;
  band: number;
}

/** One lane, placed. */
export interface Row {
  /** Its first layer: the layer, unless it shares the lane with others (see {@link laneIndex}). */
  pos: number;
  /** Its top and height within the field. */
  top: number;
  h: number;
  /** An empty lane drawn as a single row of dots. */
  collapsed: boolean;
  /** The top of its first band. */
  by: number;
  /** The height of its bands together. */
  blockH: number;
  bands: number;
  /** Rows a band in this lane: four, or fewer in a lane that shrinks to its tallest note. */
  nr: number;
  /** Where each of its notes starts. */
  at: Map<string, At>;
  /** Its notes, in the order they came into it. */
  list: Placed[];
}

/** The whole field, placed. */
export interface Plan {
  G: Geometry;
  rows: Row[];
  /** The field's height. */
  H: number;
  rulerH: number;
}

/** How a field is sized. */
export interface PlanOptions {
  /** The height to fill, if any. */
  target?: number | undefined;
  /** The main field: its height is the target, never less than its busiest state needs. */
  fill?: boolean;
  /** The layers notes sit in, which the main field is sized for. */
  busy?: ReadonlySet<number> | undefined;
  /** Draw the ruler. */
  ruler?: boolean;
  /**
   * Whether lanes may shrink (see {@link shrinks}); `false` keeps every lane
   * four rows tall and open, as the flows' slices of the field are drawn.
   */
  shrink?: boolean;
  /** The least gap between the bands of a lane: a slice opens its bands so a tag fits between them. */
  bandGap?: number;
}

/**
 * A note of `v` tokens as a block of squares at most four rows tall: 64 is
 * 16 x 4, 4 is 2 x 2, 1 is one square; one over {@link MAX_SQUARES} as that.
 */
export function shape(value: number): { w: number; h: number } {
  const v = squaresOf(value);
  const k = Math.log2(v);
  if (Number.isInteger(k)) {
    const h = Math.min(4, 2 ** Math.ceil(k / 2));
    return { w: Math.max(1, v / h), h };
  }
  const h = Math.min(4, v);
  return { w: Math.ceil(v / h), h };
}

/** Phone heights: short under 800 px tall, tiny under 700 (iPhone SE and smaller). */
export function geomBase(width: number, vh: number): Base {
  const desk = width >= 760;
  const mid = !desk && width >= 480;
  const short = !desk && vh < 800;
  const tiny = !desk && vh < 700;
  const labW = desk ? 178 : 84;
  return {
    desk,
    mid,
    short,
    tiny,
    labW,
    areaW: Math.max(120, width - labW - (desk ? 36 : 12)),
    p0: desk ? 13 : mid ? 9 : 7,
    pMin: desk ? 13 : mid ? 9 : 6,
    pMax: desk ? 17 : mid ? 15 : 8,
    pad0: desk ? 10 : mid ? 8 : short ? 7 : 9,
    padMin: desk ? (vh < 768 ? 2 : 3) : tiny ? 4 : 5,
    padMax: desk ? 30 : mid ? 36 : 22,
    rulerH: desk ? (vh < 768 ? 30 : vh < 900 ? 34 : 38) : tiny ? 28 : short ? 32 : 36,
    collapsed: desk ? 22 : 20,
    dot: desk ? 1.8 : mid ? 1.6 : 1.4,
  };
}

/** {@link Base} at pitch `p` and padding `pad`. */
export function geomAt(B: Base, p: number, pad: number): Geometry {
  const s = f2(p * (B.desk ? 0.78 : 0.74));
  const R = 4;
  return {
    ...B,
    p,
    s,
    R,
    pad,
    bh: (R - 1) * p + s,
    bandGap: Math.round(p * 1.4),
    cols: Math.floor((B.areaW - s) / p) + 1,
  };
}

/** The notes of lane `lane` of a mint of `layers` layers, in the order they came into it. */
export function laneNotes(notes: readonly Placed[], lane: number, layers: number): Placed[] {
  return notes.filter((note) => laneIndex(note.pos, layers) === lane).sort((a, b) => a.order - b.order);
}

/**
 * Notes run left to right, one empty column apart; a note that does not fit
 * wraps onto a new band of four rows, and one wider than the lane runs on
 * over as many bands as it needs, `cols` columns a band.
 */
export function laneLayout(list: readonly Placed[], G: Geometry): { at: Map<string, At>; bands: number } {
  let col = 0;
  let band = 0;
  const at = new Map<string, At>();
  for (const note of list) {
    const w = shape(note.v).w;
    if (col > 0 && col + w > G.cols) {
      band++;
      col = 0;
    }
    at.set(note.id, { col, band });
    if (w > G.cols) {
      band += Math.floor((w - 1) / G.cols);
      col = ((w - 1) % G.cols) + 2;
    } else {
      col += w + 1;
    }
  }
  return { at, bands: band + 1 };
}

/**
 * Whether lane `lane` of `lanes` shrinks when it can: an empty one collapses
 * to a row of dots, and one with a single band is as tall as its tallest
 * note. So do the fading lane and every ready lane but the last; the last
 * ready lane and the resting lane keep four rows, so the field holds still
 * as notes arrive.
 */
export function shrinks(lane: number, lanes: number): boolean {
  return lane === 0 || lane < lanes - 2;
}

/**
 * Places the field. The pitch never shrinks to fit a lane past its floor:
 * lanes wrap and grow instead. With a target height (the main field), the
 * pitch may grow while no lane wraps more, sized for every lane open, so it
 * stays the same as lanes open and close; the lanes' padding takes up the
 * rest.
 */
export function fieldPlan(
  width: number,
  vh: number,
  layers: number,
  notes: readonly Placed[],
  o: PlanOptions = {},
): Plan {
  const B = geomBase(width, vh);
  const geom = (p: number, pad: number): Geometry => {
    const g = geomAt(B, p, pad);
    return o.bandGap === undefined || o.bandGap <= g.bandGap ? g : { ...g, bandGap: o.bandGap };
  };
  // the lanes, by index: each row keeps its first layer as its `pos`
  const count = laneCount(layers);
  const lanes = Array.from({ length: count }, (_, i) => i);
  const lists = lanes.map((lane) => laneNotes(notes, lane, layers));
  const busy = o.busy === undefined ? undefined : new Set([...o.busy].map((layer) => laneIndex(layer, layers)));
  const listOf = (lane: number): Placed[] => lists[lane] ?? [];
  const shrinking = (lane: number): boolean => o.shrink !== false && shrinks(lane, count);
  const isCollapsed = (lane: number): boolean => listOf(lane).length === 0 && shrinking(lane);
  const bandsAt = (G: Geometry, lane: number): number => Math.max(laneLayout(listOf(lane), G).bands, 1);
  const laneRows = (list: readonly Placed[], G: Geometry): number =>
    list.length > 0 ? Math.max(...list.map((note) => shape(note.v).h)) : G.R;
  const blockH = (G: Geometry, lane: number, full: boolean): number => {
    const b = bandsAt(G, lane);
    const bh = !full && b === 1 && shrinking(lane) ? (laneRows(listOf(lane), G) - 1) * G.p + G.s : G.bh;
    return b * bh + (b - 1) * G.bandGap;
  };
  const rulerH = o.ruler === false ? 0 : B.rulerH;
  // worst: every lane open (true), or a set of the lanes to count as open
  const height = (G: Geometry, worst: boolean | ReadonlySet<number>): number =>
    rulerH +
    2 +
    lanes.reduce(
      (h, lane) =>
        h +
        (isCollapsed(lane) && (worst === false || (worst !== true && !worst.has(lane)))
          ? G.collapsed
          : blockH(G, lane, worst !== false) + 2 * G.pad),
      0,
    );
  // the pitch shrinks (never below its floor) only to keep every lane on one band; past that, lanes wrap
  let p0 = B.p0;
  const fits = (p: number): boolean => lanes.every((lane) => laneLayout(listOf(lane), geom(p, 0)).bands <= 1);
  if (!fits(p0)) {
    for (let q = f2(p0 - 0.1); q >= B.pMin - 1e-9; q = f2(q - 0.1)) {
      if (fits(q)) {
        p0 = q;
        break;
      }
    }
  }
  let G = geom(p0, B.pad0);
  let T = 0;
  if (o.target !== undefined) {
    const target = o.target;
    const base = lanes.map((lane) => bandsAt(G, lane));
    let p = p0;
    for (let q = B.pMax; q > p0; q--) {
      const g = geom(q, B.padMin);
      if (lanes.every((lane, i) => bandsAt(g, lane) <= (base[i] ?? 1)) && height(g, true) <= target) {
        p = q;
        break;
      }
    }
    // tablet widths: when padding alone cannot take up the slack, larger squares may wrap onto extra
    // bands, decided for every lane open, so the squares keep their size whichever lanes are collapsed
    if (B.mid && (target - height(geom(p, 0), true)) / (2 * lanes.length) > B.padMax) {
      for (let q = B.pMax; q > p; q--) {
        if (height(geom(q, B.pad0), true) <= target) {
          p = q;
          break;
        }
      }
    }
    G = geom(p, 0);
    const open = lanes.filter((lane) => !isCollapsed(lane));
    // the main field has one height for its busiest state: never less than that at the least padding,
    // and the whole target when the screen has room; the open lanes take up the rest
    T = o.fill === true ? Math.max(target, height(geom(p, B.padMin), busy ?? true)) : target;
    const padF = open.length > 0 ? (T - height(G, false)) / (2 * open.length) : B.pad0;
    G = geom(p, f2(Math.max(B.padMin, o.fill === true ? padF : Math.min(B.padMax, padF))));
  }
  const rows: Row[] = [];
  let y = rulerH;
  for (const lane of lanes) {
    const collapsed = isCollapsed(lane);
    const list = listOf(lane);
    const L = laneLayout(list, G);
    const bands = Math.max(L.bands, 1);
    const nr = bands === 1 && shrinking(lane) ? laneRows(list, G) : G.R;
    const bh = bands * ((nr - 1) * G.p + G.s) + (bands - 1) * G.bandGap;
    const h = collapsed ? G.collapsed : bh + 2 * G.pad;
    rows.push({ pos: laneLayer(lane, layers), top: y, h, collapsed, by: y + (h - bh) / 2, blockH: bh, bands, nr, at: L.at, list });
    y += h;
  }
  // the padding is rounded to the hundredth: the filled field still keeps its exact height
  return { G, rows, H: o.fill === true ? Math.max(y + 2, T) : y + 2, rulerH };
}

/** One square of a note, from the field's top left. */
export interface Square {
  x: number;
  y: number;
  /** The square's place in its note's block, column by column. */
  column: number;
  row: number;
}

/** Where a note's squares go, and the boxes around them, one a band. */
export interface Box {
  note: Placed;
  /** The note's first square: where its group is drawn from. */
  x: number;
  y: number;
  /** The width and height of the note's block on its first band. */
  w: number;
  h: number;
  /** Its squares, relative to (x, y). */
  squares: Square[];
  /** The boxes it covers, one a band, relative to the field. */
  parts: { x: number; y: number; w: number; h: number }[];
}

/** Places every square of every note: squares fill a column top to bottom, then the next. */
export function placeNotes(plan: Plan): Map<string, Box> {
  const { G } = plan;
  const boxes = new Map<string, Box>();
  for (const row of plan.rows) {
    const bandY = (band: number): number => row.by + band * ((row.nr - 1) * G.p + G.s + G.bandGap);
    for (const note of row.list) {
      const at = row.at.get(note.id);
      if (at === undefined) {
        continue;
      }
      const { h: rows } = shape(note.v);
      const x0 = at.col * G.p;
      const y0 = bandY(at.band);
      const squares: Square[] = [];
      const parts = new Map<number, { x0: number; x1: number; y0: number; y1: number }>();
      for (let i = 0; i < squaresOf(note.v); i++) {
        const column = Math.floor(i / rows);
        const r = i % rows;
        const across = at.col + column;
        const band = at.band + Math.floor(across / G.cols);
        const x = (across % G.cols) * G.p;
        const y = bandY(band) + r * G.p;
        squares.push({ x: x - x0, y: y - y0, column, row: r });
        const part = parts.get(band) ?? { x0: x, x1: x + G.s, y0: y, y1: y + G.s };
        part.x0 = Math.min(part.x0, x);
        part.x1 = Math.max(part.x1, x + G.s);
        part.y0 = Math.min(part.y0, y);
        part.y1 = Math.max(part.y1, y + G.s);
        parts.set(band, part);
      }
      const first = parts.get(at.band);
      boxes.set(note.id, {
        note,
        x: x0,
        y: y0,
        w: first === undefined ? G.s : first.x1 - first.x0,
        h: first === undefined ? G.s : first.y1 - first.y0,
        squares,
        parts: [...parts.values()].map((p) => ({ x: p.x0, y: p.y0, w: p.x1 - p.x0, h: p.y1 - p.y0 })),
      });
    }
  }
  return boxes;
}
