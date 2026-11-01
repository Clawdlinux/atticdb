/*
 * The tape as a list of flat shapes. The canvas draws them every frame; the build-time SVG
 * poster draws the same list once. Colours are token names, resolved by each renderer.
 */

import { LAY_MS, LOST_MS, PULSE_MS, TRAVEL_MS, lastGroup, type TapeState } from "./tape-sim";

export type Tone = "ink" | "muted" | "accent" | "bg";
export type Layer = "left" | "tape";

export type Prim =
  | { k: "rect"; layer: Layer; x: number; y: number; w: number; h: number; c: Tone; a: number }
  | { k: "dot"; layer: Layer; x: number; y: number; r: number; c: Tone; a: number }
  | {
      k: "text"; layer: Layer; x: number; y: number; s: string; c: Tone; a: number;
      size: number; align: "left" | "center" | "right"; num?: boolean;
    };

export type Mode = "wide" | "narrow";

// Canvas heights in CSS px, mirrored in Hero.module.css (--tape-h).
export const TAPE_H: Record<Mode, number> = { wide: 196, narrow: 204 };
// The width under which the band stacks the writers above the tape (48rem).
export const NARROW_BELOW = 768;

const APPS = ["Claude Code", "Copilot CLI", "VS Code", "Codex"];

export function laneLabel(i: number, lanes: number): string {
  const app = APPS[i % APPS.length];
  return lanes > APPS.length && i >= APPS.length ? `${app} ${Math.floor(i / APPS.length) + 1}` : app;
}

// The same margin as .container: min(80rem, 100% - 2 * clamp(1rem, 4vw, 4rem)), centred.
export function gutterFor(width: number, viewport = width): number {
  const pad = Math.min(64, Math.max(16, viewport * 0.04));
  const inner = Math.min(1280, width - 2 * pad);
  return (width - inner) / 2;
}

// cubic-bezier(0.2, 0, 0, 1), the rigid slide.
export function slide(t: number): number {
  const x = Math.min(1, Math.max(0, t));
  // Solve the curve's x for its parameter, then read y. Newton steps converge fast here.
  let u = x;
  for (let i = 0; i < 6; i += 1) {
    const bx = 3 * (1 - u) * (1 - u) * u * 0.2 + 3 * (1 - u) * u * u * 0 + u * u * u - x;
    const dx = 3 * (1 - u) * (1 - u) * 0.2 + 6 * (1 - u) * u * (0 - 0.2) + 3 * u * u * (1 - 0);
    if (Math.abs(dx) < 1e-6) break;
    u = Math.min(1, Math.max(0, u - bx / dx));
  }
  return 3 * (1 - u) * u * u * 1 + u * u * u;
}

const easeIn = (t: number) => t * t * t;

export type Geometry = {
  mode: Mode;
  w: number;
  h: number;
  // Left edge of the content column, so lanes line up with the headline's container.
  g: number;
  laneCols: number;
  laneRows: number;
  laneColW: number;
  rowH: number;
  lanesTop: number;
  visibleLanes: number;
  qx: number;
  qCap: number;
  slotW: number;
  slotH: number;
  tx: number;
  trackY: number;
  trackH: number;
  frameY: number;
  frameH: number;
  // Where the tape head rests, measured from the right edge.
  anchorFromRight: number;
  label: number;
};

export function geometry(mode: Mode, w: number, lanes: number, g = gutterFor(w)): Geometry {
  const h = TAPE_H[mode];
  if (mode === "wide") {
    const laneCols = lanes > 8 ? 3 : 1;
    const laneRows = Math.ceil(lanes / laneCols);
    const laneColW = 148;
    const lanesTop = 40;
    const rowH = Math.min(30, (h - 24 - lanesTop) / laneRows);
    const qx = g + laneCols * laneColW + 28;
    const qCap = 8;
    const slotW = 12;
    const tx = qx + qCap * (slotW + 4) + 40;
    const trackY = 70;
    const trackH = 64;
    return {
      mode, w, h, g, laneCols, laneRows, laneColW, rowH, lanesTop, visibleLanes: lanes,
      qx, qCap, slotW, slotH: 18, tx, trackY, trackH, frameY: trackY + 8, frameH: 42,
      anchorFromRight: Math.min(240, (w - tx) * 0.3), label: 14,
    };
  }
  const visibleLanes = Math.min(lanes, 4);
  const laneCols = visibleLanes > 1 ? 2 : 1;
  const laneRows = Math.ceil(visibleLanes / laneCols);
  const qx = g;
  const qCap = 5;
  const slotW = 12;
  const tx = qx + qCap * (slotW + 4) + 22;
  const trackY = 100;
  const trackH = 72;
  return {
    mode, w, h, g, laneCols, laneRows, laneColW: Math.min(160, (w - 2 * g) / 2), rowH: 26,
    lanesTop: 16, visibleLanes, qx, qCap, slotW, slotH: 20, tx, trackY, trackH,
    frameY: trackY + 10, frameH: 42, anchorFromRight: 56, label: 13,
  };
}

function lanePos(G: Geometry, i: number): { x: number; y: number } {
  const col = Math.floor(i / G.laneRows);
  const row = i % G.laneRows;
  const block = G.laneRows * G.rowH;
  const top = G.mode === "wide" ? G.lanesTop + Math.max(0, (G.h - 24 - G.lanesTop - block) / 2) : G.lanesTop;
  return { x: G.g + 4 + col * G.laneColW, y: top + row * G.rowH + G.rowH / 2 };
}

function slotPos(G: Geometry, slot: number): { x: number; y: number } {
  const s = Math.min(slot, G.qCap - 1);
  return { x: G.qx + s * (G.slotW + 4), y: G.trackY + (G.trackH - G.slotH) / 2 };
}

export type SceneOpts = { still?: boolean };

export function scene(s: TapeState, lanes: number, G: Geometry, opts: SceneOpts = {}): Prim[] {
  const out: Prim[] = [];
  const t = s.t;
  const off = s.cut;
  const L: Layer = "left";
  const T: Layer = "tape";

  // Column names.
  if (G.mode === "wide") {
    out.push({ k: "text", layer: L, x: G.g, y: 20, s: "Writers", c: "muted", a: 1, size: 13, align: "left" });
  }
  const headY = G.mode === "wide" ? 20 : G.trackY - 15;
  out.push({ k: "text", layer: L, x: G.qx, y: headY, s: "Queue", c: "muted", a: 1, size: 13, align: "left" });
  out.push({ k: "text", layer: L, x: G.tx, y: headY, s: "Tape", c: "muted", a: 1, size: 13, align: "left" });
  if (G.visibleLanes < lanes) {
    out.push({
      k: "text", layer: L, x: G.w - G.g, y: G.lanesTop + G.rowH / 2, s: `+${lanes - G.visibleLanes} more`,
      c: "muted", a: 1, size: G.label, align: "right",
    });
  }

  // Writers: a light and a name each.
  const lastG = lastGroup(s)?.group ?? -1;
  for (let i = 0; i < Math.min(lanes, G.visibleLanes); i += 1) {
    const p = lanePos(G, i);
    const lane = s.lanes[i];
    const pulse = s.pulses.find((u) => u.lane === i && t >= u.start && t < u.start + PULSE_MS);
    let c: Tone = "muted";
    let a = 0.4;
    if (off) a = 0.16;
    else if (opts.still && lane?.lastAckGroup === lastG) {
      c = "accent";
      a = 1;
    } else if (pulse) {
      c = "accent";
      a = 1 - 0.55 * easeIn((t - pulse.start) / PULSE_MS);
    } else if (lane && lane.waiting !== null) {
      c = "ink";
      a = 0.6;
    }
    out.push({ k: "dot", layer: L, x: p.x, y: p.y, r: 4, c, a });
    out.push({
      k: "text", layer: L, x: p.x + 13, y: p.y, s: laneLabel(i, lanes), c: "ink", a: off ? 0.4 : 0.88,
      size: G.label, align: "left",
    });
  }

  // The queue rail and the tape track.
  out.push({
    k: "rect", layer: L, x: G.qx - 6, y: G.trackY + (G.trackH - G.slotH) / 2 - 6,
    w: G.qCap * (G.slotW + 4) + 8, h: G.slotH + 12, c: "bg", a: 1,
  });
  out.push({ k: "rect", layer: T, x: G.tx, y: G.trackY, w: G.w - G.tx, h: G.trackH, c: "bg", a: 1 });

  // Writes on their way to the queue, and waiting in it.
  let overflow = 0;
  for (const w of s.queue) {
    const to = slotPos(G, w.slot);
    if (w.slot >= G.qCap) overflow += 1;
    const k = Math.min(1, (t - w.at) / TRAVEL_MS);
    const hidden = w.lane >= G.visibleLanes;
    // A write from a writer off the list slides in from the queue's mouth instead.
    const from = hidden ? { x: G.qx - 28, y: to.y + G.slotH / 2 } : lanePos(G, w.lane);
    const e = slide(k);
    const x = from.x + (to.x - from.x) * e;
    const y = from.y - G.slotH / 2 + (to.y - from.y + G.slotH / 2) * e;
    if (w.slot >= G.qCap && k >= 1) continue;
    const a = (off ? 0.2 : 0.62) * (hidden ? Math.min(1, k * 2) : 1);
    out.push({ k: "rect", layer: L, x, y, w: G.slotW, h: G.slotH, c: "ink", a });
  }
  if (overflow > 0) {
    const p = slotPos(G, G.qCap - 1);
    out.push({
      k: "text", layer: L, x: p.x + G.slotW + 10, y: p.y - 12, s: `+${overflow}`, c: "ink", a: 0.88,
      size: 13, align: "left", num: true,
    });
  }

  // Frames. The head rests a fixed distance in from the right edge; the feed pulls it there.
  const anchor = G.w - G.anchorFromRight;
  const scale = G.frameH / 42;
  const xOf = (x: number) => anchor - (s.feed - x) * scale;
  const synced = s.syncing;
  for (const f of s.frames) {
    let x = xOf(f.x);
    let y = G.frameY;
    let w = f.w * scale;
    let h = G.frameH;
    let a = 1;
    const lay = (t - f.laidAt) / LAY_MS;
    let layer: Layer = T;
    if (lay < 1 && f.state !== "lost") {
      // Laid down: slides from its queue slot onto the tape.
      const from = slotPos(G, f.slot);
      const e = slide(lay);
      x = from.x + (x - from.x) * e;
      y = from.y + (y - from.y) * e;
      w = G.slotW + (w - G.slotW) * e;
      h = G.slotH + (h - G.slotH) * e;
      if (x < G.tx) layer = L;
    }
    if (f.state === "lost") {
      const k = Math.min(1, (t - (f.lostAt ?? t)) / LOST_MS);
      y += easeIn(k) * 28;
      a = 1 - k;
    }
    if (x + w < G.tx - 1 && layer === T) continue;
    if (x > G.w) continue;
    const syncedTone = f.state === "synced";
    const c: Tone = f.kind === "footer" ? "muted" : "ink";
    const alpha = (syncedTone ? 1 : 0.36) * a * (off && f.state !== "lost" ? 0.75 : 1);
    if (x < G.tx && layer === T) {
      // Clip at the tape's left end.
      w -= G.tx - x;
      x = G.tx;
      if (w <= 0) continue;
    }
    out.push({ k: "rect", layer, x, y, w, h, c, a: alpha });
    if (f.kind === "record" && w >= 26 && layer === T && x + w <= G.w) {
      out.push({
        k: "text", layer, x: x + w / 2, y: y + h / 2, s: String(f.lsn), c: syncedTone ? "bg" : "ink",
        a: syncedTone ? a : 0.92 * a, size: 13, align: "center", num: true,
      });
    }
  }

  // The sync: a solid bar sweeps the group's frames at a constant rate. They turn synced at the end.
  if (synced && t >= synced.start && !off) {
    const k = Math.min(1, (t - synced.start) / (synced.end - synced.start));
    const x0 = Math.max(G.tx, xOf(synced.x0));
    const x1 = xOf(synced.x1);
    const xs = xOf(synced.x0) + (x1 - xOf(synced.x0)) * k;
    if (xs > G.tx) {
      out.push({ k: "rect", layer: T, x: x0, y: G.frameY + G.frameH + 6, w: Math.max(0, xs - x0), h: 4, c: "ink", a: 1 });
      out.push({ k: "rect", layer: T, x: xs - 1.5, y: G.trackY + 3, w: 3, h: G.trackH - 6, c: "ink", a: 1 });
    }
  }
  return out;
}
