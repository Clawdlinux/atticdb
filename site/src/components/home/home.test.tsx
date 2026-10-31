import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import Home from "@/app/page";
import SiteFooter from "@/components/SiteFooter";
import { CREDITS } from "@/content/credits";
import { HOME_FAQ } from "@/content/faq";
import { GITHUB_URL, INSTALL_CMD } from "@/lib/site";
import Faq from "./Faq";
import Numbers, { homeFigures } from "./Numbers";
import Showcase from "./Showcase";

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
    expect(sections.length).toBeGreaterThanOrEqual(9);
    for (const section of sections) {
      if (section.querySelector("h1")) continue;
      expect(section.querySelector("h2"), section.className).not.toBeNull();
    }
  });

  it("puts alt text on every picture and carries at least 4", () => {
    const { container } = render(
      <>
        <Home />
        <SiteFooter />
      </>,
    );
    const imgs = [...container.querySelectorAll("img")];
    expect(imgs.length).toBeGreaterThanOrEqual(4);
    for (const img of imgs) expect(img.getAttribute("alt")?.trim()).toBeTruthy();
  });

  it("anchors #install on the closing call's install command, once", () => {
    const { container } = render(<Home />);
    const anchors = container.querySelectorAll("#install");
    expect(anchors).toHaveLength(1);
    expect(within(anchors[0] as HTMLElement).getByText(INSTALL_CMD).tagName).toBe("CODE");
    expect(screen.getByRole("link", { name: "Star on GitHub" })).toHaveAttribute(
      "href",
      GITHUB_URL,
    );
  });
});

describe("Showcase", () => {
  it("starts on the ext4 question with matching results", () => {
    render(<Showcase />);
    expect(screen.getByRole("searchbox", { name: "Search what your agent remembered" })).toHaveValue(
      "why did we pick ext4",
    );
  });

  it("filters the sample store by words as you type", () => {
    const { container } = render(<Showcase />);
    fireEvent.change(screen.getByRole("searchbox", { name: "Search what your agent remembered" }), {
      target: { value: "ext4" },
    });
    const results = container.querySelector('[aria-live="polite"]');
    expect(results).not.toBeNull();
    const rows = results!.querySelectorAll("li");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThanOrEqual(3);
    for (const row of rows) expect(row.textContent?.toLowerCase()).toContain("ext4");
  });

  it("keeps only matching rows present as the words narrow", () => {
    const { container } = render(<Showcase />);
    fireEvent.change(screen.getByRole("searchbox", { name: "Search what your agent remembered" }), {
      target: { value: "deploy region" },
    });
    const results = container.querySelector('[aria-live="polite"]')!;
    const present = results.querySelectorAll('li[data-present="true"]');
    expect(present.length).toBeGreaterThan(0);
    for (const row of present) expect(row.textContent?.toLowerCase()).toContain("deploy");
    for (const row of results.querySelectorAll('li[data-present="false"]')) {
      expect(row).toHaveAttribute("aria-hidden", "true");
    }
  });

  it("refills the input from an example", () => {
    render(<Showcase />);
    fireEvent.click(screen.getByRole("button", { name: "flaky test" }));
    expect(screen.getByRole("searchbox", { name: "Search what your agent remembered" })).toHaveValue("flaky test");
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

describe("Faq", () => {
  it("shows all 6 questions with their answers open", () => {
    const { container } = render(<Faq />);
    expect(container.querySelector("details")).toBeNull();
    expect(container.querySelector("[hidden]")).toBeNull();
    expect(screen.getAllByRole("heading", { level: 3 })).toHaveLength(6);
    for (const { q, a } of HOME_FAQ) {
      expect(screen.getByRole("heading", { level: 3, name: q })).toBeVisible();
      expect(screen.getByText(a)).toBeVisible();
    }
  });
});

describe("SiteFooter", () => {
  it("credits every picture with its creator, licence and source", () => {
    render(<SiteFooter />);
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
  });
});
