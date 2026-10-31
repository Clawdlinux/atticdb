import { act, render, screen, waitFor } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Reveal from "./Reveal";

type Callback = (entries: Partial<IntersectionObserverEntry>[]) => void;
let observers: { callback: Callback; targets: Element[] }[] = [];

beforeEach(() => {
  observers = [];
  globalThis.IntersectionObserver = class {
    private entry: { callback: Callback; targets: Element[] };
    constructor(callback: Callback) {
      this.entry = { callback, targets: [] };
      observers.push(this.entry);
    }
    observe(el: Element) {
      this.entry.targets.push(el);
    }
    unobserve() {}
    disconnect() {
      this.entry.targets = [];
    }
    takeRecords() {
      return [];
    }
  } as unknown as typeof IntersectionObserver;
});

afterEach(() => vi.restoreAllMocks());

function placeAt(top: number) {
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    () => ({ top, bottom: top + 100, left: 0, right: 100, width: 100, height: 100, x: 0, y: top }) as DOMRect,
  );
}

function intersect(el: Element) {
  act(() => {
    for (const o of observers) {
      if (o.targets.includes(el)) o.callback([{ target: el, isIntersecting: true }]);
    }
  });
}

describe("Reveal", () => {
  it("renders its children", () => {
    render(<Reveal>Every write hits the disk.</Reveal>);
    expect(screen.getByText("Every write hits the disk.")).toBeInTheDocument();
  });

  it("renders the requested tag and marks it", () => {
    render(<Reveal as="section">Hello</Reveal>);
    const el = screen.getByText("Hello");
    expect(el.tagName).toBe("SECTION");
    expect(el).toHaveAttribute("data-reveal");
  });

  it("puts no hiding style in server HTML", () => {
    const html = renderToString(<Reveal delay={0.1}>Visible without JS</Reveal>);
    expect(html).toContain("Visible without JS");
    expect(html).not.toMatch(/opacity:\s*0/);
    expect(html).not.toMatch(/translate/);
  });

  it("leaves an element that is on screen at mount visible, with no entrance", () => {
    placeAt(100);
    render(<Reveal>On screen</Reveal>);
    const el = screen.getByText("On screen");
    expect(el).toHaveAttribute("data-reveal", "static");
    expect(el.style.opacity).toBe("");
  });

  it("hides an element below the fold, then shows it when it intersects", async () => {
    placeAt(5000);
    render(<Reveal>Below the fold</Reveal>);
    const el = screen.getByText("Below the fold");
    expect(el).toHaveAttribute("data-reveal", "waiting");
    expect(el.style.opacity).toBe("0");

    intersect(el);
    expect(el).toHaveAttribute("data-reveal", "shown");
    await waitFor(() => expect(el.style.opacity).toBe("1"), { timeout: 2500 });
  });

  it("shows a waiting element on a scroll that finds it on screen, without the observer", async () => {
    placeAt(5000);
    render(<Reveal>Missed by the observer</Reveal>);
    const el = screen.getByText("Missed by the observer");
    expect(el).toHaveAttribute("data-reveal", "waiting");

    placeAt(200);
    act(() => {
      window.dispatchEvent(new Event("scroll"));
    });
    expect(el).toHaveAttribute("data-reveal", "shown");
    await waitFor(() => expect(el.style.opacity).toBe("1"), { timeout: 2500 });
  });
});
