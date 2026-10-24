// A slice of the field for the flows: the Ready lane, the Resting lane or
// both, drawn as Holdings draws its field (layout.ts places it), with no
// ruler and nothing to point at. What a flow shows on it, the note chosen,
// what went and what came back, are marks and tags; every fact a slice
// shows is also in the words beside it, so the host is hidden from
// assistive tech.

import { type Child, h, s } from './dom.ts';
import { dissolve } from './field.ts';
import { f2 } from './format.ts';
import { type Box, type Placed, fieldPlan, placeNotes } from './layout.ts';
import { square } from './marks.ts';

/** The two lanes a slice may draw. */
export type SliceLane = 'ready' | 'rest';

/** A note on a slice: its lane, whether it fades (drawn dissolving in the Ready lane), and its order in its lane. */
export interface SliceNote {
  id: string;
  v: number;
  lane: SliceLane;
  fading?: boolean;
  order: number;
}

/** A tag over a note (or under its lane): ink, outlined, or pink. */
export interface SliceTag {
  at: string;
  text: string;
  kind: 'ink' | 'out' | 'pink';
  below?: boolean;
}

/** What a slice draws. */
export interface SliceOptions {
  lanes: SliceLane[];
  notes: SliceNote[];
  /** Notes drawn as dashed outlines (what is missing, or what went), or outlined at the corners (what is chosen). */
  marks?: ReadonlyMap<string, 'ghost' | 'outline'>;
  /** Notes that just came, by when: pink for six seconds. */
  pink?: ReadonlyMap<string, number>;
  tags?: SliceTag[];
  /** Laid out but not drawn: the change still to come, so that nothing moves when it lands. */
  reserve?: SliceNote[];
}

const LANE_NAME: Record<SliceLane, string> = { ready: 'Ready', rest: 'Resting' };
const PINK_MS = 6000;

/**
 * The gap between a lane's bands when a tag stands between them: the tag
 * (about 18 px tall, its foot 7 px over its note) clears the band above.
 */
const TAG_ROOM = 28;

/**
 * The note a tag over the field names: of the notes marked as its own is,
 * the one that sits highest, then leftmost, so the tag never stands over a
 * note it does not name. Without a mark, the note itself.
 */
function tagged(tag: SliceTag, boxes: ReadonlyMap<string, Box>, marks: ReadonlyMap<string, 'ghost' | 'outline'>, lane: (id: string) => SliceLane | undefined): string {
  const mark = marks.get(tag.at);
  if (mark === undefined || tag.below === true) {
    return tag.at;
  }
  let best = tag.at;
  let top = boxes.get(tag.at);
  for (const [id, box] of boxes) {
    if (marks.get(id) !== mark || lane(id) !== lane(tag.at)) {
      continue;
    }
    if (top === undefined || box.y < top.y - 0.5 || (Math.abs(box.y - top.y) <= 0.5 && box.x < top.x)) {
      best = id;
      top = box;
    }
  }
  return best;
}

/** The options each slice was last drawn with, and the observer that draws it again when its width changes. */
const drawn = new WeakMap<HTMLElement, { options: SliceOptions; observer: ResizeObserver; width: number }>();

/** Draws a slice into `host`, and again whenever its width changes. */
export function drawSlice(host: HTMLElement, options: SliceOptions): void {
  const was = drawn.get(host);
  draw(host, options);
  if (was !== undefined) {
    was.options = options;
    was.width = host.clientWidth;
    return;
  }
  const entry = {
    options,
    width: host.clientWidth,
    observer: new ResizeObserver(() => {
      const known = drawn.get(host);
      if (known === undefined || !host.isConnected) {
        entry.observer.disconnect();
        return;
      }
      if (Math.abs(host.clientWidth - known.width) > 1) {
        known.width = host.clientWidth;
        draw(host, known.options);
      }
    }),
  };
  drawn.set(host, entry);
  entry.observer.observe(host);
}

/** The four corner strokes round a chosen note (L2098-2100). */
function outline(box: Box): string {
  const m = 4;
  const L = 6;
  const x0 = f2(box.x - m);
  const y0 = f2(box.y - m);
  const x1 = f2(box.x + box.w + m);
  const y1 = f2(box.y + box.h + m);
  return `M${x0} ${f2(y0 + L)}V${y0}H${f2(x0 + L)}M${f2(x1 - L)} ${y0}H${x1}V${f2(y0 + L)}M${x1} ${f2(y1 - L)}V${y1}H${f2(x1 - L)}M${f2(x0 + L)} ${y1}H${x0}V${f2(y1 - L)}`;
}

function draw(host: HTMLElement, options: SliceOptions): void {
  const { lanes } = options;
  const now = Date.now();
  const width = host.clientWidth || 328;
  const reserve = options.reserve ?? [];
  const reserved = new Set(reserve.map((note) => note.id));
  const marks = options.marks ?? new Map<string, 'ghost' | 'outline'>();
  const index = (lane: SliceLane): number => Math.max(0, lanes.indexOf(lane));
  const notes = options.notes.filter((note) => lanes.includes(note.lane));
  const byId = new Map(notes.map((note) => [note.id, note]));
  // the change still to come goes after every note in its lane, as it will land
  const last = Math.max(0, ...notes.map((note) => note.order)) + 1;
  const placed: Placed[] = [
    ...notes.map((note) => ({ id: note.id, v: note.v, pos: index(note.lane), order: note.order })),
    ...reserve.filter((note) => lanes.includes(note.lane)).map((note) => ({ id: note.id, v: note.v, pos: index(note.lane), order: last + note.order })),
  ];
  let plan = fieldPlan(width, window.innerHeight, lanes.length, placed, { ruler: false, shrink: false });
  let boxes = placeNotes(plan);
  const laneOfId = (id: string): SliceLane | undefined => byId.get(id)?.lane;
  // a tag over a note on a lane's later band stands between the bands: open them for it
  const later = (options.tags ?? []).some((tag) => {
    const box = boxes.get(tagged(tag, boxes, marks, laneOfId));
    const row = plan.rows[index(laneOfId(tag.at) ?? 'ready')];
    return tag.below !== true && box !== undefined && row !== undefined && box.y > row.by + 0.5;
  });
  if (later) {
    plan = fieldPlan(width, window.innerHeight, lanes.length, placed, { ruler: false, shrink: false, bandGap: TAG_ROOM });
    boxes = placeNotes(plan);
  }
  const { G, rows, H } = plan;
  const svg = s('svg', { width, height: f2(H), viewBox: `0 0 ${width} ${f2(H)}`, 'aria-hidden': 'true', focusable: 'false' });

  // lanes: unused cells as dots (the reserved ones too, until their change lands), the hatch on the resting lane
  rows.forEach((row, i) => {
    const lane = lanes[i] ?? 'ready';
    const taken = new Set<string>();
    for (const note of row.list) {
      const box = boxes.get(note.id);
      if (box === undefined || reserved.has(note.id)) {
        continue;
      }
      for (const sq of box.squares) {
        taken.add(`${f2(box.x + sq.x)}:${f2(box.y + sq.y)}`);
      }
    }
    const dz = G.dot;
    const dh = dz / 2;
    let dots = '';
    for (let b = 0; b < row.bands; b++) {
      const bandY = row.by + b * (G.bh + G.bandGap);
      for (let c = 0; c < G.cols; c++) {
        for (let r = 0; r < row.nr; r++) {
          const x = c * G.p;
          const y = bandY + r * G.p;
          if (!taken.has(`${f2(x)}:${f2(y)}`)) {
            dots += square(x + G.s / 2 - dh, y + G.s / 2 - dh, dz);
          }
        }
      }
    }
    const group = s('g', { class: 'f-lane', 'data-lane': lane });
    if (lane === 'rest') {
      let hatch = '';
      const ext = Math.max(0, Math.min(4, G.pad - 1));
      const y0 = row.by - ext;
      const hh = row.blockH + 2 * ext;
      const W = G.areaW;
      for (let x = -hh; x < W; x += 5) {
        hatch += `M${f2(Math.max(0, x))} ${f2(x < 0 ? y0 + hh + x : y0 + hh)}L${f2(Math.min(W, x + hh))} ${f2(x + hh > W ? y0 + (x + hh - W) : y0)}`;
      }
      group.append(s('path', { class: 'f-hatch', d: hatch }));
    }
    group.append(s('path', { class: 'f-dots', d: dots }));
    svg.append(group);
  });
  let perf = '';
  for (const row of rows.slice(0, -1)) {
    const y = row.top + row.h - 0.7;
    for (let x = 0; x < width; x += 5) {
      perf += square(x, y, 1.4);
    }
  }
  svg.append(s('path', { class: 'f-perf', d: perf }));

  // the notes, and the corners round those chosen
  const group = s('g', { class: 'f-notes' });
  let corners = '';
  for (const [id, box] of boxes) {
    const note = byId.get(id);
    if (note === undefined) {
      continue;
    }
    const mark = marks.get(id);
    const g = s('g', {
      class: `f-note${mark === 'ghost' ? ' ghost' : ''}`,
      'data-id': id,
      'data-lane': note.lane,
      style: { transform: `translate(${f2(box.x)}px,${f2(box.y)}px)` },
    });
    if (note.fading === true && mark !== 'ghost') {
      g.append(...dissolve(box, plan));
    } else {
      const inset = note.lane === 'rest' ? 0.5 : 0;
      let d = '';
      for (const sq of box.squares) {
        d += square(sq.x + inset, sq.y + inset, G.s - 2 * inset);
      }
      g.append(s('path', { class: note.lane === 'rest' ? 'rs' : 'rd', d }));
    }
    const pinkAt = options.pink?.get(id);
    if (mark !== 'ghost' && note.fading !== true && pinkAt !== undefined && now - pinkAt < PINK_MS) {
      g.classList.add('pink');
      g.style.setProperty('--pd', `${f2(-(now - pinkAt) / 1000)}s`);
    }
    if (mark === 'outline') {
      corners += outline(box);
    }
    group.append(g);
  }
  svg.append(group);
  if (corners !== '') {
    svg.append(s('path', { class: 'f-mark', d: corners }));
  }

  // one label a lane, with the lane's real notes counted; then the tags
  const labels: Child[] = rows.map((row, i) => {
    const lane = lanes[i] ?? 'ready';
    const total = row.list
      .filter((note) => !reserved.has(note.id) && marks.get(note.id) !== 'ghost' && byId.has(note.id))
      .reduce((sum, note) => sum + note.v, 0);
    return h(
      'div',
      { class: `f-lab static${total > 0 ? '' : ' dim'}`, style: { top: `${f2(row.top + row.h / 2)}px` } },
      h('span', { class: 'lab' }, LANE_NAME[lane]),
      h('b', {}, String(total)),
    );
  });
  for (const tag of options.tags ?? []) {
    const box = boxes.get(tagged(tag, boxes, marks, laneOfId));
    if (box === undefined) {
      continue;
    }
    const kind = tag.kind === 'ink' ? '' : ` ${tag.kind}`;
    if (tag.below === true) {
      const row = rows[index(byId.get(tag.at)?.lane ?? 'rest')];
      const right = box.x > G.areaW - 90;
      labels.push(
        h(
          'span',
          {
            class: `f-tag${kind} below${right ? ' r' : ''}`,
            style: { left: `${f2(right ? box.x + box.w : box.x)}px`, top: `${f2((row?.by ?? box.y) + (row?.blockH ?? box.h) + 4)}px` },
          },
          tag.text,
        ),
      );
    } else {
      // over the note it names (L2090): on a later band, in the gap opened above it
      const right = box.x > G.areaW - 90;
      labels.push(
        h(
          'span',
          {
            class: `f-tag${kind}${right ? ' r' : ''}`,
            style: { left: `${f2(right ? box.x + box.w + 4 : box.x - 4)}px`, top: `${f2(box.y - 7)}px` },
          },
          tag.text,
        ),
      );
    }
  }
  host.style.height = `${f2(H)}px`;
  host.replaceChildren(svg, h('div', { class: 'f-labs' }, ...labels));
}
