import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import Home from "@/app/page";
import SiteFooter from "@/components/SiteFooter";
import { CREDITS } from "@/content/credits";
import NotFound from "@/app/not-found";
import { INSTALL_CMD } from "@/lib/site";
import Numbers, { homeFigures } from "./Numbers";

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

afterEach(cleanup);

describe("home page", () => {
  it("has exactly one h1", () => {
    const { container } = render(<Home />);
    expect(container.querySelectorAll("h1")).toHaveLength(1);
  });

  it("gives every section after the hero an h2", () => {
    const { container } = render(<Home />);
    const sections = [...container.querySelectorAll("section")];
    expect(sections).toHaveLength(4);
    expect(sections.map((section) => section.querySelector("h1,h2")?.id)).toEqual([
      "hero-title", "install-title", "numbers-title", "pricing-title",
    ]);
    for (const section of sections) {
      if (section.querySelector("h1")) continue;
      expect(section.querySelector("h2"), section.className).not.toBeNull();
    }
  });

  it("keeps archival photos and their credits off home", () => {
    const { container } = render(<><Home /><SiteFooter /></>);
    expect(container.querySelector("picture")).toBeNull();
    expect(container.textContent).not.toContain("Pictures, via Wikimedia Commons:");
    const data = [...container.querySelectorAll('script[type="application/ld+json"]')]
      .map((script) => JSON.parse(script.textContent!)["@type"]);
    expect(data).toEqual(["SoftwareApplication"]);
  });

  it("uses the shared button shape for every solid action", () => {
    render(<Home />);
    for (const button of screen.getAllByRole("button")) expect(button).toHaveClass("button");
    for (const name of ["See pricing"]) {
      expect(screen.getByRole("link", { name })).toHaveClass("button", "button--ink");
    }
  });

  it("anchors #install once, on the installer section right after the hero", () => {
    const { container } = render(<Home />);
    const anchors = container.querySelectorAll("#install");
    expect(anchors).toHaveLength(1);
    const section = anchors[0] as HTMLElement;
    expect(section.tagName).toBe("SECTION");
    expect(within(section).getByRole("heading", { level: 2, name: "Install it your way" })).toBeInTheDocument();
    const codes = [...section.querySelectorAll("code")].map((c) => c.textContent);
    expect(codes).toContain(INSTALL_CMD);
    expect(section.previousElementSibling?.querySelector("h1")).not.toBeNull();
    expect(section.textContent).toContain("Claude Code, GitHub Copilot CLI, VS Code and Codex");
  });
});

describe("Numbers", () => {
  it("derives its figures from the bench rows", () => {
    expect(homeFigures()).toEqual({ ratio: "2.8", agentP50ms: "1.3", nvmeP50us: "94" });
  });

  it("states the ratio without crediting it to Redis alone", () => {
    render(<Numbers />);
    expect(
      screen.getByText(
        "times the durable acks of Redis, Valkey and DiceDB, at most. Premium SSD v2",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /benchmarks/i })).toHaveAttribute(
      "href",
      "/benchmarks",
    );
  });
});

describe("SiteFooter", () => {
  it("credits every 404 picture with its creator, licence and source", () => {
    render(<NotFound />);
    for (const c of CREDITS) {
      const link = screen.getByRole("link", { name: c.title });
      expect(link).toHaveAttribute("href", c.source);
      expect(link.closest("li")?.textContent).toContain(c.creator);
      expect(link.closest("li")?.textContent).toContain(c.licence);
    }
  });

  it("links the site and states the licence", () => {
    render(<SiteFooter />);
    for (const name of ["GitHub", "Docs", "Pricing", "Compare", "Benchmarks", "Issues"]) {
      expect(screen.getByRole("link", { name })).toBeInTheDocument();
    }
    expect(screen.getByText("AtticDB is Apache-2.0 or MIT.")).toBeInTheDocument();
    expect(screen.getByText("It's early. Don't run it in production.")).toBeVisible();
  });
});
