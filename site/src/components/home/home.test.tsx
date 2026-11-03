import { cleanup, render, screen, within } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import Home from "@/app/page";
import SiteFooter from "@/components/SiteFooter";
import { CREDITS } from "@/content/credits";
import { HOME_FAQ } from "@/content/faq";
import NotFound from "@/app/not-found";
import { AGENT_PROMPT, INSTALL_CMD } from "@/lib/site";
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
    expect(sections).toHaveLength(5);
    expect(sections.map((section) => section.querySelector("h1,h2")?.id)).toEqual([
      "hero-title", "demo-heading", "install-title", "numbers-title", "pricing-title",
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
    expect(section.textContent).toContain("Claude Code, Codex, GitHub Copilot CLI and VS Code");
  });

  it("leads the install section with the agent prompt, then the folded installer, then the questions", () => {
    const { container } = render(<Home />);
    const section = container.querySelector("#install") as HTMLElement;
    expect(section.querySelectorAll("h2")).toHaveLength(1);
    const ask = within(section).getByText("Paste this into your coding agent.");
    const prompt = within(section).getByText(AGENT_PROMPT);
    const details = section.querySelector("details") as HTMLDetailsElement;
    const question = within(section).getByRole("heading", { level: 3, name: HOME_FAQ[0].q });
    for (const [a, b] of [[ask, prompt], [prompt, details], [details, question]] as const) {
      expect(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    }
    // The prompt is outside the details, so it shows without opening anything.
    expect(prompt.closest("details")).toBeNull();
    expect(ask.closest("details")).toBeNull();
  });

  it("folds the installer behind a closed native details", () => {
    const { container } = render(<Home />);
    const section = container.querySelector("#install") as HTMLElement;
    const all = section.querySelectorAll("details");
    expect(all).toHaveLength(1);
    const details = all[0] as HTMLDetailsElement;
    expect(details.open).toBe(false);
    expect(details).not.toHaveAttribute("open");
    expect(details.id).toBe("commands");
    const summary = details.querySelector(":scope > summary")!;
    expect(summary.textContent).toBe("Or pick the commands yourself");
    const installer = [...section.querySelectorAll("code")].find((c) => c.textContent === INSTALL_CMD)!;
    expect(installer.closest("details")).toBe(details);
    expect(within(details).getByRole("button", { name: "Copy all", hidden: true })).toBeInTheDocument();
  });

  it("opens the commands for a #commands link", () => {
    window.history.replaceState(null, "", "#commands");
    try {
      const { container } = render(<Home />);
      expect((container.querySelector("#install details") as HTMLDetailsElement).open).toBe(true);
    } finally {
      window.history.replaceState(null, "", window.location.pathname);
    }
  });
});

describe("home demo", () => {
  it("shows the animated demo right after the honest line, with alt text and a still frame for reduced motion", () => {
    const { container } = render(<Home />);
    const hero = container.querySelector("section")!;
    const demo = hero.querySelector('[aria-labelledby="demo-heading"]') as HTMLElement;
    expect(demo.previousElementSibling).toHaveAttribute("data-ui", "honest");
    expect(demo.querySelector("h2")?.id).toBe("demo-heading");
    const img = within(demo).getByRole("img", { name: DEMO_ALT });
    expect(img).toHaveAttribute("src", "/demo/remember.svg");
    const sources = [...demo.querySelectorAll("picture source")].map((s) => [
      s.getAttribute("media"),
      s.getAttribute("srcset"),
    ]);
    // The first match wins, so the phone sources come first and each keeps a still frame.
    expect(sources).toEqual([
      ["(max-width: 40rem) and (prefers-reduced-motion: reduce)", "/demo/remember-narrow.svg#still"],
      ["(max-width: 40rem)", "/demo/remember-narrow.svg"],
      ["(prefers-reduced-motion: reduce)", "/demo/remember.svg#still"],
    ]);
    expect(within(demo).getByRole("link", { name: "Try it in 1 minute" })).toHaveAttribute(
      "href",
      "https://docs.atticdb.com/install#try-it-in-1-minute",
    );
  });

  it.each(["remember.svg", "remember-narrow.svg"])(
    "ships %s under 40 KB, looping, with a title and desc, and stopping for reduced motion",
    (file) => {
      const svg = readFileSync(`public/demo/${file}`, "utf8");
      expect(Buffer.byteLength(svg)).toBeLessThan(40 * 1024);
      expect(svg).toMatch(/<title[^>]*>[^<]+<\/title>/);
      expect(svg).toMatch(/<desc[^>]*>[^<]+<\/desc>/);
      expect(svg).toContain("animation-iteration-count: infinite");
      expect(svg).toMatch(/@media \(prefers-reduced-motion: reduce\) \{ \.a \{ animation: none; \} \}/);
      expect(svg).toContain("#still:target ~ * .a, #still:target ~ .a { animation: none; }");
      expect(svg).toContain('id="still"');
      expect(svg).not.toMatch(/<script/i);
      expect(svg).not.toContain("\u2014");
      expect(svg).toContain("AtticDB memory for repo shop,");
      expect(svg).toContain("This repo (shop):");
      expect(svg).toContain("pnpm add lodash");
    },
  );

  it("keeps the real command and output on the phone, wrapped to fit, with text 16px or more at 375px", () => {
    const svg = readFileSync("public/demo/remember-narrow.svg", "utf8");
    const viewBox = svg.match(/viewBox="0 0 (\d+) (\d+)"/)!;
    const width = Number(viewBox[1]);
    expect(width).toBeLessThanOrEqual(400);
    expect(Number(viewBox[2])).toBeGreaterThan(width);
    // 375px wide, less the page's 16px gutters.
    const scale = 343 / width;
    for (const [, size] of svg.matchAll(/font-size="([\d.]+)"/g)) {
      expect(Number(size) * scale).toBeGreaterThanOrEqual(16);
    }
    const lines = [...svg.matchAll(/<text[^>]*>([^<]*)<\/text>/g)].map((m) => m[1]);
    const at = (first: string, count: number) => {
      const i = lines.indexOf(first);
      expect(i, first).toBeGreaterThanOrEqual(0);
      return lines.slice(i, i + count);
    };
    // A backslash-continued command and output wrapped at its commas: the same bytes as remember.svg.
    expect(at("atticdb fact set \\", 5)).toEqual([
      "atticdb fact set \\",
      "&gt;",
      "--key repo/shop/rule \\",
      "&gt;",
      '--content "Use pnpm, not npm."',
    ]);
    expect(at('{"committed":true,', 3).join("")).toBe('{"committed":true,"deduplicated":false,"lsn":2}');
    expect(svg).toContain(
      'atticdb fact set --key repo/shop/rule --content "Use pnpm, not npm." It prints {"committed":true,"deduplicated":false,"lsn":2}',
    );
  });

  it("ships the wide SVG with the command and output on single lines", () => {
    const svg = readFileSync("public/demo/remember.svg", "utf8");
    // Real command syntax and real output shape, from atticdb on a throwaway store.
    expect(svg).toContain("atticdb fact set --key repo/shop/rule \\");
    expect(svg).toContain('--content "Use pnpm, not npm."');
    expect(svg).toContain('{"committed":true,"deduplicated":false,"lsn":2}');
    expect(svg).toContain("AtticDB memory for repo shop, a snapshot from chat start:");
    expect(svg).toContain("This repo (shop):");
  });
});

describe("home first screen", () => {
  it("has no benchmark jargon before the demo: the chip and the plug band come after it", () => {
    const { container } = render(<Home />);
    const hero = container.querySelector("section")!;
    const demo = hero.querySelector('[aria-labelledby="demo-heading"]')!;
    const range = document.createRange();
    range.setStartBefore(hero.firstChild!);
    range.setEndBefore(demo);
    const before = range.cloneContents().textContent!;
    expect(before).toContain("Your coding agent forgets everything between chats.");
    expect(before).toMatch(/Early\. Word search, not semantic\./);
    for (const word of [/\backed\b/i, /\backs\b/i, /µs/, /p50/i, /fdatasync/i, /Pull the plug/i, /Writers/, /Disk/]) {
      expect(before).not.toMatch(word);
    }
  });
});

describe("home FAQ", () => {
  it("answers what changes, where the data is, how to uninstall and whether it's free, inside #install", () => {
    const { container } = render(<Home />);
    expect(container.querySelector('[aria-labelledby="home-faq-title"]')).toBeNull();
    const install = container.querySelector("#install") as HTMLElement;
    const questions = within(install).getAllByRole("heading", { level: 3 });
    expect(questions.map((h) => h.textContent)).toEqual([
      "What changes in my agent?", "Where is my data?", "How do I uninstall it?", "Is it free?",
    ]);
    for (const { a } of HOME_FAQ) {
      const answer = within(install).getByText(a);
      expect(answer.closest("details")).toBeNull();
    }
    expect(install.querySelector("details h3")).toBeNull();
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
