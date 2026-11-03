import { cleanup, render, screen, within } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import Home from "@/app/page";
import SiteFooter from "@/components/SiteFooter";
import { CREDITS } from "@/content/credits";
import { HOME_FAQ } from "@/content/faq";
import NotFound from "@/app/not-found";
import { INSTALL_CMD } from "@/lib/site";
import { DEMO_ALT } from "./Demo";
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
    expect(sections).toHaveLength(6);
    expect(sections.map((section) => section.querySelector("h1,h2")?.id)).toEqual([
      "hero-title", "demo-heading", "install-title", "numbers-title", "home-faq-title", "pricing-title",
    ]);
    for (const section of sections) {
      if (section.querySelector("h1")) continue;
      expect(section.querySelector("h2"), section.className).not.toBeNull();
    }
  });

  it("keeps archival photos and their credits off home", () => {
    const { container } = render(<><Home /><SiteFooter /></>);
    expect(container.querySelector('img[src^="/images/"], source[srcset*="/images/"]')).toBeNull();
    expect(container.textContent).not.toContain("Pictures, via Wikimedia Commons:");
    const data = [...container.querySelectorAll('script[type="application/ld+json"]')]
      .map((script) => JSON.parse(script.textContent!)["@type"]);
    expect(data).toEqual(["SoftwareApplication", "FAQPage"]);
  });

  it("uses the shared button shape for every solid action", () => {
    render(<Home />);
    for (const button of screen.getAllByRole("button")) expect(button).toHaveClass("button");
    for (const name of ["See pricing"]) {
      expect(screen.getByRole("link", { name })).toHaveClass("button", "button--ink");
    }
  });

  it("anchors #install once, on the installer section right after the demo", () => {
    const { container } = render(<Home />);
    const anchors = container.querySelectorAll("#install");
    expect(anchors).toHaveLength(1);
    const section = anchors[0] as HTMLElement;
    expect(section.tagName).toBe("SECTION");
    expect(within(section).getByRole("heading", { level: 2, name: "Install it your way" })).toBeInTheDocument();
    const codes = [...section.querySelectorAll("code")].map((c) => c.textContent);
    expect(codes).toContain(INSTALL_CMD);
    expect(section.previousElementSibling?.querySelector("#demo-heading")).not.toBeNull();
    expect(section.textContent).toContain("Claude Code, GitHub Copilot CLI, VS Code and Codex");
  });
});

describe("home demo", () => {
  it("shows the animated demo right under the hero, with alt text and a still frame for reduced motion", () => {
    const { container } = render(<Home />);
    const hero = container.querySelector("section")!;
    const demo = hero.nextElementSibling as HTMLElement;
    expect(demo.querySelector("h2")?.id).toBe("demo-heading");
    const img = within(demo).getByRole("img", { name: DEMO_ALT });
    expect(img).toHaveAttribute("src", "/demo/remember.svg");
    const still = demo.querySelector("picture source")!;
    expect(still).toHaveAttribute("media", "(prefers-reduced-motion: reduce)");
    expect(still).toHaveAttribute("srcset", "/demo/remember.svg#still");
    expect(within(demo).getByRole("link", { name: "Try it in 1 minute" })).toHaveAttribute(
      "href",
      "https://docs.atticdb.com/install#try-it-in-1-minute",
    );
  });

  it("ships an SVG under 40 KB that loops, has a title and desc, and stops for reduced motion", () => {
    const svg = readFileSync("public/demo/remember.svg", "utf8");
    expect(Buffer.byteLength(svg)).toBeLessThan(40 * 1024);
    expect(svg).toMatch(/<title[^>]*>[^<]+<\/title>/);
    expect(svg).toMatch(/<desc[^>]*>[^<]+<\/desc>/);
    expect(svg).toContain("animation-iteration-count: infinite");
    expect(svg).toMatch(/@media \(prefers-reduced-motion: reduce\) \{ \.a \{ animation: none; \} \}/);
    expect(svg).toContain('id="still"');
    expect(svg).not.toMatch(/<script/i);
    expect(svg).not.toContain("\u2014");
    // Real command syntax and real output shape, from atticdb on a throwaway store.
    expect(svg).toContain("atticdb fact set --key repo/shop/rule \\");
    expect(svg).toContain('--content "Use pnpm, not npm."');
    expect(svg).toContain('{"committed":true,"deduplicated":false,"lsn":2}');
    expect(svg).toContain("AtticDB memory for repo shop, a snapshot from chat start:");
    expect(svg).toContain("This repo (shop):");
  });
});

describe("home FAQ", () => {
  it("answers what changes, where the data is, how to uninstall and whether it's free", () => {
    const { container } = render(<Home />);
    const faq = container.querySelector('[aria-labelledby="home-faq-title"]') as HTMLElement;
    for (const q of ["What changes in my agent?", "Where is my data?", "How do I uninstall it?", "Is it free?"]) {
      expect(within(faq).getByRole("heading", { level: 3, name: q })).toBeInTheDocument();
    }
    for (const { a } of HOME_FAQ) expect(within(faq).getByText(a)).toBeInTheDocument();
  });

  it("uses no jargon from the onboarding audit", () => {
    const text = HOME_FAQ.map(({ q, a }) => `${q} ${a}`).join(" ");
    for (const word of [/fsync/i, /fdatasync/i, /\backs?\b/i, /\backed\b/i, /\bMCP\b/, /harness/i, /\btape\b/i, /\bLSN\b/, /\btenant\b/i, /\bhook\b/i, /\bp99\b/, /µs/, /\bturns?\b/i, /\bfacts?\b/i, /\bsessions?\b/i]) {
      expect(text).not.toMatch(word);
    }
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
