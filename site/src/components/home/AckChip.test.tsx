import { describe, expect, it } from "vitest";
import { formatAck } from "./AckChip";

describe("formatAck", () => {
  it("shows microseconds under 1000 µs", () => {
    expect(formatAck(94)).toBe("94 µs");
    expect(formatAck(817)).toBe("817 µs");
    expect(formatAck(999)).toBe("999 µs");
  });

  it("shows milliseconds with one decimal at 1000 µs and above", () => {
    expect(formatAck(1000)).toBe("1.0 ms");
    expect(formatAck(1236)).toBe("1.2 ms");
    expect(formatAck(1473)).toBe("1.5 ms");
  });
});
