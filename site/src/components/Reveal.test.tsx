import { render, screen } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { beforeAll, describe, expect, it } from "vitest";
import Reveal from "./Reveal";

beforeAll(() => {
  // jsdom has no IntersectionObserver. Motion's whileInView needs one.
  globalThis.IntersectionObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
    takeRecords() {
      return [];
    }
  } as unknown as typeof IntersectionObserver;
});

describe("Reveal", () => {
  it("renders its children", () => {
    render(<Reveal>Every write hits the disk.</Reveal>);
    expect(screen.getByText("Every write hits the disk.")).toBeInTheDocument();
  });

  it("marks the element as a reveal target", () => {
    render(<Reveal as="section">Hello</Reveal>);
    const el = screen.getByText("Hello");
    expect(el.tagName).toBe("SECTION");
    expect(el).toHaveAttribute("data-reveal");
  });

  it("does not hide content with an inline style in server HTML", () => {
    const html = renderToString(<Reveal delay={0.1}>Visible without JS</Reveal>);
    expect(html).toContain("Visible without JS");
    expect(html).toContain("data-reveal");
    expect(html).not.toMatch(/style="[^"]*opacity:\s*0/);
    expect(html).not.toMatch(/style="[^"]*transform/);
  });
});
