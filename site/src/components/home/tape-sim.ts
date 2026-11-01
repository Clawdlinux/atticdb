/*
 * The tape: a pure, deterministic model of AtticDB's write path for the hero graphic.
 * Source: docs/design/0001-architecture.md, "Writer thread". Leader-follower group commit:
 * the writer drains the queue, lays every pending batch onto the tape (frames plus a footer),
 * syncs once for the group, then acks every write in it. No timer, so a lone write commits alone.
 * A transaction never spans segments: a batch that would cross one rolls to the next.
 *
 * Times are visual milliseconds. Tape positions are units (CSS px at desktop scale).
 * Nothing here touches the DOM, so tests, the canvas and the build-time SVG all share it.
 */

import { LAY_MS, syncMsFor, type Disk, type PlugOutcome, type Writers } from "./tape-time";

export type { Disk, PlugOutcome, Writers } from "./tape-time";
export { LAY_MS, SLOWDOWN, cleanFactor, commitMsFor, slowdownFor, syncMsFor } from "./tape-time";

export type TapeParams = {
  writers: Writers;
  // How long one fdatasync takes on screen.
  syncMs: number;
  // Mean think time of one writer between its ack and its next write.
  writeEveryMs: number;
  seed: number;
};

export type Pending = {
  id: number;
  lane: number;
  records: number;
  // The write leaves its writer at `at` and parks in the queue at `readyAt`.
  at: number;
  readyAt: number;
  // Its place in the queue's stack, bottom up.
  slot: number;
};

export type FrameState = "written" | "synced" | "lost";

export type TapeFrame = {
  lsn: number;
  kind: "record" | "footer";
  write: number;
  lane: number;
  group: number;
  segment: number;
  // Left edge and width on the tape, in units.
  x: number;
  w: number;
  state: FrameState;
  laidAt: number;
  slot: number;
  lostAt?: number;
};

export type Lane = {
  // Next time this writer sends, once it is free.
  nextAt: number;
  // The write it is waiting on. A closed-loop writer has at most one in flight.
  waiting: number | null;
  lastAckAt: number | null;
  lastAckGroup: number | null;
};

// An ack on its way back: it leaves the queue at `start`, reaches the writer at `arrive`,
// and the writer's light flashes for PULSE_MS from then.
export type Pulse = { lane: number; write: number; group: number; start: number; arrive: number };

export type GroupRecord = {
  group: number;
  writes: number[];
  from: number;
  to: number;
  layStart: number;
  syncStart: number;
  syncEnd: number;
};

export type Sync = { group: number; from: number; to: number; x0: number; x1: number; start: number; end: number };

export type TapeState = {
  t: number;
  rng: number;
  nextWrite: number;
  // The next LSN the writer assigns. Strictly increasing on the tape.
  lsn: number;
  queue: Pending[];
  frames: TapeFrame[];
  lanes: Lane[];
  // Frames being laid down before the sync starts.
  laying: GroupRecord | null;
  syncing: Sync | null;
  group: number;
  // Tape end in units, the current segment and where it ends.
  head: number;
  segment: number;
  // The feed position and its speed. The head rests near a fixed point on screen.
  feed: number;
  vel: number;
  pulses: Pulse[];
  nextPulseAt: number;
  history: GroupRecord[];
  // Counters since load or the last restart.
  written: number;
  acked: number;
  groupsDone: number;
  syncsDone: number;
  groupWrites: number;
  cut: boolean;
  cutAt: number | null;
  // Writes the cut dropped. Their writers never see an ack.
  lost: number[];
  // Set at a cut: what each store kept, from this run.
  lastRun: PlugOutcome | null;
};

// A write slides from its writer to the queue, distance-scaled, 300 to 450 ms on screen.
export const TRAVEL_MS = 320;
export const ACK_TRAVEL_MS = 300;
export const PULSE_MS = 240;
export const FRAME_W = 44;
export const FOOTER_W = 12;
export const FRAME_GAP = 3;
// A 64 MiB segment, drawn this long. About 26 frames.
export const SEGMENT_UNITS = 1240;
// The ambient feed: a slow base speed, plus a pull toward the head, smoothed.
export const FEED_BASE = 18;
export const FEED_GAIN = 4;
export const FEED_TAU_MS = 450;
// Winding back to the last synced frame after a cut.
export const REWIND_SPEED = 420;
export const LOST_MS = 400;
export const RESTART_MS = 1500;

/*
 * Think time between a writer's ack and its next write. The benchmark's writers have none,
 * so throughput gain there comes from group commit alone. Here a short pause keeps it readable,
 * the same for 1 and 4 writers. At 16 it is longer, so the tape stays slow enough to read.
 */
export const THINK_MS: Record<Writers, number> = { 1: 300, 4: 120, 16: 2000 };
// mulberry32: small, seedable and the same everywhere.
function rand(s: { rng: number }): number {
  s.rng = (s.rng + 0x6d2b79f5) | 0;
  let x = s.rng;
  x = Math.imul(x ^ (x >>> 15), x | 1);
  x ^= x + Math.imul(x ^ (x >>> 7), x | 61);
  return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
}

function think(s: { rng: number }, p: TapeParams): number {
  return p.writeEveryMs * (0.8 + 0.4 * rand(s));
}

function records(s: { rng: number }): number {
  const r = rand(s);
  return r < 0.72 ? 1 : r < 0.94 ? 2 : 3;
}

export function writeEveryFor(writers: Writers): number {
  return THINK_MS[writers];
}

export function tapeParams(disk: Disk, writers: Writers, seed = 7): TapeParams {
  return { writers, syncMs: syncMsFor(disk, writers), writeEveryMs: writeEveryFor(writers), seed };
}

function newLane(s: { rng: number }, p: TapeParams, t: number): Lane {
  // Writers start spread out, so the first group is not every writer at once.
  const delay = 120 + rand(s) * p.writeEveryMs * 0.9;
  return { nextAt: t + delay, waiting: null, lastAckAt: null, lastAckGroup: null };
}

export function initTape(p: TapeParams): TapeState {
  const s: TapeState = {
    t: 0,
    rng: p.seed | 0,
    nextWrite: 1,
    lsn: 1,
    queue: [],
    frames: [],
    lanes: [],
    laying: null,
    syncing: null,
    group: 0,
    head: 0,
    segment: 1,
    feed: 0,
    vel: 0,
    pulses: [],
    nextPulseAt: 0,
    history: [],
    written: 0,
    acked: 0,
    groupsDone: 0,
    syncsDone: 0,
    groupWrites: 0,
    cut: false,
    cutAt: null,
    lost: [],
    lastRun: null,
  };
  s.lanes = Array.from({ length: p.writers }, () => newLane(s, p, 0));
  return s;
}

// The most lanes that may flash in one frame: a third, and at least one.
export function pulseCap(lanes: number): number {
  return Math.max(1, Math.floor(lanes / 3));
}

// Acks leave this far apart, so no more than pulseCap lights are lit at once.
export function pulseStagger(lanes: number): number {
  return PULSE_MS / pulseCap(lanes);
}

function clone(s: TapeState): TapeState {
  return {
    ...s,
    queue: s.queue.slice(),
    frames: s.frames.slice(),
    lanes: s.lanes.map((l) => ({ ...l })),
    pulses: s.pulses.slice(),
    history: s.history.slice(),
    lost: s.lost.slice(),
  };
}

const MAX_FRAMES = 480;
const MAX_HISTORY = 64;

export function segmentStart(segment: number): number {
  return (segment - 1) * SEGMENT_UNITS;
}

function batchWidth(records: number): number {
  return records * (FRAME_W + FRAME_GAP) + FOOTER_W + FRAME_GAP;
}

// The writer is the leader: when it is free it drains every write parked in the queue.
function drain(s: TapeState, p: TapeParams) {
  const ready = s.queue.filter((w) => w.readyAt <= s.t);
  if (ready.length === 0) return;
  s.queue = s.queue.filter((w) => w.readyAt > s.t);
  s.group += 1;
  const from = s.lsn;
  for (const w of ready) {
    // A batch never spans segments. If it would, the rest of this one is roll padding.
    if (s.head + batchWidth(w.records) > segmentStart(s.segment + 1)) {
      s.segment += 1;
      s.head = segmentStart(s.segment);
    }
    // One batch per write: its records, then the footer that closes it.
    for (let i = 0; i < w.records; i += 1) {
      s.frames.push({
        lsn: s.lsn++, kind: "record", write: w.id, lane: w.lane, group: s.group, segment: s.segment,
        x: s.head, w: FRAME_W, state: "written", laidAt: s.t, slot: w.slot,
      });
      s.head += FRAME_W + FRAME_GAP;
    }
    s.frames.push({
      lsn: s.lsn++, kind: "footer", write: w.id, lane: w.lane, group: s.group, segment: s.segment,
      x: s.head, w: FOOTER_W, state: "written", laidAt: s.t, slot: w.slot,
    });
    s.head += FOOTER_W + FRAME_GAP;
    s.written += 1;
  }
  s.laying = {
    group: s.group, writes: ready.map((w) => w.id), from, to: s.lsn - 1,
    layStart: s.t, syncStart: s.t + LAY_MS, syncEnd: s.t + LAY_MS + p.syncMs,
  };
  // Writes still on their way keep their order; the stack closes up under them.
  s.queue = s.queue.map((w, i) => ({ ...w, slot: i }));
  if (s.frames.length > MAX_FRAMES) s.frames = s.frames.slice(s.frames.length - MAX_FRAMES);
}

function startSync(s: TapeState) {
  const g = s.laying!;
  const own = s.frames.filter((f) => f.group === g.group);
  const last = own[own.length - 1];
  s.syncing = {
    group: g.group, from: g.from, to: g.to,
    x0: own[0].x, x1: last.x + last.w,
    start: g.syncStart, end: g.syncEnd,
  };
}

// fdatasync returned: the group's frames are durable. Publish, then ack every write in it.
function finishSync(s: TapeState, p: TapeParams) {
  const sync = s.syncing!;
  const rec = s.laying!;
  s.frames = s.frames.map((f) => (f.group === sync.group ? { ...f, state: "synced" } : f));
  s.syncsDone += 1;
  s.groupsDone += 1;
  s.groupWrites += rec.writes.length;
  s.history.push(rec);
  if (s.history.length > MAX_HISTORY) s.history.shift();
  const stagger = pulseStagger(p.writers);
  for (const id of rec.writes) {
    s.acked += 1;
    const lane = s.lanes.findIndex((l) => l.waiting === id);
    const start = Math.max(sync.end, s.nextPulseAt);
    s.nextPulseAt = start + stagger;
    const arrive = start + ACK_TRAVEL_MS;
    const laneOf = s.frames.find((f) => f.write === id)?.lane ?? -1;
    s.pulses.push({ lane: lane >= 0 ? lane : laneOf, write: id, group: sync.group, start, arrive });
    if (lane >= 0) {
      const l = s.lanes[lane];
      l.waiting = null;
      l.lastAckAt = arrive;
      l.lastAckGroup = sync.group;
      // The writer sees its ack when it arrives, then thinks before writing again.
      l.nextAt = arrive + think(s, p);
    }
  }
  s.syncing = null;
  s.laying = null;
}

// Where the durable tape ends: just past the last synced frame.
export function durableEnd(s: TapeState): number {
  for (let i = s.frames.length - 1; i >= 0; i -= 1) {
    const f = s.frames[i];
    if (f.state === "synced") return f.x + f.w + FRAME_GAP;
  }
  return 0;
}

// The ambient feed: a slow constant drift plus a smoothed pull toward the head. Never backwards.
function feed(s: TapeState, dt: number) {
  const target = Math.max(0, FEED_BASE + FEED_GAIN * (s.head - s.feed));
  s.vel += (target - s.vel) * Math.min(1, dt / FEED_TAU_MS);
  s.feed += (s.vel * dt) / 1000;
}

function rewind(s: TapeState, dt: number, target: number) {
  const step = (REWIND_SPEED * dt) / 1000;
  s.vel = 0;
  if (s.feed > target) s.feed = Math.max(target, s.feed - step);
  else s.feed = Math.min(target, s.feed + step);
}

// One step of `dtMs`, handled in small slices so every event lands on its own time.
export function stepTape(prev: TapeState, dtMs: number, p: TapeParams): TapeState {
  const s = clone(prev);
  let left = Math.max(0, dtMs);
  while (left > 0) {
    const dt = Math.min(left, 8);
    left -= dt;
    s.t += dt;
    s.pulses = s.pulses.filter((u) => s.t < u.arrive + PULSE_MS + 200);
    s.frames = s.frames.filter((f) => f.state !== "lost" || s.t < (f.lostAt ?? 0) + LOST_MS);
    if (s.cut) {
      // With the power out the tape holds still while unsynced frames drop,
      // then winds back to the last synced frame: that is where recovery will start.
      if (s.t >= (s.cutAt ?? 0) + LOST_MS) rewind(s, dt, durableEnd(s));
      continue;
    }
    feed(s, dt);

    // Writers whose think time is up send their next write toward the queue.
    s.lanes.forEach((l, i) => {
      if (l.waiting !== null || s.t < l.nextAt) return;
      const id = s.nextWrite++;
      l.waiting = id;
      s.queue.push({ id, lane: i, records: records(s), at: s.t, readyAt: s.t + TRAVEL_MS, slot: s.queue.length });
    });

    if (s.laying && !s.syncing && s.t >= s.laying.syncStart) startSync(s);
    if (s.syncing && s.t >= s.syncing.end) finishSync(s, p);
    if (!s.laying && !s.syncing) drain(s, p);
  }
  return s;
}

// The power goes out. Frames laid since the last sync were never durable, so they drop.
// Synced frames, and so every acked write, stay. Acks still on their way never arrive.
export function cutTape(prev: TapeState): TapeState {
  if (prev.cut) return prev;
  const s = clone(prev);
  s.cut = true;
  s.cutAt = s.t;
  s.lastRun = plugOutcome(prev);
  const lost = new Set<number>();
  s.frames = s.frames.map((f) => {
    if (f.state !== "written") return f;
    lost.add(f.write);
    return { ...f, state: "lost", lostAt: s.t };
  });
  for (const w of s.queue) lost.add(w.id);
  s.lost = [...lost];
  s.queue = [];
  s.laying = null;
  s.syncing = null;
  s.pulses = s.pulses.filter((u) => u.arrive + PULSE_MS <= s.t);
  s.vel = 0;
  return s;
}

// The store restarts. Recovery truncates at the last synced frame, so the next LSN follows it.
// Writers whose writes were never acked start again.
export function restartTape(prev: TapeState, p: TapeParams): TapeState {
  const s = clone(prev);
  s.frames = s.frames.filter((f) => f.state === "synced");
  const last = s.frames[s.frames.length - 1];
  s.lsn = last ? last.lsn + 1 : 1;
  s.head = last ? last.x + last.w + FRAME_GAP : 0;
  s.segment = last ? last.segment : 1;
  s.cut = false;
  s.cutAt = null;
  s.lost = [];
  s.written = 0;
  s.acked = 0;
  s.nextPulseAt = s.t;
  s.lanes = Array.from({ length: p.writers }, () => newLane(s, p, s.t));
  return s;
}

// The settings changed. The tape and any group in flight carry on; the writers change.
export function retuneTape(prev: TapeState, p: TapeParams): TapeState {
  const s = clone(prev);
  const n = p.writers;
  // Writers that go away take their unsent writes with them. Queued ones still commit.
  if (s.lanes.length > n) s.lanes = s.lanes.slice(0, n);
  while (s.lanes.length < n) s.lanes.push(newLane(s, p, s.t));
  // A sync in flight keeps its own length; the next one uses the new disk.
  return s;
}

// What each store kept if the power went out now.
export function plugOutcome(s: TapeState): PlugOutcome {
  // Counters, not visible frames: old frames scroll off and get pruned.
  // Only synced writes survive, and AtticDB acked exactly those.
  return {
    attic: { acked: s.acked, kept: s.acked },
    ackBeforeSync: { acked: s.written, kept: s.acked },
  };
}

// A group is on the tape and not yet synced.
export function midGroup(s: TapeState): boolean {
  return !s.cut && (s.laying !== null || s.syncing !== null);
}

// How far the sync bar has swept the current group, 0 to 1.
export function syncProgress(s: TapeState): number {
  if (!s.syncing || s.t < s.syncing.start) return 0;
  return Math.min(1, (s.t - s.syncing.start) / (s.syncing.end - s.syncing.start));
}

// The cut lands while the bar is a third of the way across a group.
export const CUT_AT = 0.3;

// Run forward until a group is part way through its sync, then cut there.
export function runToCut(prev: TapeState, p: TapeParams, limitMs = 20000): TapeState {
  let s = prev;
  let spent = 0;
  while (spent < limitMs) {
    if (syncProgress(s) >= CUT_AT) break;
    s = stepTape(s, 16, p);
    spent += 16;
  }
  return cutTape(s);
}

const STILL_HISTORY = 1100;
const stills = new Map<string, TapeState>();

// One finished group commit, for reduced motion and the build-time poster.
// The run stops just after a group of the expected size syncs, with its acks lit.
export function stillFrame(p: TapeParams): TapeState {
  const key = `${p.writers}/${p.syncMs}/${p.writeEveryMs}/${p.seed}`;
  const hit = stills.get(key);
  if (hit) return clone(hit);
  const out = computeStill(p);
  stills.set(key, out);
  return clone(out);
}

function computeStill(p: TapeParams): TapeState {
  const want = p.writers === 1 ? 1 : p.writers === 4 ? 3 : 6;
  let s = initTape(p);
  let best: TapeState | null = null;
  for (let i = 0; i < 4000; i += 1) {
    const before = s.groupsDone;
    s = stepTape(s, 16, p);
    // Enough history that the tape reads as a tape, not a few blocks.
    if (s.groupsDone > before && s.groupsDone >= 3 && s.head >= STILL_HISTORY) {
      const last = s.history[s.history.length - 1];
      if (last.writes.length >= want) {
        best = s;
        break;
      }
      if (!best || last.writes.length > best.history[best.history.length - 1].writes.length) best = s;
    }
  }
  const out = clone(best ?? s);
  // At rest the tape has fed to its head.
  out.feed = out.head;
  out.vel = 0;
  return out;
}

export function lastGroup(s: TapeState): GroupRecord | null {
  return s.history.length ? s.history[s.history.length - 1] : null;
}
