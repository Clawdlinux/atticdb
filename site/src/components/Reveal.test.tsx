import { act, render, screen } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MOTION, revealBoot, revealBootScript } from "@/lib/motion";
import Reveal from "./Reveal";

type Callback = (entries: Partial<IntersectionObserverEntry>[]) => void;
let observers: { callback: Callback; targets: Element[] }[] = [];
const RealIO = globalThis.IntersectionObserver;

class FakeIO {
  private entry: { callback: Callback; targets: Element[] };
  constructor(callback: Callback) {
    this.entry = { callback, targets: [] };
    observers.push(this.entry);
  }
  observe(el: Element) {
    this.entry.targets.push(el);
  }
  unobserve(el: Element) {
    this.entry.targets = this.entry.targets.filter((t) => t !== el);
  }
  disconnect() {
    this.entry.targets = [];
  }
  takeRecords() {
    return [];
  }
}

beforeEach(() => {
  observers = [];
  globalThis.IntersectionObserver = FakeIO as unknown as typeof IntersectionObserver;
  delete (window as Window & { __reveal?: unknown }).__reveal;
});

afterEach(() => {
  vi.restoreAllMocks();
  globalThis.IntersectionObserver = RealIO;
});

// Runs the boot with its first frame flushed at once.
function boot() {
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb: FrameRequestCallback) => {
    cb(0);
    return 0;
  });
  revealBoot(MOTION);
}

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
  it("renders its children in the requested tag, marked static", () => {
    render(<Reveal as="section">Every write hits the disk.</Reveal>);
    const el = screen.getByText("Every write hits the disk.");
    expect(el.tagName).toBe("SECTION");
    expect(el).toHaveAttribute("data-reveal", "static");
  });

  it("puts no hiding style in server HTML, only the delay", () => {
    const html = renderToString(<Reveal delay={0.15}>Visible without JS</Reveal>);
    expect(html).toContain("Visible without JS");
    expect(html).toContain('data-reveal-delay="150"');
    expect(html).not.toMatch(/opacity|translate|style=/);
  });

  it("leaves content untouched when the boot script never ran", () => {
    placeAt(5000);
    render(<Reveal>No script</Reveal>);
    const el = screen.getByText("No script");
    expect(el).toHaveAttribute("data-reveal", "static");
    expect(el.style.opacity).toBe("");
  });

  it("leaves an element that is on screen when the script runs visible", () => {
    boot();
    placeAt(100);
    render(<Reveal>On screen</Reveal>);
    const el = screen.getByText("On screen");
    expect(el).toHaveAttribute("data-reveal", "static");
    expect(el.style.opacity).toBe("");
  });

  it("hides an element below the fold, then commits its end state when it intersects", () => {
    boot();
    placeAt(5000);
    render(<Reveal>Below the fold</Reveal>);
    const el = screen.getByText("Below the fold");
    expect(el).toHaveAttribute("data-reveal", "waiting");
    expect(el.style.opacity).toBe("0");
    expect(el.style.transform).toBe(`translateY(${MOTION.risePx}px)`);

    intersect(el);
    expect(el).toHaveAttribute("data-reveal", "shown");
    expect(el.style.opacity).toBe("");
    expect(el.style.transform).toBe("");
  });

  it("shows a waiting element on a scroll that finds it on screen, without the observer", () => {
    boot();
    placeAt(5000);
    render(<Reveal>Missed by the observer</Reveal>);
    const el = screen.getByText("Missed by the observer");
    expect(el).toHaveAttribute("data-reveal", "waiting");

    placeAt(200);
    act(() => {
      window.dispatchEvent(new Event("scroll"));
    });
    expect(el).toHaveAttribute("data-reveal", "shown");
    expect(el.style.opacity).toBe("");
  });

  it("never hides anything where IntersectionObserver is missing", () => {
    // @ts-expect-error simulate an old browser
    globalThis.IntersectionObserver = undefined;
    boot();
    placeAt(5000);
    render(<Reveal>Old browser</Reveal>);
    expect(screen.getByText("Old browser").style.opacity).toBe("");
  });

  it("drops the travel under reduced motion but keeps the fade", () => {
    const real = window.matchMedia;
    window.matchMedia = ((q: string) => ({ matches: q.includes("reduce"), media: q })) as never;
    boot();
    window.matchMedia = real;
    placeAt(5000);
    render(<Reveal>Still</Reveal>);
    const el = screen.getByText("Still");
    expect(el.style.opacity).toBe("0");
    expect(el.style.transform).toBe("");
  });

  it("touches nothing before the first frame, when the stylesheets apply", () => {
    vi.spyOn(window, "requestAnimationFrame").mockImplementation(() => 0);
    revealBoot(MOTION);
    placeAt(5000);
    render(<Reveal>Before styles</Reveal>);
    const el = screen.getByText("Before styles");
    expect(el).toHaveAttribute("data-reveal", "static");
    expect(el.style.opacity).toBe("");
  });

  it("ships the boot as one self-contained inline script", () => {
    const src = revealBootScript();
    expect(src).toContain("classList.add('js')");
    expect(() => new Function(src)).not.toThrow();
  });
});
