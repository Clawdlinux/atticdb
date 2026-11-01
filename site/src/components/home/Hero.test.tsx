import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { INSTALL_CMD, WAITLIST_URL } from "@/lib/site";
import Hero from "./Hero";
import { RESTART_MS, runToCut, stillFrame, tapeParams } from "./tape-sim";

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
  // jsdom has no 2D context (see vitest.setup.ts). The canvas then never starts and the poster stays.
  beforeEach(() => vi.useRealTimers());
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  // The canvas module loads after hydration.
  async function loaded(container: HTMLElement) {
    await waitFor(() => expect(container.querySelector("canvas")).not.toBeNull());
    return container.querySelector("canvas")!;
  }

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

  it("shows the build-time poster first, then one labelled canvas", async () => {
    const { container } = render(<Hero />);
    const poster = container.querySelector('[data-ui="poster"]')!;
    expect(poster).toHaveAttribute("role", "img");
    expect(poster).toHaveAttribute("aria-label", expect.stringMatching(/^The tape\. 1 writer on Local NVMe/));
    expect(poster.querySelectorAll("svg rect").length).toBeGreaterThan(5);
    expect(poster).toHaveTextContent("Claude Code");
    const canvas = await loaded(container);
    expect(canvas).toHaveAttribute("role", "img");
    expect(canvas).toHaveAttribute("aria-label", poster.getAttribute("aria-label"));
    fireEvent.click(screen.getByRole("radio", { name: "16" }));
    expect(canvas).toHaveAttribute("aria-label", expect.stringMatching(/^The tape\. 16 writers on Local NVMe/));
  });

  it("states the slowdown against the real p50", () => {
    const { container } = render(<Hero />);
    const caption = () => container.querySelector("[class*=timeCaption]");
    expect(caption()).toHaveTextContent("Time slowed 2,500x. Real p50 on Local NVMe: 94 µs");
    fireEvent.click(screen.getByRole("radio", { name: "Premium SSD v2" }));
    expect(caption()).toHaveTextContent("Time slowed 1,500x. Real p50 on Premium SSD v2: 817 µs");
  });

  it("pulls the plug mid-group and reports what each store kept, from the simulation", async () => {
    const p = tapeParams("nvme", 1);
    const outcome = runToCut(stillFrame(p), p).lastRun!;
    const { container } = render(<Hero />);
    await loaded(container);
    vi.useFakeTimers();
    expect(
      screen.getByText("Pull the plug. Frames not yet synced drop away. Acked writes stay."),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Pull the plug" }));
    expect(screen.getByText("Power off. Frames not yet synced drop away.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Pull the plug" })).toHaveAttribute("aria-disabled", "true");
    act(() => {
      vi.advanceTimersByTime(RESTART_MS + 10);
    });
    const { attic, ackBeforeSync } = outcome;
    expect(attic.kept).toBe(attic.acked);
    expect(ackBeforeSync.acked).toBeGreaterThan(ackBeforeSync.kept);
    expect(
      screen.getByText(`AtticDB: ${attic.kept} of ${attic.acked} acked writes kept.`),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        `A store that acks before sync: ${ackBeforeSync.kept} of ${ackBeforeSync.acked} acked writes kept.`,
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Pull it again" })).not.toHaveAttribute("aria-disabled");
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
