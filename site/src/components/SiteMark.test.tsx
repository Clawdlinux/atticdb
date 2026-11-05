import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import SiteNav from "@/components/SiteNav";

describe("site mark", () => {
  it("draws the roof and box beside the wordmark, hidden from screen readers", () => {
    const document = new DOMParser().parseFromString(renderToStaticMarkup(<SiteNav />), "text/html");
    const home = document.querySelector('[aria-label="AtticDB home"]');
    const svg = home?.querySelector("svg");
    expect(svg?.getAttribute("aria-hidden")).toBe("true");
    expect(svg?.querySelectorAll("path")).toHaveLength(1);
    expect(svg?.querySelectorAll("rect")).toHaveLength(1);
    expect(home?.textContent).toBe("AtticDB");
  });

  it("stops the load animation and the lid lift for reduced motion", () => {
    const css = readFileSync("src/components/SiteMark.module.css", "utf8");
    const reduced = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));
    expect(reduced).toContain("animation: none");
    expect(reduced).toContain("transform: none");
  });
});
