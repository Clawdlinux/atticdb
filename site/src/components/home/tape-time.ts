// The tape's clock. Small on purpose: the hero imports it eagerly; the sim loads later.
import { ackFor } from "@/content/bench";

export type Disk = "nvme" | "premium-v2";
export type Writers = 1 | 4 | 16;

// The writer lays a group on the tape in this long. Shared by the sim and the clock below.
export const LAY_MS = 360;

/*
 * Screen time for one lone ack (lay plus fdatasync) is the real 1-writer p50 times the slowdown.
 * 8,000x makes NVMe's 94 µs a 752 ms commit. Premium SSD v2 would take 6.5 s at that rate,
 * so it takes the largest clean factor that keeps the commit under ACK_CAP_MS.
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

// How much slower than real the picture runs, for the caption. Exact, not rounded after the fact.
export function slowdownFor(disk: Disk): number {
  const p50ms = ackFor(disk, 1).p50us / 1000;
  return p50ms * SLOWDOWN <= ACK_CAP_MS ? SLOWDOWN : cleanFloor(ACK_CAP_MS / p50ms);
}

// The on-screen fdatasync: the commit time less the lay.
export function syncMsFor(disk: Disk): number {
  return Math.round((ackFor(disk, 1).p50us * slowdownFor(disk)) / 1000) - LAY_MS;
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
