import { describe, expect, it } from "vitest";
import { ackFor } from "@/content/bench";
import {
  FRAME_GAP,
  LAY_MS,
  PULSE_MS,
  SEGMENT_UNITS,
  segmentStart,
  cleanFactor,
  commitMsFor,
  cutTape,
  initTape,
  midGroup,
  plugOutcome,
  pulseCap,
  restartTape,
  retuneTape,
  runToCut,
  slowdownFor,
  stepTape,
  stillFrame,
  syncMsFor,
  tapeParams,
  type TapeParams,
  type TapeState,
  type Writers,
} from "./tape-sim";

function run(p: TapeParams, ms: number, dt = 16, from = initTape(p)): TapeState[] {
  const out: TapeState[] = [];
  let s = from;
  for (let t = 0; t < ms; t += dt) {
    s = stepTape(s, dt, p);
    out.push(s);
  }
  return out;
}

const last = <T,>(xs: T[]) => xs[xs.length - 1];

describe.each([1, 4, 16] as Writers[])("the tape with %i writers", (writers) => {
  const p = tapeParams("nvme", writers);
  const states = run(p, 12000);
  const end = last(states);

  it("acks only writes whose frames are all synced", () => {
    let checked = 0;
    for (const s of states) {
      for (const u of s.pulses) {
        const own = s.frames.filter((f) => f.write === u.write);
        expect(own.length).toBeGreaterThan(0);
        expect(own.every((f) => f.state === "synced")).toBe(true);
        checked += 1;
      }
    }
    expect(checked).toBeGreaterThan(0);
  });

  it("gives frames strictly increasing LSNs along the tape", () => {
    for (const s of [states[200], states[400], end]) {
      for (let i = 1; i < s.frames.length; i += 1) {
        expect(s.frames[i].lsn).toBe(s.frames[i - 1].lsn + 1);
        expect(s.frames[i].x).toBeGreaterThan(s.frames[i - 1].x);
      }
    }
  });

  it("closes every batch with exactly one footer, as its last frame", () => {
    const byWrite = new Map<number, string[]>();
    for (const f of end.frames) byWrite.set(f.write, [...(byWrite.get(f.write) ?? []), f.kind]);
    const complete = [...byWrite.values()].slice(1, -1);
    expect(complete.length).toBeGreaterThan(0);
    for (const kinds of complete) {
      expect(kinds[kinds.length - 1]).toBe("footer");
      expect(kinds.filter((k) => k === "footer")).toHaveLength(1);
    }
  });

  it("runs one sync per group, never two at once", () => {
    expect(end.syncsDone).toBe(end.groupsDone);
    const groups = end.history.map((g) => g.group);
    expect(new Set(groups).size).toBe(groups.length);
    for (let i = 1; i < end.history.length; i += 1) {
      expect(end.history[i].layStart).toBeGreaterThanOrEqual(end.history[i - 1].syncEnd);
    }
  });

  it("makes a write that arrives during a sync wait for the next group", () => {
    let seen = 0;
    for (const s of states) {
      if (!s.syncing) continue;
      for (const w of s.queue) {
        if (w.readyAt <= s.syncing.start) continue;
        const g = end.history.find((h) => h.writes.includes(w.id));
        if (!g) continue;
        expect(g.group).toBeGreaterThan(s.syncing.group);
        seen += 1;
      }
    }
    if (writers > 1) expect(seen).toBeGreaterThan(0);
  });

  it("never pulses more than a third of the lanes in one frame", () => {
    const cap = pulseCap(writers);
    for (const s of states) {
      const lit = s.pulses.filter((u) => s.t >= u.arrive && s.t < u.arrive + PULSE_MS);
      expect(lit.length).toBeLessThanOrEqual(cap);
    }
  });

  it("sends an ack only after its group's sync has ended, and the light flashes after it arrives", () => {
    let seen = 0;
    for (const s of states) {
      for (const u of s.pulses) {
        const g = end.history.find((h) => h.group === u.group) ?? s.history.find((h) => h.group === u.group);
        expect(g).toBeDefined();
        expect(u.start).toBeGreaterThanOrEqual(g!.syncEnd);
        expect(u.arrive).toBeGreaterThan(u.start);
        seen += 1;
      }
    }
    expect(seen).toBeGreaterThan(0);
  });

  it("never lets a batch span two segments", () => {
    const segOf = new Map<number, Set<number>>();
    for (const f of end.frames) segOf.set(f.write, (segOf.get(f.write) ?? new Set()).add(f.segment));
    for (const segs of segOf.values()) expect(segs.size).toBe(1);
    for (const f of end.frames) {
      expect(f.x).toBeGreaterThanOrEqual(segmentStart(f.segment));
      expect(f.x + f.w).toBeLessThanOrEqual(segmentStart(f.segment) + SEGMENT_UNITS);
    }
  });

  it("is deterministic for a seed", () => {
    expect(last(run(p, 12000))).toEqual(end);
  });
});

describe("group commit", () => {
  function meanGroup(writers: Writers) {
    const end = last(run(tapeParams("nvme", writers), 20000));
    return end.groupWrites / end.groupsDone;
  }

  it("commits a lone write alone, with no timer", () => {
    const end = last(run(tapeParams("nvme", 1), 20000));
    expect(end.history.every((g) => g.writes.length === 1)).toBe(true);
    // Lay plus sync, nothing more: a lone write never waits on a timer.
    for (const g of end.history) expect(g.syncEnd - g.layStart).toBe(LAY_MS + syncMsFor("nvme", 1));
  });

  it("groups writes at 4 writers too: more than 1.3 a group on average", () => {
    expect(meanGroup(4)).toBeGreaterThan(1.3);
  });

  it("makes bigger groups with more writers", () => {
    const one = meanGroup(1);
    const four = meanGroup(4);
    const sixteen = meanGroup(16);
    expect(sixteen).toBeGreaterThan(one);
    expect(sixteen).toBeGreaterThan(four);
    expect(four).toBeGreaterThanOrEqual(one);
  });
});

describe("pulling the plug", () => {
  const p = tapeParams("nvme", 16);
  const before = runToCut(last(run(p, 6000)), p);

  it("cuts mid-group", () => {
    const s = before;
    expect(s.cut).toBe(true);
    expect(s.frames.some((f) => f.state === "lost")).toBe(true);
  });

  it("drops exactly the frames written after the last sync", () => {
    const pre = last(run(p, 6000));
    let s = pre;
    while (!midGroup(s)) s = stepTape(s, 16, p);
    const lastSynced = Math.max(0, ...s.frames.filter((f) => f.state === "synced").map((f) => f.lsn));
    const after = cutTape(s);
    const lost = after.frames.filter((f) => f.state === "lost").map((f) => f.lsn);
    const unsynced = s.frames.filter((f) => f.lsn > lastSynced).map((f) => f.lsn);
    expect(lost).toEqual(unsynced);
    expect(lost.length).toBeGreaterThan(0);
    expect(after.frames.filter((f) => f.state === "synced")).toEqual(
      s.frames.filter((f) => f.state === "synced"),
    );
  });

  it("keeps every acked write", () => {
    const s = before;
    const kept = new Set(s.frames.filter((f) => f.state === "synced").map((f) => f.write));
    const ackedIds = s.history.flatMap((g) => g.writes);
    for (const id of ackedIds) {
      if (s.frames.some((f) => f.write === id)) expect(kept.has(id)).toBe(true);
    }
    expect(s.lastRun!.attic.kept).toBe(s.lastRun!.attic.acked);
  });

  it("reports the plug result from the simulation", () => {
    const pre = last(run(p, 3000));
    const lastRun = runToCut(pre, p).lastRun!;
    expect(lastRun.attic.acked).toBeGreaterThan(0);
    // The other store acked every write laid down, and only the synced ones survive.
    expect(lastRun.ackBeforeSync.acked).toBeGreaterThan(lastRun.ackBeforeSync.kept);
    expect(lastRun.ackBeforeSync.kept).toBe(lastRun.attic.kept);
    // Counted against the frames: the synced writes are exactly the acked ones.
    const fresh = runToCut(stepTape(initTape(p), 3000, p), p);
    const synced = new Set(fresh.frames.filter((f) => f.state === "synced").map((f) => f.write));
    const laid = new Set(fresh.frames.map((f) => f.write));
    expect(fresh.lastRun).toEqual({
      attic: { acked: synced.size, kept: synced.size },
      ackBeforeSync: { acked: laid.size, kept: synced.size },
    });
    expect(plugOutcome(stepTape(initTape(p), 3000, p)).attic.acked).toBe(synced.size);
  });

  it("holds the tape still while frames drop, then winds back to the last synced frame", () => {
    const held = stepTape(before, 200, p);
    expect(held.feed).toBe(before.feed);
    const wound = stepTape(before, 4000, p);
    const lastSynced = wound.frames.filter((f) => f.state === "synced").at(-1)!;
    expect(wound.feed).toBe(lastSynced.x + lastSynced.w + FRAME_GAP);
  });

  it("never lights a writer green for a write the cut dropped", () => {
    expect(before.lost.length).toBeGreaterThan(0);
    const lostIds = new Set(before.lost);
    let s = before;
    for (let i = 0; i < 300; i += 1) {
      s = stepTape(s, 16, p);
      if (i === 100) s = restartTape(s, p);
      for (const u of s.pulses) {
        expect(lostIds.has(u.write)).toBe(false);
        if (s.cut) expect(u.arrive + PULSE_MS).toBeLessThanOrEqual(before.t);
      }
    }
    expect(s.acked).toBeGreaterThan(0);
  });

  it("stops writing while the power is out, then recovers from the last synced frame", () => {
    const frozen = stepTape(before, 800, p);
    expect(frozen.frames.filter((f) => f.state === "synced")).toEqual(
      before.frames.filter((f) => f.state === "synced"),
    );
    expect(frozen.frames.some((f) => f.state === "lost")).toBe(false);
    const back = restartTape(frozen, p);
    const lastSynced = back.frames[back.frames.length - 1];
    expect(back.cut).toBe(false);
    expect(back.lsn).toBe(lastSynced.lsn + 1);
    const later = stepTape(back, 3000, p);
    expect(later.acked).toBeGreaterThan(0);
    for (let i = 1; i < later.frames.length; i += 1) {
      expect(later.frames[i].lsn).toBe(later.frames[i - 1].lsn + 1);
    }
  });
});

describe("settings", () => {
  it("syncs slower on Premium SSD v2, and keeps every commit watchable", () => {
    expect(syncMsFor("nvme", 1)).toBe(752 - LAY_MS);
    expect(syncMsFor("premium-v2", 1)).toBeGreaterThan(syncMsFor("nvme", 1) * 2);
    for (const d of ["nvme", "premium-v2"] as const) {
      for (const w of [1, 4, 16] as const) expect(commitMsFor(d, w)).toBeLessThanOrEqual(1700);
    }
  });

  it("times every selection from its own p50, exactly", () => {
    expect(slowdownFor("nvme", 1)).toBe(8000);
    expect(slowdownFor("premium-v2", 1)).toBe(2000);
    for (const d of ["nvme", "premium-v2"] as const) {
      for (const w of [1, 4, 16] as const) {
        const p = tapeParams(d, w);
        const real = (ackFor(d, w).p50us * slowdownFor(d, w)) / 1000;
        expect(Math.abs(LAY_MS + p.syncMs - real) / real).toBeLessThan(0.01);
        // And the sim spends exactly that long on each group commit.
        const end = stepTape(initTape(p), 15000, p);
        for (const g of end.history) expect(g.syncEnd - g.layStart).toBe(LAY_MS + p.syncMs);
      }
    }
    expect(cleanFactor(2660)).toBe(2500);
    expect(cleanFactor(1469)).toBe(1500);
  });

  it("retunes the writers without touching the tape", () => {
    const p1 = tapeParams("nvme", 1);
    const s = stepTape(initTape(p1), 3000, p1);
    const p16 = tapeParams("nvme", 16);
    const t = retuneTape(s, p16);
    expect(t.lanes).toHaveLength(16);
    expect(t.frames).toEqual(s.frames);
    expect(retuneTape(stepTape(t, 2000, p16), p1).lanes).toHaveLength(1);
  });

  it("draws a still frame of one finished group commit", () => {
    for (const w of [1, 4, 16] as Writers[]) {
      const s = stillFrame(tapeParams("nvme", w));
      const g = s.history[s.history.length - 1];
      expect(g.syncEnd).toBeLessThanOrEqual(s.t);
      expect(s.frames.filter((f) => f.group === g.group).every((f) => f.state === "synced")).toBe(true);
      expect(s.lanes.some((l) => l.lastAckGroup === g.group)).toBe(true);
      expect(s.feed).toBe(s.head);
    }
    expect(stillFrame(tapeParams("premium-v2", 16))).toEqual(stillFrame(tapeParams("premium-v2", 16)));
  });
});
