// The tape's clock. Small on purpose: the hero imports it eagerly; the sim loads later.
import { ackFor } from "@/content/bench";

export type Disk = "nvme" | "premium-v2";
export type Writers = 1 | 4 | 16;

// The writer lays a group on the tape in this long. Shared by the sim and the clock below.
export const LAY_MS = 360;

/*
 * Each selection is timed from its own p50. On screen, one commit (lay plus fdatasync) takes
 * the real p50 for that disk and writer count times the slowdown, so the caption is true for
 * whatever is selected. 8,000x makes NVMe's 94 µs (1 writer) a 752 ms commit. Where that would
 * pass ACK_CAP_MS, the selection takes the largest clean factor that fits, to stay watchable.
 */
export const SLOWDOWN = 8000;
export const ACK_CAP_MS = 1700;

const CLEAN = [1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10];

// A slowdown factor, rounded to a number a person would say.
export function cleanFactor(x: number): number {
  if (!(x > 0)) return 1;
  const mag = 10 ** Math.floor(Math.log10(x));
  const n = x / mag;
  const best = CLEAN.reduce((a, b) => (Math.abs(b - n) < Math.abs(a - n) ? b : a));
  return Math.round(best * mag);
}

// The largest clean factor at or under x.
function cleanFloor(x: number): number {
  const mag = 10 ** Math.floor(Math.log10(x));
  const n = x / mag;
  const best = CLEAN.filter((c) => c <= n + 1e-9).pop() ?? 1;
  return Math.round(best * mag);
}

// How much slower than real the picture runs for this selection. Exact, not rounded after the fact.
export function slowdownFor(disk: Disk, writers: Writers): number {
  const p50ms = ackFor(disk, writers).p50us / 1000;
  return p50ms * SLOWDOWN <= ACK_CAP_MS ? SLOWDOWN : cleanFloor(ACK_CAP_MS / p50ms);
}

// One commit on screen: the selection's real p50 times its slowdown.
export function commitMsFor(disk: Disk, writers: Writers): number {
  return Math.round((ackFor(disk, writers).p50us * slowdownFor(disk, writers)) / 1000);
}

// The on-screen fdatasync: the commit time less the lay.
export function syncMsFor(disk: Disk, writers: Writers): number {
  return commitMsFor(disk, writers) - LAY_MS;
}

// What each store kept when the power went out. Counted by the sim.
export type PlugOutcome = {
  // AtticDB acks after sync, so every acked write is on a synced frame.
  attic: { acked: number; kept: number };
  // A store that acks once a write is laid down acks every written write, and keeps the synced ones.
  ackBeforeSync: { acked: number; kept: number };
};

export type PlugPhase = "idle" | "armed" | "cut" | "restarted";
export type PlugView = { phase: PlugPhase; outcome: PlugOutcome | null };
