// The tape's clock. Small on purpose: the hero imports it eagerly; the sim loads later.
import { ackFor } from "@/content/bench";

export type Disk = "nvme" | "premium-v2";
export type Writers = 1 | 4 | 16;

// Real fsync time to screen time. 94 µs on NVMe reads as about a quarter second.
export const SLOWDOWN = 2500;
// Premium SSD v2 would sweep for 2 s at that rate. The cap keeps it watchable.
export const SYNC_CAP_MS = 1200;

// The on-screen sync time for a disk, from its 1-writer p50.
export function syncMsFor(disk: Disk): number {
  return Math.min(SYNC_CAP_MS, Math.round((ackFor(disk, 1).p50us * SLOWDOWN) / 1000));
}

const CLEAN = [1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10];

// A slowdown factor, rounded to a number a person would say.
export function cleanFactor(x: number): number {
  if (!(x > 0)) return 1;
  const mag = 10 ** Math.floor(Math.log10(x));
  const n = x / mag;
  const best = CLEAN.reduce((a, b) => (Math.abs(b - n) < Math.abs(a - n) ? b : a));
  return Math.round(best * mag);
}

// How much slower than real the sync on screen runs, for the caption.
export function slowdownFor(disk: Disk, syncMs = syncMsFor(disk)): number {
  return cleanFactor((syncMs * 1000) / ackFor(disk, 1).p50us);
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
