export type PlugPhase = "idle" | "writing" | "cut" | "restarted";

export type PlugState = {
  phase: PlugPhase;
  sent: number;
  acked: number;
  synced: number;
  keptAttic: number;
  keptAckBeforeSync: number;
};

export type PlugEvent = "tick" | "cut" | "restart" | "reset";

// The store that acks before sync has this many acked writes not yet on disk.
export const UNSYNCED_TAIL = 3;

export function initialPlug(): PlugState {
  return { phase: "idle", sent: 0, acked: 0, synced: 0, keptAttic: 0, keptAckBeforeSync: 0 };
}

// A tick sends one write. The first tick from idle starts the stream.
// AtticDB acks a write only once it is synced, and sync lags the writes sent by 1.
export function stepPlug(s: PlugState, ev: PlugEvent): PlugState {
  switch (ev) {
    case "tick": {
      if (s.phase !== "idle" && s.phase !== "writing") return s;
      const sent = s.sent + 1;
      const synced = Math.max(0, sent - 1);
      return { ...s, phase: "writing", sent, synced, acked: synced };
    }
    case "cut":
      return s.phase === "writing" || s.phase === "idle" ? { ...s, phase: "cut" } : s;
    case "restart":
      if (s.phase !== "cut") return s;
      return {
        ...s,
        phase: "restarted",
        keptAttic: s.acked,
        keptAckBeforeSync: Math.max(0, s.sent - UNSYNCED_TAIL),
      };
    case "reset":
      return initialPlug();
  }
}
