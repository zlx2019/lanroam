// Layout canvas geometry for the editor: fitting, snapping, overlaps and
// touching edges. The engine has the final word (lanroam-core::layout
// checks every placement, lanroam-input::world decides crossings); this only
// previews what a drag will do.

import type { RectDto } from "./types";

/** Canvas to screen: scale, then offset */
export interface View {
  k: number;
  ox: number;
  oy: number;
}

/** A device's box on the canvas, by fingerprint */
export interface Box extends RectDto {
  id: string;
}

/** Where two devices touch: `a` left of (or above) `b` */
export interface Touch {
  a: Box;
  b: Box;
  /** `h`: a vertical boundary (side by side); `v`: a horizontal one */
  dir: "h" | "v";
  /** The boundary's position */
  at: number;
  /** Where along the boundary the two overlap */
  from: number;
  to: number;
}

/** How close (in canvas units) two edges count as touching */
const TOUCH = 1;

/** Scale and offset that fit `boxes` into a `w`×`h` area */
export function fit(boxes: RectDto[], w: number, h: number, padding: number, maxK: number): View | null {
  if (!boxes.length || !w || !h) return null;
  const minX = Math.min(...boxes.map((r) => r.x));
  const minY = Math.min(...boxes.map((r) => r.y));
  const maxX = Math.max(...boxes.map((r) => r.x + r.w));
  const maxY = Math.max(...boxes.map((r) => r.y + r.h));
  const k = Math.min((w - 2 * padding) / (maxX - minX), (h - 2 * padding) / (maxY - minY), maxK);
  return {
    k,
    ox: (w - (maxX - minX) * k) / 2 - minX * k,
    oy: (h - (maxY - minY) * k) / 2 - minY * k,
  };
}

/** Whether two boxes share more than a boundary */
export function overlaps(a: RectDto, b: RectDto): boolean {
  return (
    a.x < b.x + b.w - TOUCH &&
    b.x < a.x + a.w - TOUCH &&
    a.y < b.y + b.h - TOUCH &&
    b.y < a.y + a.h - TOUCH
  );
}

/** Pull `box`, dropped at (`x`, `y`), onto the nearby edges of `others`
 * (and level with their sides) when within `reach` canvas units */
export function snap(box: RectDto, x: number, y: number, others: RectDto[], reach: number) {
  let bx: number | null = null;
  let by: number | null = null;
  for (const o of others) {
    for (const c of [o.x + o.w, o.x - box.w, o.x, o.x + o.w - box.w]) {
      if (Math.abs(c - x) < reach && (bx === null || Math.abs(c - x) < Math.abs(bx - x))) bx = c;
    }
    for (const c of [o.y + o.h, o.y - box.h, o.y, o.y + o.h - box.h]) {
      if (Math.abs(c - y) < reach && (by === null || Math.abs(c - y) < Math.abs(by - y))) by = c;
    }
  }
  return { x: Math.round(bx ?? x), y: Math.round(by ?? y) };
}

/** Every pair of boxes whose sides touch with some overlap */
export function touches(boxes: Box[]): Touch[] {
  const out: Touch[] = [];
  for (const a of boxes) {
    for (const b of boxes) {
      if (a === b) continue;
      if (Math.abs(a.x + a.w - b.x) <= TOUCH) {
        const from = Math.max(a.y, b.y);
        const to = Math.min(a.y + a.h, b.y + b.h);
        if (to > from) out.push({ a, b, dir: "h", at: b.x, from, to });
      }
      if (Math.abs(a.y + a.h - b.y) <= TOUCH) {
        const from = Math.max(a.x, b.x);
        const to = Math.min(a.x + a.w, b.x + b.w);
        if (to > from) out.push({ a, b, dir: "v", at: b.y, from, to });
      }
    }
  }
  return out;
}

/** The devices touching `id`, with the side they are on */
export function neighbours(id: string, all: Touch[]): { side: "left" | "right" | "top" | "bottom"; id: string }[] {
  const out: { side: "left" | "right" | "top" | "bottom"; id: string }[] = [];
  for (const t of all) {
    if (t.a.id === id) out.push({ side: t.dir === "h" ? "right" : "bottom", id: t.b.id });
    if (t.b.id === id) out.push({ side: t.dir === "h" ? "left" : "top", id: t.a.id });
  }
  return out;
}
