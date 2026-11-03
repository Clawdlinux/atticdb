import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HEADLINE, HONEST, SUB, TRUST } from "@/content/hero";
import { INSTALL_CMD, WAITLIST_URL } from "@/lib/site";
import Hero from "./Hero";
import { LAY_MS, RESTART_MS, runToCut, stillFrame, tapeParams } from "./tape-sim";

// The proof's own live line. The copy button has a status of its own.
function status() {
  return document.querySelector('[data-ui="proof"] [role="status"]')!.textContent;
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

  it("names the h1 by the ICP one-liner alone", () => {
    render(<Hero />);
    expect(
      screen.getByRole("heading", {
        level: 1,
        name: "Your coding agent forgets everything between chats. AtticDB remembers, on your own disk.",
      }),
    ).toBeInTheDocument();
    expect(HEADLINE.join(" ")).toBe(
      "Your coding agent forgets everything between chats. AtticDB remembers, on your own disk.",
    );
  });

  it("keeps the latency chip and the plug demo out of the h1", () => {
    const { container } = render(<Hero />);
    const h1 = container.querySelector("h1")!;
    expect(h1.querySelector('[data-ui="ack"]')).toBeNull();
    expect(h1.querySelector("button, canvas, fieldset")).toBeNull();
    expect(h1.textContent).not.toMatch(/acked|µs/);
  });

  it("shows the sub line, then install, then the honest line, all before the latency chip and the tape", () => {
    const { container } = render(<Hero />);
    const sub = screen.getByText(SUB);
    expect(SUB).toBe(
      "It saves your rules and decisions locally and loads them into every new chat in Claude Code, Codex, Copilot and VS Code. Free, open source, 1 small binary, no account.",
    );
    const install = screen.getByText(INSTALL_CMD);
    const honest = screen.getByText(HONEST);
    expect(HONEST).toBe("Early. Word search, not semantic. Great for your laptop, not for production yet.");
    const chip = container.querySelector('[data-ui="ack"]')!;
    const band = container.querySelector('[data-ui="band"]')!;
    const h1 = container.querySelector("h1")!;
    const order = [h1, sub, install, honest, chip, band];
    for (let i = 1; i < order.length; i++) {
      expect(order[i - 1].compareDocumentPosition(order[i]) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    }
    expect(screen.getByText(TRUST)).toBeInTheDocument();
  });

  it("says saved to disk on the chip, with no unit, and puts the numbers in the status line", () => {
    const { container } = render(<Hero />);
    const chip = container.querySelector('[data-ui="ack"]')!;
    expect(chip).toHaveTextContent(/^saved to disk$/);
    expect(chip).toHaveAttribute("aria-hidden", "true");
    expect(status()).toBe("Local NVMe, 1 writer: AtticDB acked in 94 µs at p50, 9,799 acks/s.");
    fireEvent.click(screen.getByRole("radio", { name: "Premium SSD v2" }));
    expect(status()).toContain("817 µs");
    fireEvent.click(screen.getByRole("radio", { name: "16" }));
    expect(status()).toContain("1.6 ms");
    expect(chip).toHaveTextContent(/^saved to disk$/);
  });

  it("puts the slot after the honest line, so the chip and the plug band come after it", () => {
    const { container } = render(<Hero afterHonest={<section data-testid="slot">demo</section>} />);
    const honest = screen.getByText(HONEST);
    const slot = screen.getByTestId("slot");
    const chip = container.querySelector('[data-ui="ack"]')!;
    const band = container.querySelector('[data-ui="band"]')!;
    for (const [a, b] of [[honest, slot], [slot, chip], [chip, band]] as const) {
      expect(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
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
    // One static SVG, four parts by reference; the page's HTML carries no drawing of its own.
    const refs = [...poster.querySelectorAll("use")].map((u) => u.getAttribute("href"));
    expect(refs).toEqual([
      "/tape-poster.svg#wide-left",
      "/tape-poster.svg#wide-tape",
      "/tape-poster.svg#narrow-left",
      "/tape-poster.svg#narrow-tape",
    ]);
    expect(poster.querySelectorAll("rect, text, img")).toHaveLength(0);
    const canvas = await loaded(container);
    expect(canvas).toHaveAttribute("role", "img");
    expect(canvas).toHaveAttribute("aria-label", poster.getAttribute("aria-label"));
    fireEvent.click(screen.getByRole("radio", { name: "16" }));
    expect(canvas).toHaveAttribute("aria-label", expect.stringMatching(/^The tape\. 16 writers on Local NVMe/));
  });

  it("states a slowdown that is true for every selection: factor × real p50 = commit on screen", () => {
    const { container } = render(<Hero />);
    const caption = () => container.querySelector("[class*=timeCaption]")!.textContent!.replace(/\s+/g, " ");
    expect(caption()).toBe("Time slowed 8,000x. Real p50, Local NVMe, 1 writer: 94 µs");
    for (const disk of ["nvme", "premium-v2"] as const) {
      fireEvent.click(screen.getByRole("radio", { name: disk === "nvme" ? "Local NVMe" : "Premium SSD v2" }));
      for (const writers of [1, 4, 16] as const) {
        fireEvent.click(screen.getByRole("radio", { name: String(writers) }));
        const m = caption().match(/Time slowed ([\d,]+)x\. Real p50, .+, \d+ writers?: ([\d,]+) µs/)!;
        expect(m).not.toBeNull();
        const factor = Number(m[1].replace(/,/g, ""));
        const p50ms = Number(m[2].replace(/,/g, "")) / 1000;
        const screenMs = LAY_MS + tapeParams(disk, writers).syncMs;
        expect(Math.abs(factor * p50ms - screenMs) / screenMs).toBeLessThan(0.01);
      }
    }
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
