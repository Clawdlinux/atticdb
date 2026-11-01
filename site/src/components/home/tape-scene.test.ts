import { describe, expect, it } from "vitest";
import { geometry, scene, type Mode, type Prim } from "./tape-scene";
import { cutTape, initTape, stepTape, tapeParams, type TapeState, type Writers } from "./tape-sim";

type Rect = Extract<Prim, { k: "rect" }>;

// A frame is drawn in the synced tone when its rect is at full strength.
function syncedLsns(prims: Prim[]): number[] {
  return prims
    .filter((p): p is Rect => p.k === "rect" && p.lsn !== undefined && p.a >= 0.99)
    .map((p) => p.lsn!);
}

describe("the picture never shows durability it does not have", () => {
  it.each([
    ["wide", 1440, 16],
    ["wide", 1440, 4],
    ["narrow", 390, 16],
  ] as [Mode, number, Writers][])("%s %i px, %i writers: no frame drawn synced ever falls in a cut", (mode, w, writers) => {
    for (const disk of ["nvme", "premium-v2"] as const) {
      const p = tapeParams(disk, writers);
      const G = geometry(mode, w, writers);
      // Cut at many moments: during lays, part way through sweeps, between groups.
      for (let cutAt = 1500; cutAt <= 6000; cutAt += 170) {
        let s: TapeState = initTape(p);
        const shownSynced = new Set<number>();
        while (s.t < cutAt) {
          s = stepTape(s, 16, p);
          for (const lsn of syncedLsns(scene(s, writers, G))) shownSynced.add(lsn);
        }
        const after = cutTape(s);
        const fell = after.frames.filter((f) => f.state === "lost").map((f) => f.lsn);
        for (const lsn of fell) expect(shownSynced.has(lsn)).toBe(false);
      }
    }
  });

  it("keeps the unsynced tone through the whole sweep, then turns synced when the sync returns", () => {
    const p = tapeParams("nvme", 16);
    const G = geometry("wide", 1440, 16);
    let s = initTape(p);
    let sawSweep = false;
    for (let i = 0; i < 600; i += 1) {
      s = stepTape(s, 16, p);
      if (!s.syncing || s.t < s.syncing.start) continue;
      sawSweep = true;
      const pending = new Set(s.frames.filter((f) => f.group === s.syncing!.group).map((f) => f.lsn));
      for (const lsn of syncedLsns(scene(s, 16, G))) expect(pending.has(lsn)).toBe(false);
    }
    expect(sawSweep).toBe(true);
  });
});

describe("layout", () => {
  it.each([
    ["wide", 1440],
    ["wide", 1024],
    ["narrow", 390],
    ["narrow", 360],
  ] as [Mode, number][])("%s %i px: everything sits inside the content column, tracks are solid", (mode, w) => {
    for (const writers of [1, 4, 16] as Writers[]) {
      const p = tapeParams("nvme", writers);
      const G = geometry(mode, w, writers);
      let s = initTape(p);
      for (let i = 0; i < 300; i += 1) {
        s = stepTape(s, 16, p);
        for (const q of scene(s, writers, G)) {
          if (q.k === "text") {
            // Text width estimated at 7 px a character, generous for the 13 px text face.
            const half = (q.s.length * 7) / 2;
            const tx0 = q.align === "left" ? q.x : q.align === "right" ? q.x - 2 * half : q.x - half;
            expect(tx0).toBeGreaterThanOrEqual(G.g - 0.5);
            if (q.align !== "left") expect(tx0 + 2 * half).toBeLessThanOrEqual(G.right + 0.5);
            continue;
          }
          const x0 = q.k === "rect" ? q.x : q.x - q.r;
          const x1 = q.k === "rect" ? q.x + q.w : q.x + q.r;
          expect(x0).toBeGreaterThanOrEqual(G.g - 0.5);
          expect(x1).toBeLessThanOrEqual(G.right + 0.5);
          // No faint hairlines: a long thin shape that is not a frame in motion is drawn at full strength.
          if (q.k === "rect" && q.lsn === undefined && Math.min(q.w, q.h) < 6 && Math.max(q.w, q.h) > 24) expect(q.a).toBeGreaterThanOrEqual(0.99);
        }
      }
      // Each writer's channel is at least 6 px tall, and the 16 writers share 4 channels.
      expect(G.chH).toBeGreaterThanOrEqual(6);
      expect(G.channels).toBe(writers === 16 ? 4 : writers);
    }
  });
});
