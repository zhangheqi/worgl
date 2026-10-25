// Draws the field: one square per token, one lane per layer, first layer at
// the top, under a ruler of the current rotation with a needle for now. The
// geometry comes from layout.ts; this file turns it into SVG and the label
// overlay, opens the tips, follows the pointer, moves the needle and slides
// notes that changed lane: a note keep-alive moved a layer on slides from
// where it was, with a pink thread from there.

import { $, $$, type Child, announce, h, reduced, s } from './dom.ts';
import { f2, hm } from './format.ts';
import { type Holdings, type LaneKey, type Note, laneOf, laneTotal } from './holdings.ts';
import { type Box, type Plan, type Placed, type Row, capped, fieldPlan, laneIndex, placeNotes, shape, squaresOf } from './layout.ts';
import { closeIcon, square } from './marks.ts';
import { rng } from './rng.ts';
import { LANE_NAME, type Tip, laneDescription, laneTip, noteTip, tipWords } from './words.ts';

/** What tip is open: a lane's, or a note's. */
export type Opened = { lane: LaneKey } | { note: string } | undefined;

/** What the field needs to draw, and what to call when the person opens a tip. */
export interface FieldInput {
  holdings: Holdings;
  mintName: string;
  /** The open tip. */
  opened: Opened;
  /** Notes that just changed, by when: they are pink for six seconds. */
  pink: ReadonlyMap<string, number>;
  /** Whether the purse can keep these tokens alive now, for the tips' words. */
  keeps: boolean;
  /** Layers kept open though nothing is in them: those a keep-alive run passes through, so the field keeps its height. */
  busy?: ReadonlySet<number>;
  /** Notes keep-alive just moved a layer on, by when: a thread runs from where each was. */
  hopped?: ReadonlyMap<string, number>;
  /** Opens or closes a lane's tip. */
  toggleLane: (lane: LaneKey) => void;
  /** Opens or closes a note's tip, from a tap or a click. */
  toggleNote: (id: string) => void;
  /** Opens a note's tip, from the arrow keys on its lane's label. */
  showNote: (id: string) => void;
  /** Closes the open tip. */
  close: () => void;
}

/** What the last draw placed: for the needle, the pointer and the slides. */
interface Drawn {
  plan: Plan;
  boxes: Map<string, Box>;
  input: FieldInput;
}

const drawn = new WeakMap<HTMLElement, Drawn>();

/** Where each note was drawn last, so that a note that moved lanes slides from there. */
const lastPos = new Map<string, { x: number; y: number }>();

/** When each note was first drawn: one that just arrived slides in. */
const firstDrawn = new Map<string, number>();

/** How long pink lasts. */
const PINK_MS = 6000;

/** How soon after its hop a note's thread is drawn: the slide that goes with it is under a second. */
const THREAD_MS = 1500;

/** The notes whose thread was drawn: once, on the slide that follows the hop, never again as the lane reflows. */
const threaded = new Set<string>();

/** The lane a layer's notes and dots are drawn as. */
const laneKey = (pos: number, layers: number): LaneKey => laneOf(pos, layers);

/**
 * The field's leftover height in its paper band: what the band leaves, so
 * the page meets the bottom of the screen with no void; when the page
 * already scrolls, the field keeps its natural height.
 */
function fieldTarget(field: HTMLElement): number | undefined {
  const band = field.closest<HTMLElement>('.band-p');
  const wrap = band?.querySelector<HTMLElement>('.wrap');
  if (band === null || band === undefined || wrap === null || wrap === undefined) {
    return undefined;
  }
  const style = getComputedStyle(band);
  const natural = wrap.offsetHeight + Number.parseFloat(style.paddingTop) + Number.parseFloat(style.paddingBottom);
  const docH = document.documentElement.scrollHeight;
  return docH > window.innerHeight + 0.5 ? window.innerHeight - docH : band.clientHeight - natural;
}

/**
 * Draws the main field into `host`, filling the height its band leaves.
 * Focus on a lane's label or the tip's close button stays on its
 * counterpart in the new drawing, as when a resize draws the field again.
 */
export function drawMain(host: HTMLElement, input: FieldInput): void {
  const slot = $('.tip-slot');
  const active = document.activeElement;
  const inside = active instanceof HTMLElement && (host.contains(active) || slot?.contains(active) === true);
  const key = inside ? active.dataset['fk'] : undefined;
  // measured with a 1 px field, so the margins around it do not collapse through an empty box
  host.replaceChildren();
  host.style.height = '1px';
  slot?.replaceChildren();
  const t0 = fieldTarget(host);
  const target = t0 === undefined ? undefined : Math.max(0, t0 + 1);
  draw(host, input, target);
  if (key !== undefined) {
    $(`[data-fk="${CSS.escape(key)}"]`)?.focus({ preventScroll: true });
  }
}

function draw(host: HTMLElement, input: FieldInput, target: number | undefined): void {
  const { holdings } = input;
  const now = Date.now();
  const width = host.clientWidth || 328;
  const placed = order(holdings.notes);
  const schedule = holdings.schedule;
  const plan = fieldPlan(width, window.innerHeight, holdings.layers, placed, {
    target,
    fill: true,
    busy: new Set([...placed.map((note) => note.pos), ...(input.busy ?? [])]),
    ruler: schedule !== undefined,
  });
  const { G, rows, H, rulerH } = plan;
  const boxes = placeNotes(plan);
  const frac = schedule === undefined ? 0 : Math.min(1, Math.max(0, (now - schedule.startMs) / schedule.periodMs));
  const cx = f2(frac * G.areaW);
  const ry = rulerH - 12;
  const layers = holdings.layers;
  const byId = new Map(holdings.notes.map((note) => [note.id, note]));

  const svg = s('svg', {
    width,
    height: f2(H),
    viewBox: `0 0 ${width} ${f2(H)}`,
    'aria-hidden': 'true',
    focusable: 'false',
  });

  // lanes: unused cells as dots, the hatch on the last layer, the fading dust, perforations between
  for (const row of rows) {
    const key = laneKey(row.pos, layers);
    const occupied = new Set<string>();
    for (const note of row.list) {
      const at = row.at.get(note.id);
      if (at === undefined) {
        continue;
      }
      const rowsOf = shape(note.v).h;
      for (let i = 0; i < squaresOf(note.v); i++) {
        const across = at.col + Math.floor(i / rowsOf);
        occupied.add(`${at.band + Math.floor(across / G.cols)}:${across % G.cols}:${i % rowsOf}`);
      }
    }
    const dz = G.dot;
    const dh = dz / 2;
    let dots = '';
    if (row.collapsed) {
      const y = row.top + row.h / 2 - dh;
      for (let c = 0; c < G.cols; c++) {
        dots += square(c * G.p + G.s / 2 - dh, y, dz);
      }
    } else {
      for (let b = 0; b < row.bands; b++) {
        const bandY = row.by + b * (G.bh + G.bandGap);
        for (let c = 0; c < G.cols; c++) {
          for (let r = 0; r < row.nr; r++) {
            if (!occupied.has(`${b}:${c}:${r}`)) {
              dots += square(c * G.p + G.s / 2 - dh, bandY + r * G.p + G.s / 2 - dh, dz);
            }
          }
        }
      }
    }
    const lane = s('g', { class: 'f-lane', 'data-l': row.pos, 'data-lane': key });
    if (key === 'rest' && !row.collapsed) {
      let hatch = '';
      const ext = Math.max(0, Math.min(4, G.pad - 1));
      const y0 = row.by - ext;
      const hh = row.blockH + 2 * ext;
      const W = G.areaW;
      for (let x = -hh; x < W; x += 5) {
        hatch += `M${f2(Math.max(0, x))} ${f2(x < 0 ? y0 + hh + x : y0 + hh)}L${f2(Math.min(W, x + hh))} ${f2(x + hh > W ? y0 + (x + hh - W) : y0)}`;
      }
      lane.append(s('path', { class: 'f-hatch', d: hatch }));
    }
    lane.append(s('path', { class: 'f-dots', d: dots }));
    if (key === 'fade' && !row.collapsed && row.list.length > 0) {
      lane.append(...fadeTrail(plan, row));
    }
    svg.append(lane);
  }
  let perf = '';
  for (const row of rows.slice(0, -1)) {
    const y = row.top + row.h - 0.7;
    for (let x = 0; x < width; x += 5) {
      perf += square(x, y, 1.4);
    }
  }
  svg.append(s('path', { class: 'f-perf', d: perf }));

  // the needle's tail runs under the notes, knocked out behind them
  if (schedule !== undefined) {
    svg.append(
      s(
        'g',
        { class: 'f-needle', style: { transform: `translateX(${cx}px)` } },
        s('line', { class: 'tail', x1: 0, y1: ry + 7, x2: 0, y2: f2(H - 2) }),
      ),
    );
  }

  // the notes
  const notes = s('g', { class: 'f-notes' });
  for (const box of boxes.values()) {
    const note = byId.get(box.note.id);
    if (note === undefined) {
      continue;
    }
    const key = laneKey(note.pos, layers);
    const group = s('g', {
      class: 'f-note',
      'data-id': note.id,
      'data-l': note.pos,
      'data-lane': key,
      // for tests only: a note on its way a layer on is drawn as any other where it was
      'data-moving': note.moving ? 'true' : undefined,
      style: { transform: `translate(${f2(box.x)}px,${f2(box.y)}px)` },
    });
    if (schedule !== undefined) {
      // a knock-out behind each part, shown while the needle crosses it, so its tail never runs through a note
      for (const part of box.parts) {
        group.append(
          s('rect', {
            class: 'f-ko',
            x: f2(part.x - box.x - 2),
            y: f2(part.y - box.y - 2),
            width: f2(part.w + 4),
            height: f2(part.h + 4),
            'data-x0': f2(part.x - 3),
            'data-x1': f2(part.x + part.w + 3),
            style: { visibility: crossing(part.x, part.w, cx) ? 'visible' : 'hidden' },
          }),
        );
      }
    }
    if (key === 'fade') {
      group.append(...dissolve(box, plan));
    } else {
      const inset = key === 'rest' ? 0.5 : 0;
      let d = '';
      for (const sq of box.squares) {
        d += square(sq.x + inset, sq.y + inset, G.s - 2 * inset);
      }
      group.append(s('path', { class: key === 'rest' ? 'rs' : 'rd', d }));
    }
    if (capped(note.v)) {
      // a note too large to draw square by square: its block, and its value on it
      group.append(
        s('text', { class: 'f-val', x: f2(G.s * 0.4), y: f2(box.h / 2) }, String(note.v)),
      );
    }
    const pinkAt = input.pink.get(note.id);
    if (key !== 'fade' && pinkAt !== undefined && now - pinkAt < PINK_MS) {
      group.classList.add('pink');
      group.style.setProperty('--pd', `${f2(-(now - pinkAt) / 1000)}s`);
    }
    notes.append(group);
  }
  svg.append(notes);

  // the rotation as a ruler, with a needle at now
  const labels: Child[] = [];
  if (schedule !== undefined) {
    const ruler = rulerPaths(frac, G.areaW, ry);
    svg.append(
      s('path', { class: 'f-rul', d: `M0 ${ry}H${f2(G.areaW)}` }),
      s('path', { class: 'f-rul-done', d: ruler.line }),
      s('path', { class: 'f-tick', d: ruler.ticks }),
      s('path', { class: 'f-tick done', d: ruler.done }),
      s(
        'g',
        { class: 'f-needle', style: { transform: `translateX(${cx}px)` } },
        s('line', { x1: 0, y1: ry - 4, x2: 0, y2: ry + 7 }),
        s('rect', { x: -2.5, y: ry - 2.5, width: 5, height: 5 }),
      ),
    );
    const nl = nowLeft(frac, G.areaW);
    // just after a rotation the needle sits at the start: its label stands for both
    labels.push(
      h(
        'span',
        {
          class: 'f-time',
          'data-from': true,
          style: { left: '0', top: `${ry - 21}px`, visibility: nl < 96 ? 'hidden' : 'visible' },
        },
        hm(schedule.startMs),
        nl > 214 ? h('span', { class: 'f-rot' }, ' · this rotation') : null,
      ),
      h('span', { class: 'f-time now', 'data-now': true, style: { left: `${f2(nl)}px`, top: `${ry - 21}px` } }, `Now ${hm(now)}`),
      h('div', { class: 'f-next', style: { top: `${ry - 20}px` } }, h('span', { class: 'lab' }, 'Next'), h('b', {}, hm(schedule.nextMs))),
    );
  }

  // lane labels, as buttons; the ready layers share one label
  const opened = input.opened;
  const openLane = opened !== undefined && 'lane' in opened ? opened.lane : undefined;
  const readyOpen = rows.filter((row) => laneKey(row.pos, layers) === 'ready' && !row.collapsed);
  for (const row of rows) {
    const key = laneKey(row.pos, layers);
    if (row.collapsed) {
      if (key === 'fade') {
        labels.push(
          h(
            'div',
            { class: 'f-lab static dim', style: { top: `${f2(row.top + row.h / 2)}px` } },
            h('span', { class: 'lab' }, 'Fading'),
            h('b', {}, '0'),
          ),
        );
      }
      continue;
    }
    let top = row.top;
    let height = row.h;
    if (key === 'ready') {
      if (row !== readyOpen[0]) {
        continue;
      }
      const last = readyOpen[readyOpen.length - 1] ?? row;
      height = last.top + last.h - row.top;
    }
    const total = laneTotal(holdings, key);
    const fade = key === 'fade';
    const at = fade && schedule !== undefined ? `${G.desk ? '· ' : 'at '}${hm(schedule.nextMs)}` : '';
    const lab = h(
      'span',
      { class: `f-lab${fade ? ' fade' : ''}${total > 0 ? '' : ' dim'}` },
      h('span', { class: 'lab' }, LANE_NAME[key]),
      h('b', {}, total),
      fade && G.desk && at !== '' ? h('span', { class: 'at' }, at) : null,
    );
    const mid = top + height / 2;
    const isOpen = openLane === key;
    labels.push(
      h(
        'button',
        {
          type: 'button',
          class: `f-lanebtn${fade && !G.desk && at !== '' ? ' two' : ''}`,
          'data-lane': key,
          'data-fk': `lane-${key}`,
          'aria-expanded': isOpen ? 'true' : 'false',
          'aria-controls': isOpen ? 'field-tip' : undefined,
          'aria-label': laneDescription(holdings, key, now, input.keeps),
          'aria-keyshortcuts': laneTotal(holdings, key) > 0 ? 'ArrowLeft ArrowRight' : undefined,
          style: { top: `${f2(Math.max(rulerH - 2, mid - 22))}px`, height: '44px' },
          on: {
            click: () => {
              // a tap or click on a note under the button opened the note's tip already
              if (!fieldTook()) {
                input.toggleLane(key);
              }
            },
            keydown: (event) => {
              if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
                event.preventDefault();
                stepNote(rows, key, event.key === 'ArrowRight' ? 1 : -1, input);
              }
            },
          },
        },
        lab,
        fade && !G.desk && at !== '' ? h('span', { class: 'at' }, at) : null,
      ),
    );
  }

  // the open tip: on the phone in the flow below the field, so the lit lane stays in view
  let tipEl: HTMLElement | undefined;
  let selected: LaneKey | undefined;
  let tipRow: Row | undefined;
  if (opened !== undefined) {
    if ('lane' in opened) {
      selected = opened.lane;
      const rowsOfLane = rows.filter((row) => laneKey(row.pos, layers) === opened.lane);
      const open = rowsOfLane.filter((row) => !row.collapsed);
      tipRow = open[open.length - 1] ?? rowsOfLane[rowsOfLane.length - 1];
      tipEl = tipBox(laneTip(holdings, opened.lane, now, input.keeps), input.close);
    } else {
      const note = byId.get(opened.note);
      if (note !== undefined) {
        selected = laneKey(note.pos, layers);
        tipRow = rows[laneIndex(note.pos, layers)];
        tipEl = tipBox(noteTip(holdings, note, input.mintName, now, input.keeps), input.close);
      }
    }
  }
  const slot = $('.tip-slot');
  if (tipEl !== undefined && G.desk && tipRow !== undefined) {
    const mid = tipRow.top + tipRow.h / 2;
    tipEl.classList.add('at-r');
    tipEl.dataset['mid'] = String(f2(mid));
    tipEl.style.top = `${f2(mid + 24)}px`;
    labels.push(tipEl);
    slot?.replaceChildren();
  } else {
    slot?.replaceChildren(...(tipEl === undefined ? [] : [tipEl]));
  }
  if (selected === undefined) {
    host.removeAttribute('data-sel');
  } else {
    host.setAttribute('data-sel', selected);
  }
  $('.f-cap')?.classList.toggle('tip-on', tipEl !== undefined && G.desk);

  host.style.height = `${f2(H)}px`;
  host.replaceChildren(svg, h('div', { class: 'f-labs' }, ...labels));
  if (opened !== undefined && 'note' in opened) {
    $(`.f-note[data-id="${CSS.escape(opened.note)}"]`, host)?.classList.add('hov');
  }
  drawn.set(host, { plan, boxes, input });

  // a desktop tip that would run past the field opens above its lane label instead
  const deskTip = $('.f-tip.at-r', host);
  if (deskTip !== null && deskTip.offsetTop + deskTip.offsetHeight > H + 6) {
    deskTip.style.top = `${f2(Math.max(0, Number.parseFloat(deskTip.dataset['mid'] ?? '0') - 22 - deskTip.offsetHeight))}px`;
    $('.f-cap')?.classList.remove('tip-on');
  }

  slide(host, svg, boxes, now, width, input.hopped);
}

/** Notes in the order they came, earliest first; those the log does not know first, by serial. */
export function order(notes: readonly Note[]): Placed[] {
  const serial = (id: string): [number, number] => {
    const [generation, index] = id.split(':').map(Number);
    return [generation ?? 0, index ?? 0];
  };
  return [...notes]
    .sort((a, b) => {
      const at = (a.arrivedAt ?? -1) - (b.arrivedAt ?? -1);
      if (at !== 0) {
        return at;
      }
      const [ga, ia] = serial(a.id);
      const [gb, ib] = serial(b.id);
      return ga - gb || ia - ib;
    })
    .map((note, i) => ({ id: note.id, v: note.v, pos: note.pos, order: i }));
}

/** Whether the needle at `cx` crosses a part of a note at `x`, `w` wide. */
const crossing = (x: number, w: number, cx: number): boolean => cx >= x - 3 && cx <= x + w + 3;

/** The ruler up to the needle, and its eight ticks, those passed apart. */
function rulerPaths(frac: number, areaW: number, ry: number): { line: string; ticks: string; done: string } {
  let ticks = '';
  let done = '';
  for (let i = 0; i <= 8; i++) {
    const x = f2((i / 8) * areaW);
    const tick = `M${x} ${ry - (i % 8 === 0 ? 6 : i % 4 === 0 ? 4.5 : 3)}V${ry}`;
    if (i / 8 <= frac) {
      done += tick;
    } else {
      ticks += tick;
    }
  }
  return { line: `M0 ${ry}H${f2(frac * areaW)}`, ticks, done };
}

/** Where the "Now" label's centre goes: on the needle, kept clear of the ends. */
const nowLeft = (frac: number, areaW: number): number => Math.min(areaW - 34, Math.max(52, frac * areaW));

/**
 * FLIP: notes slide from where they were, and a note that just arrived
 * drops in from above. A note keep-alive just moved a layer on gets a pink
 * thread from its old place to its new, which fades as it lands; with less
 * motion, notes are placed and no thread is drawn.
 */
function slide(
  host: HTMLElement,
  svg: SVGSVGElement,
  boxes: Map<string, Box>,
  now: number,
  width: number,
  hopped: ReadonlyMap<string, number> | undefined,
): void {
  const moves: [SVGGElement, { x: number; y: number; fresh: boolean }, Box][] = [];
  const still = reduced();
  for (const el of $$<SVGGElement>('.f-note', host)) {
    const id = el.dataset['id'] ?? '';
    const box = boxes.get(id);
    if (box === undefined) {
      continue;
    }
    const last = lastPos.get(id);
    const first = firstDrawn.get(id);
    if (first === undefined) {
      firstDrawn.set(id, now);
    }
    if (still) {
      continue;
    }
    if (last !== undefined && (Math.abs(last.x - box.x) > 0.5 || Math.abs(last.y - box.y) > 0.5)) {
      moves.push([el, { ...last, fresh: false }, box]);
      const at = hopped?.get(id);
      if (at !== undefined && now - at < THREAD_MS && !threaded.has(id)) {
        threaded.add(id);
        const x0 = last.x + box.w + 3;
        const y0 = last.y + box.h / 2;
        const x1 = box.x + box.w + 3;
        const y1 = box.y + box.h / 2;
        // never past the field's right edge, at 360 px
        const xm = Math.min(Math.max(x0, x1) + 26, width - 2);
        svg.append(
          s('path', {
            class: 'f-thread',
            d: `M${f2(x0)} ${f2(y0)}C${f2(xm)} ${f2(y0)} ${f2(xm)} ${f2(y1)} ${f2(x1)} ${f2(y1)}`,
          }),
        );
      }
    } else if (last === undefined && first === undefined && arriving.has(id)) {
      moves.push([el, { x: box.x, y: box.y - 16, fresh: true }, box]);
    }
  }
  for (const [el, from] of moves) {
    el.style.transition = 'none';
    el.style.transform = `translate(${f2(from.x)}px,${f2(from.y)}px)`;
    if (from.fresh) {
      el.style.opacity = '0';
    }
  }
  if (moves.length > 0) {
    host.getBoundingClientRect();
    requestAnimationFrame(() => {
      for (const [el, , box] of moves) {
        el.style.transition = 'transform .9s cubic-bezier(.3,.7,.2,1), opacity .5s ease';
        el.style.transform = `translate(${f2(box.x)}px,${f2(box.y)}px)`;
        el.style.opacity = '';
      }
    });
  }
  lastPos.clear();
  for (const [id, box] of boxes) {
    lastPos.set(id, { x: box.x, y: box.y });
  }
  for (const id of firstDrawn.keys()) {
    if (!boxes.has(id)) {
      firstDrawn.delete(id);
    }
  }
  for (const id of threaded) {
    if (!boxes.has(id)) {
      threaded.delete(id);
    }
  }
  arriving.clear();
}

/** Notes that just came in this tab, which slide in the first time they are drawn. */
const arriving = new Set<string>();

/** Marks notes as just arrived, so the next draw slides them in. */
export function arrive(ids: readonly string[]): void {
  for (const id of ids) {
    arriving.add(id);
  }
}

/**
 * A note moved a layer on by keep-alive: the new note `newId` takes the
 * place the old one `oldId` was last drawn at, so the next draw slides it
 * from there rather than dropping it in.
 */
export function hop(oldId: string, newId: string): void {
  const last = lastPos.get(oldId);
  if (last !== undefined) {
    lastPos.set(newId, last);
    lastPos.delete(oldId);
  }
  const first = firstDrawn.get(oldId);
  if (first !== undefined) {
    firstDrawn.set(newId, first);
    firstDrawn.delete(oldId);
  }
}

/** Forgets where notes were, so the next draw places them without sliding (another mint, or a fresh view). */
export function forgetPositions(): void {
  lastPos.clear();
}

/** A tip box: its head, its lines, and the close cross. */
function tipBox(tip: Tip, close: () => void): HTMLElement {
  const [first, ...more] = tip.lines;
  return h(
    'div',
    { class: 'f-tip', id: 'field-tip', role: 'note', 'aria-label': tip.head },
    h('p', { class: 'lab' }, tip.head),
    first === undefined ? null : h('p', {}, first),
    ...more.map((line) => h('p', { class: 'm' }, line)),
    h(
      'button',
      { type: 'button', class: 'x', 'aria-label': 'Close', 'data-fk': 'tip-close', on: { click: close } },
      closeIcon(),
    ),
  );
}

/** A fading note comes apart into loose bits in the darkest ink; the right-hand bits first. */
export function dissolve(box: Box, plan: Plan): SVGElement[] {
  const { G } = plan;
  const v = box.note.v;
  const { w, h: rowsOf } = shape(v);
  const random = rng(v * 131 + 7);
  const k = G.desk ? 3 : 2;
  const sub = G.s / k;
  const bit = f2(sub * (G.desk ? 0.9 : 1));
  let d = '';
  const loose: SVGElement[] = [];
  box.squares.forEach((sq, i) => {
    const column = Math.floor(i / rowsOf);
    for (let a = 0; a < k; a++) {
      for (let b = 0; b < k; b++) {
        const x = sq.x + a * sub;
        const y = sq.y + b * sub;
        const q = random();
        const keep = 0.9 - 0.45 * ((column + a / k) / w);
        if (q < keep) {
          d += square(x, y, bit);
        } else if (q < keep + 0.24) {
          const dx = G.p * (0.3 + random() * 1.5);
          const dy = G.p * (random() - 0.5) * 0.7;
          const drift = random() < 0.7;
          const delay = f2(-random() * 3);
          const opacity = f2(0.55 + random() * 0.4);
          loose.push(
            s('rect', {
              class: drift ? 'lo lo-drift' : 'lo',
              x: f2(x + dx),
              y: f2(y + dy),
              width: bit,
              height: bit,
              style: { animationDelay: `${delay}s`, opacity: String(opacity) },
            }),
          );
        }
      }
    }
  });
  return [s('path', { class: 'fd', d }), ...loose];
}

/** Dust trails from the fading notes across the lane, thinning toward the mark where the lane fades. */
function fadeTrail(plan: Plan, row: Row): SVGElement[] {
  const { G } = plan;
  let x0 = 0;
  for (const note of row.list) {
    const at = row.at.get(note.id);
    if (at !== undefined) {
      x0 = Math.max(x0, Math.min(G.cols, at.col + shape(note.v).w) * G.p);
    }
  }
  const total = row.list.reduce((t, note) => t + squaresOf(note.v), 0);
  const random = rng(97 + total);
  const y0 = row.by;
  const hh = row.blockH;
  const bit = f2((G.s / (G.desk ? 3 : 2)) * (G.desk ? 0.9 : 1));
  const end = G.areaW - (G.desk ? 16 : 12);
  const span = Math.max(0, end - x0);
  const count = Math.round(span / (G.desk ? 9 : 6));
  const dust = s('g', { class: 'f-dust' });
  for (let j = 0; j < count; j++) {
    // a stream: dense by the notes, thinning and narrowing as it runs toward the mark
    const u = random() ** 2.6;
    const x = x0 + G.p * 0.4 + u * span;
    const y = y0 + hh / 2 + (random() - 0.5) * (hh - bit) * (1 - 0.6 * u) - bit / 2;
    const drift = random() < 0.6;
    const delay = f2(-random() * 3);
    dust.append(
      s('rect', {
        class: drift ? 'lo-drift' : undefined,
        x: f2(x),
        y: f2(y),
        width: bit,
        height: bit,
        style: { opacity: String(f2(0.95 - u * 0.65)), animationDelay: `${delay}s` },
      }),
    );
  }
  const ex = G.areaW - (G.desk ? 6 : 5);
  const ey = y0 + hh / 2;
  const m = G.desk ? 4.5 : 3.5;
  return [
    dust,
    s('path', {
      class: 'f-end',
      d: `M${f2(ex - m)} ${f2(ey - m)}h${f2(2 * m)}v${f2(2 * m)}h${f2(-2 * m)}zM${f2(ex - m)} ${f2(ey - m)}l${f2(2 * m)} ${f2(2 * m)}`,
    }),
  ];
}

/**
 * Moves the needle, the ruler behind it and the "Now" label to the time
 * now, and the knock-outs with it, without drawing the field again.
 */
export function moveNeedle(host: HTMLElement): void {
  const last = drawn.get(host);
  const schedule = last?.input.holdings.schedule;
  if (last === undefined || schedule === undefined) {
    return;
  }
  const now = Date.now();
  const { G, rulerH } = last.plan;
  const frac = Math.min(1, Math.max(0, (now - schedule.startMs) / schedule.periodMs));
  const cx = f2(frac * G.areaW);
  for (const needle of $$<SVGGElement>('.f-needle', host)) {
    needle.style.transform = `translateX(${cx}px)`;
  }
  const ruler = rulerPaths(frac, G.areaW, rulerH - 12);
  $('.f-rul-done', host)?.setAttribute('d', ruler.line);
  $('.f-tick:not(.done)', host)?.setAttribute('d', ruler.ticks);
  $('.f-tick.done', host)?.setAttribute('d', ruler.done);
  for (const ko of $$<SVGRectElement>('.f-ko', host)) {
    const x0 = Number(ko.dataset['x0']);
    const x1 = Number(ko.dataset['x1']);
    ko.style.visibility = cx >= x0 && cx <= x1 ? 'visible' : 'hidden';
  }
  const label = $('[data-now]', host);
  if (label !== null) {
    const nl = nowLeft(frac, G.areaW);
    label.textContent = `Now ${hm(now)}`;
    label.style.left = `${f2(nl)}px`;
    const from = $('[data-from]', host);
    if (from !== null) {
      from.style.visibility = nl < 96 ? 'hidden' : 'visible';
    }
  }
}

/** The note under a point of the field, within `slack` px of its squares. */
function noteAt(host: HTMLElement, x: number, y: number, slack: number): Box | undefined {
  const last = drawn.get(host);
  if (last === undefined) {
    return undefined;
  }
  let best: Box | undefined;
  let bestDistance = Number.POSITIVE_INFINITY;
  for (const box of last.boxes.values()) {
    for (const part of box.parts) {
      const dx = Math.max(part.x - x, 0, x - (part.x + part.w));
      const dy = Math.max(part.y - y, 0, y - (part.y + part.h));
      const distance = Math.hypot(dx, dy);
      if (distance <= slack && distance < bestDistance) {
        best = box;
        bestDistance = distance;
      }
    }
  }
  return best;
}

/** The hover tip: the note it is about, and one Escape dismissed until the pointer leaves that note. */
const hover: { tip: HTMLElement | undefined; last: string | undefined; dismissed: string | undefined } = {
  tip: undefined,
  last: undefined,
  dismissed: undefined,
};

/** Takes the hover tip away, and the outline of its note unless that note's tip is open. */
function hideHover(host: HTMLElement): void {
  hover.tip?.remove();
  hover.tip = undefined;
  for (const el of $$('.f-note.hov', host)) {
    if (el.dataset['id'] !== openNote(host)) {
      el.classList.remove('hov');
    }
  }
  hover.last = undefined;
}

/**
 * Escape, for the hover tip: it goes, and stays away while the pointer is
 * on that note. Whether there was one to dismiss.
 */
export function dismissHover(): boolean {
  const host = $('#field');
  if (host === null || hover.tip === undefined) {
    return false;
  }
  const id = hover.last;
  hideHover(host);
  hover.dismissed = id;
  return true;
}

/**
 * Hover on desktop: the note under the mouse is outlined and a small tip
 * follows it, below the pointer, or above it near the field's foot. A tap,
 * or a click, opens the note's tip as a lane's opens (a note of 1 is under
 * 5 px wide on a phone, so a finger gets more room than a mouse).
 */
export function bindPointer(host: HTMLElement): void {
  hover.tip = undefined;
  hover.last = undefined;
  hover.dismissed = undefined;
  host.addEventListener('pointermove', (event) => {
    const state = drawn.get(host);
    if (event.pointerType !== 'mouse' || state === undefined) {
      return;
    }
    // the lane buttons span the lane, over its notes, so the pointer reaches the notes through them; an open tip is in the way
    if ((event.target as Element | null)?.closest('.f-tip') != null) {
      hideHover(host);
      return;
    }
    const rect = host.getBoundingClientRect();
    const x = event.clientX - rect.left;
    const y = event.clientY - rect.top;
    const hit = noteAt(host, x, y, 2);
    if (hit === undefined || hit.note.id !== hover.dismissed) {
      hover.dismissed = undefined;
    }
    // no hover tip over a note whose own tip is open, or one Escape dismissed
    if (hit === undefined || hit.note.id === openNote(host) || hit.note.id === hover.dismissed) {
      hideHover(host);
      return;
    }
    if (hover.last !== hit.note.id) {
      hideHover(host);
      hover.last = hit.note.id;
      $(`.f-note[data-id="${CSS.escape(hit.note.id)}"]`, host)?.classList.add('hov');
      const note = state.input.holdings.notes.find((each) => each.id === hit.note.id);
      if (note === undefined) {
        return;
      }
      const words = noteTip(state.input.holdings, note, state.input.mintName, Date.now(), state.input.keeps);
      const [first, ...more] = words.lines;
      hover.tip = h(
        'div',
        { class: 'f-tip hover', 'aria-hidden': 'true' },
        h('p', { class: 'lab' }, words.head),
        first === undefined ? null : h('p', {}, first),
        ...more.map((line) => h('p', { class: 'm' }, line)),
      );
      $('.f-labs', host)?.append(hover.tip);
    }
    const tip = hover.tip;
    if (tip !== undefined) {
      tip.style.left = `${f2(Math.max(0, Math.min(rect.width - tip.offsetWidth - 4, x + 16)))}px`;
      // below the pointer, unless it would run past the field: then above it
      const below = y + 18;
      tip.style.top = `${f2(below + tip.offsetHeight > rect.height ? Math.max(0, y - 12 - tip.offsetHeight) : below)}px`;
    }
  });
  host.addEventListener('pointerleave', () => {
    hover.dismissed = undefined;
    hideHover(host);
  });
  host.addEventListener('pointerup', (event) => {
    const state = drawn.get(host);
    if (event.button > 0 || state === undefined) {
      return;
    }
    // a tap or click on a lane's label opens the lane's tip; anywhere else, the note nearest it
    if ((event.target as Element | null)?.closest('.f-tip, .f-lab') != null) {
      return;
    }
    const rect = host.getBoundingClientRect();
    const slack = event.pointerType === 'mouse' ? 2 : 12;
    const hit = noteAt(host, event.clientX - rect.left, event.clientY - rect.top, slack);
    if (hit === undefined && (event.target as Element | null)?.closest('.f-lanebtn') != null) {
      // no note near: the lane's button, which spans the lane, opens the lane's tip
      return;
    }
    tapped = Date.now();
    if (hit !== undefined) {
      state.input.toggleNote(hit.note.id);
    } else if (state.input.opened !== undefined) {
      state.input.close();
    }
  });
}

/**
 * The arrow keys on a lane's label: open the tip of the next (`by` 1) or
 * the previous (-1) note in the lane, in the order they are drawn, and read
 * it out, so every note can be reached without a pointer.
 */
function stepNote(rows: readonly Row[], lane: LaneKey, by: 1 | -1, input: FieldInput): void {
  const { holdings } = input;
  const ids = rows.filter((row) => laneKey(row.pos, holdings.layers) === lane).flatMap((row) => row.list.map((note) => note.id));
  if (ids.length === 0) {
    return;
  }
  const opened = input.opened;
  const at = opened !== undefined && 'note' in opened ? ids.indexOf(opened.note) : -1;
  const next = at < 0 ? (by > 0 ? 0 : ids.length - 1) : (at + by + ids.length) % ids.length;
  const id = ids[next];
  const note = holdings.notes.find((each) => each.id === id);
  if (id === undefined || note === undefined) {
    return;
  }
  input.showNote(id);
  announce(tipWords(noteTip(holdings, note, input.mintName, Date.now(), input.keeps)));
}

/** When a tap or a click last opened or closed a tip in the field: the click that follows is the field's own. */
let tapped = 0;

/** Whether the click now is the one that ends a tap the field already answered. */
export const fieldTook = (): boolean => Date.now() - tapped < 600;

/** The note whose tip is open in the field, if one is. */
function openNote(host: HTMLElement): string | undefined {
  const opened = drawn.get(host)?.input.opened;
  return opened !== undefined && 'note' in opened ? opened.note : undefined;
}
