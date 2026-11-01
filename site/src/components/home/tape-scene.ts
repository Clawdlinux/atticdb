/*
 * The tape as a list of flat shapes. The canvas draws them every frame; the build-time SVG
 * poster draws the same list once. Colours are token names, resolved by each renderer.
 *
 * Reading order, left to right: writers are tracks; a write slides along its track and parks
 * in the queue column; the leader moves the whole stack onto the tape in one motion; a bar
 * sweeps the group (fdatasync); then acks run back along each track and the lights flash.
 */

import {
  ACK_TRAVEL_MS,
  LAY_MS,
  LOST_MS,
  PULSE_MS,
  SEGMENT_UNITS,
  TRAVEL_MS,
  lastGroup,
  segmentStart,
  type TapeState,
} from "./tape-sim";

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
export const TAPE_H: Record<Mode, number> = { wide: 240, narrow: 262 };
// The width under which the band stacks the writers above the tape (48rem).
export const NARROW_BELOW = 768;

const APPS = ["Claude Code", "Copilot CLI", "VS Code", "Codex"];

// 1 and 4 writers: one app each. 16: four of each, grouped, the group's first track labelled.
export function laneApp(i: number, lanes: number): string {
  return lanes > APPS.length ? APPS[Math.floor(i / (lanes / APPS.length))] : APPS[i % APPS.length];
}

export function laneLabel(i: number, lanes: number): string | null {
  if (lanes <= APPS.length) return APPS[i % APPS.length];
  const per = lanes / APPS.length;
  return i % per === 0 ? `${laneApp(i, lanes)} ×${per}` : null;
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
    const bx = 3 * (1 - u) * (1 - u) * u * 0.2 + u * u * u - x;
    const dx = 0.6 * (1 - u) * (1 - u) - 1.2 * (1 - u) * u + 3 * u * u;
    if (Math.abs(dx) < 1e-6) break;
    u = Math.min(1, Math.max(0, u - bx / dx));
  }
  return 3 * (1 - u) * u * u + u * u * u;
}

const easeIn = (t: number) => t * t * t;
const clamp01 = (t: number) => Math.min(1, Math.max(0, t));

export type Geometry = {
  mode: Mode;
  w: number;
  h: number;
  // Left edge of the content column, so labels line up with the headline's container.
  g: number;
  lanes: number;
  cy: number;
  pitch: number;
  lightX: number;
  light: number;
  qx: number;
  colW: number;
  colTop: number;
  colH: number;
  blockW: number;
  blockH: number;
  tx: number;
  trackY: number;
  trackH: number;
  frameY: number;
  frameH: number;
  scale: number;
  // Where the tape head rests, measured from the right edge.
  anchorFromRight: number;
  aboveY: number;
  belowY: number;
  segY: number;
  // The group rides this rail from the queue to the head, clear of the frames already laid.
  railY: number;
};

export function geometry(mode: Mode, w: number, lanes: number, g = gutterFor(w)): Geometry {
  const h = TAPE_H[mode];
  const wide = mode === "wide";
  const area = wide ? 176 : 128;
  const cy = wide ? 118 : 76;
  const pitch = lanes === 1 ? 0 : Math.min(wide ? 40 : 30, area / lanes);
  const blockH = lanes > 8 ? (wide ? 8 : 6) : wide ? 14 : 12;
  const span = (lanes - 1) * pitch;
  const colH = Math.min(area + 8, span + blockH * 2 + 26);
  const lightX = g + (wide ? 112 : 96);
  const qx = g + (wide ? 470 : 290);
  const colW = wide ? 30 : 26;
  if (wide) {
    const tx = qx + colW + 46;
    return {
      mode, w, h, g, lanes, cy, pitch, lightX, light: lanes > 8 ? 3 : 4, qx, colW,
      colTop: cy - colH / 2, colH, blockW: 20, blockH,
      tx, trackY: cy - 40, trackH: 80, frameY: cy - 30, frameH: 60, scale: 1,
      anchorFromRight: Math.min(220, (w - tx) * 0.3), aboveY: cy - 52, belowY: cy + 62, segY: cy + 82,
      railY: cy + 44,
    };
  }
  return {
    mode, w, h, g, lanes, cy, pitch, lightX, light: lanes > 8 ? 3 : 4, qx, colW,
    colTop: cy - colH / 2, colH, blockW: 16, blockH,
    tx: g, trackY: 162, trackH: 48, frameY: 166, frameH: 40, scale: 40 / 60,
    anchorFromRight: 48, aboveY: 152, belowY: 224, segY: 152, railY: 150,
  };
}

function laneY(G: Geometry, i: number): number {
  return G.cy + (i - (G.lanes - 1) / 2) * G.pitch;
}

function slotRect(G: Geometry, slot: number) {
  const cap = Math.max(1, Math.floor((G.colH - 10 + 2) / (G.blockH + 2)));
  const k = Math.min(slot, cap - 1);
  const bottom = G.colTop + G.colH - 5;
  return { x: G.qx + (G.colW - G.blockW) / 2, y: bottom - (k + 1) * G.blockH - k * 2, w: G.blockW, h: G.blockH, cap };
}

export type SceneOpts = {
  still?: boolean;
  // Where a segment label sticks once its boundary has scrolled away. The tape's left end by default;
  // the poster passes the left end it will show at its reference width.
  stickyX?: number;
};

export function scene(s: TapeState, lanes: number, G: Geometry, opts: SceneOpts = {}): Prim[] {
  const out: Prim[] = [];
  const t = s.t;
  const off = s.cut;
  const L: Layer = "left";
  const T: Layer = "tape";
  const lost = new Set(s.lost);

  // Tracks: from the band's left edge, past the name and the light, to the queue.
  const lastG = lastGroup(s)?.group ?? -1;
  for (let i = 0; i < lanes; i += 1) {
    const y = laneY(G, i);
    const label = laneLabel(i, lanes);
    if (G.mode === "wide" && G.g >= 32) out.push({ k: "rect", layer: L, x: 0, y: y - 1, w: G.g - 12, h: 2, c: "ink", a: 0.14 });
    out.push({ k: "rect", layer: L, x: G.lightX + 10, y: y - 1, w: G.qx - G.lightX - 14, h: 2, c: "ink", a: 0.14 });
    if (label) {
      out.push({ k: "text", layer: L, x: G.g, y, s: label, c: "ink", a: off ? 0.4 : 0.86, size: 13, align: "left" });
    }
    const lane = s.lanes[i];
    const flash = s.pulses.find((u) => u.lane === i && t >= u.arrive && t < u.arrive + PULSE_MS);
    let c: Tone = "muted";
    let a = 0.45;
    if (off) a = 0.15;
    else if (opts.still && lane?.lastAckGroup === lastG) {
      c = "accent";
      a = 1;
    } else if (flash && !lost.has(flash.write)) {
      c = "accent";
      a = 1 - 0.5 * easeIn((t - flash.arrive) / PULSE_MS);
    } else if (lane && lane.waiting !== null) {
      c = "ink";
      a = 0.75;
    }
    out.push({ k: "dot", layer: L, x: G.lightX, y, r: G.light, c, a });
  }

  // The queue: a column where parked writes stack, bottom up.
  out.push({ k: "rect", layer: L, x: G.qx, y: G.colTop, w: G.colW, h: G.colH, c: "bg", a: 1 });
  if (G.mode === "wide") {
    out.push({
      k: "text", layer: L, x: G.qx + G.colW / 2, y: G.colTop + G.colH + 12, s: "Queue", c: "muted", a: 1,
      size: 13, align: "center",
    });
  }

  // Writes: along the track first, then down or up into their slot.
  let overflow = 0;
  for (const w of s.queue) {
    const slot = slotRect(G, w.slot);
    if (w.slot >= slot.cap) overflow += 1;
    const k = clamp01((t - w.at) / TRAVEL_MS);
    const y0 = laneY(G, w.lane) - G.blockH / 2;
    const run = slide(k / 0.72);
    const drop = slide((k - 0.72) / 0.28);
    const x = G.lightX + 8 + (slot.x - G.lightX - 8) * run;
    const y = y0 + (slot.y - y0) * drop;
    if (w.slot >= slot.cap && k >= 1) continue;
    out.push({ k: "rect", layer: L, x, y, w: G.blockW, h: G.blockH, c: "ink", a: off ? 0.2 : 0.85 });
  }
  if (overflow > 0) {
    out.push({
      k: "text", layer: L, x: G.qx + G.colW / 2, y: G.colTop - 10, s: `+${overflow}`, c: "ink", a: 0.88,
      size: 13, align: "center", num: true,
    });
  }

  // Acks: a short signal-green bar runs back along the writer's track, right to left.
  if (!off) {
    for (const u of s.pulses) {
      if (t < u.start || t >= u.arrive || u.lane >= lanes || lost.has(u.write)) continue;
      const k = slide((t - u.start) / ACK_TRAVEL_MS);
      const x0 = G.qx - 18;
      const x1 = G.lightX + 8;
      out.push({ k: "rect", layer: L, x: x0 + (x1 - x0) * k, y: laneY(G, u.lane) - 2, w: 22, h: 4, c: "accent", a: 1 });
    }
  }

  // The tape track.
  out.push({ k: "rect", layer: T, x: G.tx, y: G.trackY, w: G.w - G.tx, h: G.trackH, c: "bg", a: 1 });

  const anchor = G.w - G.anchorFromRight;
  const xOf = (x: number) => anchor - (s.feed - x) * G.scale;

  // Segment boundaries: every 64 MiB the writer rolls to a new file.
  const first = Math.max(1, Math.floor((s.feed - (anchor - G.tx) / G.scale) / SEGMENT_UNITS));
  for (let seg = first; seg <= s.segment + 1; seg += 1) {
    const X = xOf(segmentStart(seg));
    const next = xOf(segmentStart(seg + 1));
    if (X > G.w) continue;
    if (seg > 1 && X >= G.tx) {
      out.push({ k: "rect", layer: T, x: X - 1, y: G.trackY - 8, w: 2, h: G.trackH + 16, c: "muted", a: 0.7 });
    }
    // The label rides its boundary and sticks to the tape's left end once the boundary has gone.
    // It is skipped when it would run off the right edge or into the next one.
    const lx = Math.max(opts.stickyX ?? G.tx, X) + 6;
    if (lx + 96 > G.w || next < lx + 104) continue;
    out.push({
      k: "text", layer: T, x: lx, y: G.segY, s: `segment ${String(seg).padStart(4, "0")}`,
      c: "muted", a: 1, size: 13, align: "left", num: true,
    });
  }

  // The sweep: frames turn synced as the bar passes them, and stay so once the sync returns.
  const sync = s.syncing && t >= s.syncing.start && !off ? s.syncing : null;
  const k = sync ? clamp01((t - sync.start) / (sync.end - sync.start)) : 0;
  const barX = sync ? xOf(sync.x0) + (xOf(sync.x1) - xOf(sync.x0)) * k : 0;

  for (const f of s.frames) {
    let x = xOf(f.x);
    let y = G.frameY;
    let w = f.w * G.scale;
    let h = G.frameH;
    let a = 1;
    const lay = (t - f.laidAt) / LAY_MS;
    let layer: Layer = T;
    let fill = f.state === "synced" || (sync && f.group === sync.group && x + w / 2 <= barX) ? 1 : 0.36;
    if (lay < 1 && f.state !== "lost") {
      // The group commit: the whole stack moves together. It rides a rail beside the tape
      // to the head, then each block steps onto the tape as its frame.
      const from = slotRect(G, f.slot);
      const toX = x;
      const toY = y;
      const ride = slide(lay / 0.6);
      const step = slide((lay - 0.6) / 0.4);
      x = from.x + (toX - from.x) * ride;
      const railTop = G.railY - from.h / 2;
      y = step > 0 ? railTop + (toY - railTop) * step : from.y + (railTop - from.y) * ride;
      w = from.w + (w - from.w) * step;
      h = from.h + (h - from.h) * step;
      fill = 0.85 + (fill - 0.85) * step;
      if (x < G.tx) layer = L;
    }
    if (f.state === "lost") {
      const d = clamp01((t - (f.lostAt ?? t)) / LOST_MS);
      y += easeIn(d) * 32;
      a = 1 - d;
    }
    if (layer === T && x + w < G.tx - 1) continue;
    if (x > G.w) continue;
    if (layer === T && x < G.tx) {
      // Clip at the tape's left end.
      w -= G.tx - x;
      x = G.tx;
      if (w <= 0) continue;
    }
    const c: Tone = f.kind === "footer" ? "muted" : "ink";
    const alpha = fill * a * (off && f.state !== "lost" ? 0.75 : 1);
    out.push({ k: "rect", layer, x, y, w, h, c, a: alpha });
    if (f.kind === "record" && w >= 26 && layer === T && x + w <= G.w) {
      const solid = fill > 0.6;
      out.push({
        k: "text", layer, x: x + w / 2, y: y + h / 2, s: String(f.lsn), c: solid ? "bg" : "ink",
        a: solid ? a : 0.92 * a, size: 13, align: "center", num: true,
      });
    }
  }

  if (sync) {
    const x0 = Math.max(G.tx, xOf(sync.x0));
    if (barX > G.tx) {
      out.push({ k: "rect", layer: T, x: x0, y: G.frameY + G.frameH + 3, w: Math.max(0, barX - x0), h: 3, c: "ink", a: 1 });
      out.push({ k: "rect", layer: T, x: barX - 2, y: G.trackY - 6, w: 4, h: G.trackH + 12, c: "ink", a: 1 });
      out.push({ k: "text", layer: T, x: barX, y: G.belowY, s: "fdatasync", c: "ink", a: 1, size: 13, align: "center" });
    }
  }

  // The group's name: how many writes, and one sync for all of them. Shown until its acks land.
  const g = s.laying ?? lastGroup(s);
  if (g && !off) {
    const own = s.frames.filter((f) => f.group === g.group);
    const lastAck = Math.max(g.syncEnd, ...s.pulses.filter((u) => u.group === g.group).map((u) => u.arrive + PULSE_MS));
    const showing = opts.still || t < lastAck;
    if (own.length && showing && t >= g.layStart + LAY_MS * 0.5) {
      const x0 = Math.max(G.tx + 4, xOf(own[0].x));
      const x1 = xOf(own[own.length - 1].x + own[own.length - 1].w);
      const n = g.writes.length;
      out.push({
        k: "text", layer: T, x: Math.max(G.tx + 4, (x0 + x1) / 2), y: G.aboveY,
        s: `group of ${n}, 1 sync`, c: "ink", a: 0.88, size: 13, align: "center",
      });
    }
  }
  return out;
}
