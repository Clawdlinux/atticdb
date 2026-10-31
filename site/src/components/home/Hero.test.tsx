import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { INSTALL_CMD, WAITLIST_URL } from "@/lib/site";
import Hero from "./Hero";
import { RESTART_AT_MS } from "./PlugDemo";

// The chip's text as it reads at rest: a leaving value may still be fading out beside it.
function ackText(container: HTMLElement) {
  const value = container.querySelector('[data-ui="ack"] [data-present="true"]')?.textContent;
  return `acked ${value}`;
}

function highlightIn(container: HTMLElement, name: string) {
  return container.querySelector(`input[name="${name}"]:checked`)?.parentElement?.querySelector(
    "[class*=highlight]",
  );
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

  it("swaps the ack value: the new one arrives, the old one leaves out of the accessible tree", () => {
    const { container } = render(<Hero />);
    fireEvent.click(screen.getByRole("radio", { name: "Premium SSD v2" }));
    const values = [...container.querySelectorAll('[data-ui="ack"] [data-present]')];
    const leaving = values.find((v) => v.getAttribute("data-present") === "false");
    const current = values.find((v) => v.getAttribute("data-present") === "true");
    expect(current).toHaveTextContent("817 µs");
    if (leaving) {
      expect(leaving).toHaveTextContent("94 µs");
      expect(leaving).toHaveAttribute("aria-hidden", "true");
    }
  });

  it("moves the one selection highlight to the picked option in each chip", () => {
    const { container } = render(<Hero />);
    expect(container.querySelectorAll("[class*=highlight]")).toHaveLength(2);
    expect(highlightIn(container, "hero-disk")).not.toBeFalsy();
    fireEvent.click(screen.getByRole("radio", { name: "Premium SSD v2" }));
    fireEvent.click(screen.getByRole("radio", { name: "4" }));
    expect(highlightIn(container, "hero-disk")?.parentElement).toHaveTextContent("Premium SSD v2");
    expect(highlightIn(container, "hero-writers")?.parentElement).toHaveTextContent("4");
    expect(container.querySelectorAll("[class*=highlight]")).toHaveLength(2);
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
    const { container } = render(<Hero />);
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
    // AtticDB's 9 acked blocks hold; the other store drops its 3 unsynced ones.
    const attic = container.querySelector('[data-label="AtticDB"]')!;
    const other = container.querySelector('[data-label="Ack before sync"]')!;
    expect(attic.querySelectorAll('[class*=block][data-kind="acked"]')).toHaveLength(9);
    expect(attic.querySelectorAll('[data-kind="lost"]')).toHaveLength(0);
    expect(other.querySelectorAll('[class*=block][data-kind="lost"]')).toHaveLength(3);
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
