import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import SiteNav from "@/components/SiteNav";
import SiteFooter from "@/components/SiteFooter";

describe("shared site links", () => {
  afterEach(() => vi.unstubAllEnvs());

  it.each(["", "https://atticdb.com"])("uses main-site base %j in navigation and footer", (base) => {
    vi.stubEnv("SITE_BASE", base);
    const html = renderToStaticMarkup(<><SiteNav /><SiteFooter /></>);
    const document = new DOMParser().parseFromString(html, "text/html");
    expect(document.querySelector('[aria-label="AtticDB home"]')?.getAttribute("href")).toBe(`${base}/`);
    expect(Array.from(document.querySelectorAll("a")).find((a) => a.textContent === "Install")?.getAttribute("href"))
      .toBe(`${base}/#install`);

    for (const name of ["Main navigation", "Footer"]) {
      const links = Array.from(document.querySelectorAll(`nav[aria-label="${name}"] a`));
      for (const [label, path] of [["Pricing", "/pricing"], ["Compare", "/compare"], ["Benchmarks", "/benchmarks"]]) {
        expect(links.find((a) => a.textContent === label)?.getAttribute("href")).toBe(`${base}${path}`);
      }
      expect(links.find((a) => a.textContent === "Docs")?.getAttribute("href")).toBe("https://docs.atticdb.com");
    }
    expect(document.querySelector('a[href="#main"]')?.textContent).toBe("Back to top");
    expect(document.querySelector('[aria-label="AtticDB on GitHub"]')?.getAttribute("href"))
      .toBe("https://github.com/Clawdlinux/atticdb");
  });
});
