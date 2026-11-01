/*
 * The tape: a pure, deterministic model of AtticDB's write path for the hero graphic.
 * Source: docs/design/0001-architecture.md, "Writer thread". Leader-follower group commit:
 * the writer drains the queue, lays every pending batch onto the tape (frames plus a footer),
 * syncs once for the group, then acks every write in it. No timer, so a lone write commits alone.
 *
 * Times are visual milliseconds. Tape positions are abstract units; the renderer scales them.
 * Nothing here touches the DOM, so tests, the canvas and the build-time SVG all share it.
 */

import { ackFor } from "@/content/bench";
import { syncMsFor, type Disk, type PlugOutcome, type Writers } from "./tape-time";

export type { Disk, PlugOutcome, Writers } from "./tape-time";
export { SLOWDOWN, SYNC_CAP_MS, cleanFactor, slowdownFor, syncMsFor } from "./tape-time";

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
  // The write leaves its lane at `at` and reaches the queue at `readyAt`.
  at: number;
  readyAt: number;
  // Its place in the queue, for the slide in and the slide out.
  slot: number;
};

export type FrameState = "written" | "synced" | "lost";

export type TapeFrame = {
  lsn: number;
  kind: "record" | "footer";
  write: number;
  lane: number;
  group: number;
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

export type Pulse = { lane: number; write: number; group: number; start: number };

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
  // Tape end in units, and the feed position that chases it at a constant speed.
  head: number;
  feed: number;
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
  // Set when a restart follows a cut: what each store kept, from this run.
  lastRun: PlugOutcome | null;
};

export const TRAVEL_MS = 240;
export const LAY_MS = 200;
export const PULSE_MS = 240;
export const FRAME_W = 36;
export const FOOTER_W = 10;
export const FRAME_GAP = 2;
// Units per second. Linear: the tape feeds like a mechanism.
export const FEED_SPEED = 420;
export const LOST_MS = 520;
export const RESTART_MS = 1500;

// Mean think time of 1 writer. More writers write faster in total, by the benchmark's acks/s ratio.
export const BASE_THINK_MS = 640;

// mulberry32: small, seedable and the same everywhere.
function rand(s: { rng: number }): number {
  s.rng = (s.rng + 0x6d2b79f5) | 0;
  let x = s.rng;
  x = Math.imul(x ^ (x >>> 15), x | 1);
  x ^= x + Math.imul(x ^ (x >>> 7), x | 61);
  return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
}

function think(s: { rng: number }, p: TapeParams): number {
  return p.writeEveryMs * (0.65 + 0.7 * rand(s));
}

function records(s: { rng: number }): number {
  const r = rand(s);
  return r < 0.72 ? 1 : r < 0.94 ? 2 : 3;
}

// Each writer's think time, so total writes per second follow the benchmark's acks/s ratio.
export function writeEveryFor(disk: Disk, writers: Writers): number {
  const ratio = ackFor(disk, writers).acksPerSec / ackFor(disk, 1).acksPerSec;
  return Math.round((BASE_THINK_MS * writers) / ratio);
}

export function tapeParams(disk: Disk, writers: Writers, seed = 7): TapeParams {
  return { writers, syncMs: syncMsFor(disk), writeEveryMs: writeEveryFor(disk, writers), seed };
}

function newLane(s: { rng: number }, p: TapeParams, t: number, first: boolean): Lane {
  // Writers start spread out, so the first group is not every writer at once.
  const delay = first ? 120 + rand(s) * p.writeEveryMs * 0.9 : think(s, p);
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
    feed: 0,
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
    lastRun: null,
  };
  s.lanes = Array.from({ length: p.writers }, () => newLane(s, p, 0, true));
  return s;
}

// The most lanes that may pulse in one frame: a third, and at least one.
export function pulseCap(lanes: number): number {
  return Math.max(1, Math.floor(lanes / 3));
}

function clone(s: TapeState): TapeState {
  return {
    ...s,
    queue: s.queue.slice(),
    frames: s.frames.slice(),
    lanes: s.lanes.map((l) => ({ ...l })),
    pulses: s.pulses.slice(),
    history: s.history.slice(),
  };
}

const MAX_FRAMES = 480;
const MAX_HISTORY = 64;

// The writer is the leader: when it is free it drains every write that has reached the queue.
function drain(s: TapeState, p: TapeParams) {
  const ready = s.queue.filter((w) => w.readyAt <= s.t);
  if (ready.length === 0) return;
  s.queue = s.queue.filter((w) => w.readyAt > s.t);
  s.group += 1;
  const from = s.lsn;
  for (const w of ready) {
    // One batch per write: its records, then the footer that closes it.
    for (let i = 0; i < w.records; i += 1) {
      s.frames.push({
        lsn: s.lsn++, kind: "record", write: w.id, lane: w.lane, group: s.group,
        x: s.head, w: FRAME_W, state: "written", laidAt: s.t, slot: w.slot,
      });
      s.head += FRAME_W + FRAME_GAP;
    }
    s.frames.push({
      lsn: s.lsn++, kind: "footer", write: w.id, lane: w.lane, group: s.group,
      x: s.head, w: FOOTER_W, state: "written", laidAt: s.t, slot: w.slot,
    });
    s.head += FOOTER_W + FRAME_GAP;
    s.written += 1;
  }
  s.laying = {
    group: s.group, writes: ready.map((w) => w.id), from, to: s.lsn - 1,
    layStart: s.t, syncStart: s.t + LAY_MS, syncEnd: s.t + LAY_MS + p.syncMs,
  };
  // Writes still travelling keep their places; the queue closes up behind the drained ones.
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
  const stagger = PULSE_MS / pulseCap(p.writers);
  for (const id of rec.writes) {
    s.acked += 1;
    const lane = s.lanes.findIndex((l) => l.waiting === id);
    const start = Math.max(sync.end, s.nextPulseAt);
    s.nextPulseAt = start + stagger;
    const laneOf = s.frames.find((f) => f.write === id)?.lane ?? -1;
    s.pulses.push({ lane: lane >= 0 ? lane : laneOf, write: id, group: sync.group, start });
    if (lane >= 0) {
      const l = s.lanes[lane];
      l.waiting = null;
      l.lastAckAt = start;
      l.lastAckGroup = sync.group;
      // The writer sees its ack when its light pulses, then thinks before writing again.
      l.nextAt = start + think(s, p);
    }
  }
  s.syncing = null;
  s.laying = null;
}

// The tape moves toward its target at a constant speed, either way.
function feed(s: TapeState, dt: number, target: number) {
  const step = (FEED_SPEED * dt) / 1000;
  const gap = target - s.feed;
  if (gap > 0) s.feed = Math.min(target, s.feed + step);
  else if (gap < 0) s.feed = Math.max(target, s.feed - step);
}

// Where the durable tape ends: just past the last synced frame.
function durableEnd(s: TapeState): number {
  for (let i = s.frames.length - 1; i >= 0; i -= 1) {
    const f = s.frames[i];
    if (f.state === "synced") return f.x + f.w + FRAME_GAP;
  }
  return 0;
}

// One step of `dtMs`, handled in small slices so every event lands on its own time.
export function stepTape(prev: TapeState, dtMs: number, p: TapeParams): TapeState {
  const s = clone(prev);
  let left = Math.max(0, dtMs);
  while (left > 0) {
    const dt = Math.min(left, 8);
    left -= dt;
    s.t += dt;
    // With the power out, the tape holds still while unsynced frames drop,
    // then winds back to the last synced frame: that is where recovery will start.
    if (!s.cut) feed(s, dt, s.head);
    else if (s.t >= (s.cutAt ?? 0) + LOST_MS) feed(s, dt, durableEnd(s));
    s.pulses = s.pulses.filter((u) => s.t < u.start + PULSE_MS + 400);
    s.frames = s.frames.filter((f) => f.state !== "lost" || s.t < (f.lostAt ?? 0) + LOST_MS);
    if (s.cut) continue;

    // Writers whose think time is up send their next write into the queue.
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
// Synced frames, and so every acked write, stay.
export function cutTape(prev: TapeState): TapeState {
  if (prev.cut) return prev;
  const s = clone(prev);
  s.cut = true;
  s.cutAt = s.t;
  s.lastRun = plugOutcome(prev);
  s.frames = s.frames.map((f) => (f.state === "written" ? { ...f, state: "lost", lostAt: s.t } : f));
  s.queue = [];
  s.laying = null;
  s.syncing = null;
  s.pulses = [];
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
  s.cut = false;
  s.cutAt = null;
  s.written = 0;
  s.acked = 0;
  s.nextPulseAt = s.t;
  s.lanes = Array.from({ length: p.writers }, () => newLane(s, p, s.t, true));
  return s;
}

// The settings changed. The tape and any group in flight carry on; the writers change.
export function retuneTape(prev: TapeState, p: TapeParams): TapeState {
  const s = clone(prev);
  const n = p.writers;
  if (s.lanes.length > n) {
    // Writers that go away take their unsent writes with them. Queued ones still commit.
    s.lanes = s.lanes.slice(0, n);
  } else {
    while (s.lanes.length < n) s.lanes.push(newLane(s, p, s.t, true));
  }
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

// Run forward until a group is part way through its sync, then cut there.
export function runToCut(prev: TapeState, p: TapeParams, limitMs = 20000): TapeState {
  let s = prev;
  let spent = 0;
  while (spent < limitMs) {
    if (s.syncing && s.t >= s.syncing.start + (s.syncing.end - s.syncing.start) * 0.4) break;
    s = stepTape(s, 16, p);
    spent += 16;
  }
  return cutTape(s);
}

// One finished group commit, for reduced motion and the build-time poster.
// The run stops just after a group of the expected size syncs, with its acks lit.
const STILL_HISTORY = 1100;

const stills = new Map<string, TapeState>();

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
  return out;
}

export function lastGroup(s: TapeState): GroupRecord | null {
  return s.history.length ? s.history[s.history.length - 1] : null;
}
