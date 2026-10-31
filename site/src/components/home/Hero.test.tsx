import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { INSTALL_CMD, WAITLIST_URL } from "@/lib/site";
import Hero from "./Hero";
import { RESTART_AT_MS } from "./PlugDemo";

function ackText(container: HTMLElement) {
  return container.querySelector('[data-ui="ack"]')?.textContent;
}

describe("Hero", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("names the h1 by the sentence alone", () => {
    render(<Hero />);
    expect(
      screen.getByRole("heading", { level: 1, name: "Memory that hits the disk" }),
    ).toBeInTheDocument();
  });

  it("shows the NVMe 1-writer ack and updates it from the chips", () => {
    const { container } = render(<Hero />);
    expect(ackText(container)).toBe("acked 94 µs");
    fireEvent.click(screen.getByRole("radio", { name: "Premium SSD v2" }));
    expect(ackText(container)).toBe("acked 817 µs");
    fireEvent.click(screen.getByRole("radio", { name: "16" }));
    expect(ackText(container)).toBe("acked 1.6 ms");
  });

  it("shows the install command, the copy button and the waitlist link", () => {
    render(<Hero />);
    expect(screen.getByText(INSTALL_CMD).tagName).toBe("CODE");
    expect(screen.getByRole("button", { name: "Install AtticDB" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Join the cloud waitlist" })).toHaveAttribute(
      "href",
      WAITLIST_URL,
    );
  });

  it("pulls the plug and reports what each store kept", () => {
    render(<Hero />);
    fireEvent.click(screen.getByRole("button", { name: "Pull the plug" }));
    act(() => {
      vi.advanceTimersByTime(RESTART_AT_MS + 10);
    });
    expect(screen.getByText("AtticDB: 9 of 9 acked writes kept.")).toBeInTheDocument();
    expect(
      screen.getByText("A store that acks before sync: 7 of 10 acked writes kept."),
    ).toBeInTheDocument();
    expect(
      screen.getByText("An illustration of ack-after-sync, not a crash test.", { exact: false }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("link", {
        name: "The real test cuts power on LazyFS. CI runs crash simulations on 2,000 seeds.",
      }),
    ).toHaveAttribute(
      "href",
      "https://github.com/Clawdlinux/atticdb/blob/main/TESTING.md#power-loss",
    );
  });
});
