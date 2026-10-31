import { describe, expect, it } from "vitest";
import { initialPlug, stepPlug, type PlugState } from "./plug";

function run(s: PlugState, ...events: Parameters<typeof stepPlug>[1][]): PlugState {
  return events.reduce(stepPlug, s);
}

function ticks(n: number) {
  return Array.from({ length: n }, () => "tick" as const);
}

describe("plug demo", () => {
  it("starts idle with nothing written", () => {
    expect(initialPlug()).toEqual({
      phase: "idle",
      sent: 0,
      acked: 0,
      synced: 0,
      keptAttic: 0,
      keptAckBeforeSync: 0,
    });
  });

  it("acks only synced writes, one behind the writes sent", () => {
    const s = run(initialPlug(), ...ticks(10));
    expect(s.phase).toBe("writing");
    expect(s.sent).toBe(10);
    expect(s.synced).toBe(9);
    expect(s.acked).toBe(9);
  });

  it("keeps every AtticDB ack and loses the unsynced tail of the other store", () => {
    const s = run(initialPlug(), ...ticks(10), "cut", "restart");
    expect(s.phase).toBe("restarted");
    expect(s.keptAttic).toBe(9);
    expect(s.keptAttic).toBe(s.acked);
    // The ack-before-sync store acked all 10 sent writes and kept 7.
    expect(s.sent).toBe(10);
    expect(s.keptAckBeforeSync).toBe(7);
  });

  it("stops writing once the power is cut", () => {
    const s = run(initialPlug(), ...ticks(4), "cut", "tick", "tick");
    expect(s.phase).toBe("cut");
    expect(s.sent).toBe(4);
  });

  it("never reports a negative kept count", () => {
    const s = run(initialPlug(), "tick", "cut", "restart");
    expect(s.keptAckBeforeSync).toBe(0);
    expect(s.keptAttic).toBe(0);
  });

  it("resets to the initial state", () => {
    const s = run(initialPlug(), ...ticks(10), "cut", "restart", "reset");
    expect(s).toEqual(initialPlug());
  });
});
