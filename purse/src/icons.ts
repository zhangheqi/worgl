// The flows' small icons, from the prototype (L864-878), drawn with DOM
// calls in the current colour; and a share icon of the same make. Each is
// hidden from assistive tech: the control it sits in carries the words.

import { s } from './dom.ts';

type Shape = ['path', string] | ['rect', number, number, number, number] | ['circle', number, number, number];

function icon(size: number, stroke: number, shapes: Shape[], options: { square?: boolean; cls?: string; height?: number } = {}): SVGSVGElement {
  const height = options.height ?? size;
  return s(
    'svg',
    {
      class: options.cls,
      width: options.cls === undefined ? size : undefined,
      height: options.cls === undefined ? height : undefined,
      viewBox: `0 0 ${size} ${height}`,
      fill: 'none',
      stroke: 'currentColor',
      'stroke-width': stroke,
      'stroke-linecap': options.square === true ? 'square' : undefined,
      'aria-hidden': 'true',
      focusable: 'false',
    },
    ...shapes.map((shape) => {
      if (shape[0] === 'path') {
        return s('path', { d: shape[1] });
      }
      if (shape[0] === 'rect') {
        return s('rect', { x: shape[1], y: shape[2], width: shape[3], height: shape[4] });
      }
      return s('circle', { cx: shape[1], cy: shape[2], r: shape[3] });
    }),
  );
}

/** The back arrow of a flow's top bar. */
export const backIcon = (): SVGSVGElement => icon(22, 1.6, [['path', 'M19 11H4M10 4.5 3.5 11l6.5 6.5']], { square: true });

/** Two sheets: copy. */
export const copyIcon = (): SVGSVGElement => icon(20, 1.5, [['rect', 6.5, 6.5, 10.5, 10.5], ['path', 'M13.5 3H3v10.5']]);

/** A tick: copied. */
export const checkIcon = (): SVGSVGElement => icon(20, 1.7, [['path', 'M3.5 10.5 8 15l8.5-9.5']], { square: true });

/** A viewfinder: scan. */
export const scanIcon = (): SVGSVGElement => icon(20, 1.5, [['path', 'M2.5 7V2.5H7M13 2.5h4.5V7M17.5 13v4.5H13M7 17.5H2.5V13M5.5 10h9']]);

/** A clipboard: paste. */
export const pasteIcon = (): SVGSVGElement =>
  icon(20, 1.5, [['path', 'M7 3.5H4v14h12v-14h-3'], ['rect', 7, 2, 6, 3], ['path', 'M7 10h6M7 13h4']]);

/** An arrow to the right. */
export const arrowIcon = (): SVGSVGElement => icon(16, 1.5, [['path', 'M1 6h13M9.5 1.5 14 6l-4.5 4.5']], { square: true, height: 12 });

/** An arrow down: Receive. */
export const downIcon = (): SVGSVGElement => icon(16, 1.5, [['path', 'M8 2v11M3.5 8.5 8 13l4.5-4.5']], { square: true });

/** An arrow up: Send. */
export const upIcon = (): SVGSVGElement => icon(16, 1.5, [['path', 'M8 14V3M3.5 7.5 8 3l4.5 4.5']], { square: true });

/** The registration mark of a flow's top bar. */
export const regIcon = (): SVGSVGElement => icon(14, 1, [['circle', 7, 7, 4.2], ['path', 'M7 0v14M0 7h14']], { cls: 'reg' });

/** An arrow out of a tray: share (Derived, in the same make as copy and paste). */
export const shareIcon = (): SVGSVGElement => icon(20, 1.5, [['path', 'M10 2.5v10M6 6.5l4-4 4 4'], ['path', 'M4.5 9.5v8h11v-8']], { square: true });
