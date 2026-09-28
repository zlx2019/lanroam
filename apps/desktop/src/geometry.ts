// Layout canvas geometry for the arrangement panel: fitting, snapping,
// overlaps and the seams where devices meet. Screen by screen, as the
// engine sees them; it has the final word (lanroam-core::layout checks
// every placement, lanroam-input::world decides crossings), this only
// previews what a drag will do.

import type { RectDto } from "./types";

/** Canvas to screen: scale, then offset */
export interface View {
  k: number;
  ox: number;
  oy: number;
}

/** A device's screens on the canvas, by fingerprint */
export interface Placed {
  id: string;
  screens: RectDto[];
}

/** Where two devices' screens meet */
export interface Seam {
  /** The device left of (or above) the line */
  a: string;
  /** The device right of (or below) it */
  b: string;
  /** `v`: a vertical line, the devices side by side; `h`: a horizontal
   * one, the devices stacked */
  line: "v" | "h";
  /** Where the line lies */
  at: number;
  /** The stretch of it both screens share */
  from: number;
  to: number;
}

/** A snapped position, with the lines it snapped to (canvas units) */
export interface Snapped {
  x: number;
  y: number;
  /** A vertical guide line, if x snapped */
  gx: number | null;
  /** A horizontal guide line, if y snapped */
  gy: number | null;
}

/** How close (in canvas units) two edges count as touching */
const TOUCH = 1;

/** The rectangle around `rects` (they must not be empty) */
export function bounds(rects: RectDto[]): RectDto {
  const x = Math.min(...rects.map((r) => r.x));
  const y = Math.min(...rects.map((r) => r.y));
  const right = Math.max(...rects.map((r) => r.x + r.w));
  const bottom = Math.max(...rects.map((r) => r.y + r.h));
  return { x, y, w: right - x, h: bottom - y };
}

/** `rects` moved by (`dx`, `dy`) */
export function shift(rects: RectDto[], dx: number, dy: number): RectDto[] {
  return rects.map((r) => ({ ...r, x: r.x + dx, y: r.y + dy }));
}

/** Scale and offset that fit `rects` into a `w`×`h` area */
export function fit(rects: RectDto[], w: number, h: number, padding: number, maxK: number): View | null {
  if (!rects.length || !w || !h) return null;
  const all = bounds(rects);
  const k = Math.min((w - 2 * padding) / all.w, (h - 2 * padding) / all.h, maxK);
  return {
    k,
    ox: (w - all.w * k) / 2 - all.x * k,
    oy: (h - all.h * k) / 2 - all.y * k,
  };
}

/** Whether two rectangles share more than a boundary */
export function overlaps(a: RectDto, b: RectDto): boolean {
  return (
    a.x < b.x + b.w - TOUCH &&
    b.x < a.x + a.w - TOUCH &&
    a.y < b.y + b.h - TOUCH &&
    b.y < a.y + a.h - TOUCH
  );
}

/** Whether any of `mine` overlaps any of `others` */
export function clash(mine: RectDto[], others: RectDto[]): boolean {
  return mine.some((m) => others.some((o) => overlaps(m, o)));
}

/** Pull `box`, dropped at (`x`, `y`), onto the nearby edges of `others`
 * (and level with their sides) when within `reach` canvas units */
export function snap(box: RectDto, x: number, y: number, others: RectDto[], reach: number): Snapped {
  let bx: [number, number] | null = null;
  let by: [number, number] | null = null;
  for (const o of others) {
    // Each candidate: where the box goes, and the line it then lines up on
    const xs: [number, number][] = [
      [o.x + o.w, o.x + o.w],
      [o.x - box.w, o.x],
      [o.x, o.x],
      [o.x + o.w - box.w, o.x + o.w],
    ];
    const ys: [number, number][] = [
      [o.y + o.h, o.y + o.h],
      [o.y - box.h, o.y],
      [o.y, o.y],
      [o.y + o.h - box.h, o.y + o.h],
    ];
    for (const c of xs) {
      if (Math.abs(c[0] - x) < reach && (!bx || Math.abs(c[0] - x) < Math.abs(bx[0] - x))) bx = c;
    }
    for (const c of ys) {
      if (Math.abs(c[0] - y) < reach && (!by || Math.abs(c[0] - y) < Math.abs(by[0] - y))) by = c;
    }
  }
  return {
    x: Math.round(bx ? bx[0] : x),
    y: Math.round(by ? by[0] : y),
    gx: bx ? bx[1] : null,
    gy: by ? by[1] : null,
  };
}

/** Every stretch where screens of two different devices touch */
export function seams(devices: Placed[]): Seam[] {
  const out: Seam[] = [];
  for (const da of devices) {
    for (const db of devices) {
      if (da === db) continue;
      for (const a of da.screens) {
        for (const b of db.screens) {
          if (Math.abs(a.x + a.w - b.x) <= TOUCH) {
            const from = Math.max(a.y, b.y);
            const to = Math.min(a.y + a.h, b.y + b.h);
            if (to > from) out.push({ a: da.id, b: db.id, line: "v", at: b.x, from, to });
          }
          if (Math.abs(a.y + a.h - b.y) <= TOUCH) {
            const from = Math.max(a.x, b.x);
            const to = Math.min(a.x + a.w, b.x + b.w);
            if (to > from) out.push({ a: da.id, b: db.id, line: "h", at: b.y, from, to });
          }
        }
      }
    }
  }
  return out;
}

/** The pairs of devices that meet somewhere, once each (the first seam
 * between them tells how) */
export function pairs(all: Seam[]): Seam[] {
  const seen = new Set<string>();
  return all.filter((s) => {
    const key = [s.a, s.b].sort().join("|");
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/** Index of a device's primary screen: the one at its origin */
export function primaryIndex(screens: RectDto[], origin: { x: number; y: number } | null): number {
  if (!origin) return 0;
  const i = screens.findIndex((s) => Math.abs(s.x - origin.x) < 1 && Math.abs(s.y - origin.y) < 1);
  return Math.max(i, 0);
}
