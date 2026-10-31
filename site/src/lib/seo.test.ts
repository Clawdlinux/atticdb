import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import NotFound from "@/app/not-found";
import robots from "@/app/robots";
import sitemap from "@/app/sitemap";
import JsonLd from "@/components/JsonLd";
import { HOME_FAQ, PRICING_FAQ } from "@/content/faq";
import { PAGES, faqJsonLd, pageMetadata, softwareAppJsonLd } from "@/lib/seo";
import { absoluteUrl, GITHUB_URL, SITE_URL } from "@/lib/site";

const expectedPages = [
  ["/", "AtticDB: durable memory for coding agents"],
  ["/pricing", "AtticDB pricing: free locally, cloud planned"],
  ["/compare", "AtticDB vs Mem0, Zep, Letta and Supermemory"],
  ["/benchmarks", "AtticDB benchmarks: durable acks vs Redis and SQLite"],
];

const expectedDescriptions = [
  "AtticDB is an open-source memory database for coding agents. One Rust binary. Every write is acked after fdatasync. No model calls.",
  "AtticDB is free on your own disk, forever. Cloud sync, backups and team memory are planned. No per-memory or per-call fees.",
  "How AtticDB compares with Mem0, Zep, Letta, Supermemory, Redis Agent Memory, Cognee and Engram. Sourced facts, including where we lose.",
  "Durable acks per second and latency for AtticDB, Redis, Valkey, DiceDB and SQLite on NVMe and Premium SSD v2. 7 runs, raw data linked.",
];

describe("page metadata", () => {
  it("lists exactly the 4 public pages with the specified titles", () => {
    expect(PAGES.map(({ path, title }) => [path, title])).toEqual(expectedPages);
  });

  it("keeps titles unique and within 60 characters", () => {
    expect(new Set(PAGES.map(({ title }) => title)).size).toBe(4);
    for (const { title } of PAGES) expect(title.length).toBeLessThanOrEqual(60);
  });

  it("keeps the specified descriptions unique and within 155 characters", () => {
    expect(PAGES.map(({ description }) => description)).toEqual(expectedDescriptions);
    expect(new Set(PAGES.map(({ description }) => description)).size).toBe(4);
    for (const { description } of PAGES) expect(description.length).toBeLessThanOrEqual(155);
  });

  it("uses an absolute home title without a template suffix", () => {
    expect(pageMetadata("/").title).toEqual({ absolute: expectedPages[0][1] });
  });

  it("uses the pricing canonical without a trailing slash", () => {
    expect(pageMetadata("/pricing").alternates?.canonical).toBe("https://atticdb.com/pricing");
  });

  it("gives every page its own canonical and social metadata", () => {
    for (const { path, title, description } of PAGES) {
      const metadata = pageMetadata(path);
      expect(metadata.description).toBe(description);
      expect(metadata.alternates?.canonical).toBe(absoluteUrl(path));
      if (path !== "/") expect(metadata.title).toBe(title);
      expect(metadata.openGraph).toEqual({
        title,
        description,
        url: absoluteUrl(path),
        siteName: "AtticDB",
        type: "website",
        images: [{ url: "/og.png", width: 1200, height: 630, alt: expect.any(String) }],
      });
      expect(metadata.twitter).toEqual({
        card: "summary_large_image",
        title,
        description,
        images: ["/og.png"],
      });
    }
  });
});

describe("structured data", () => {
  it("describes the free, open-source application", () => {
    expect(softwareAppJsonLd()).toEqual({
      "@context": "https://schema.org",
      "@type": "SoftwareApplication",
      name: "AtticDB",
      applicationCategory: "DeveloperApplication",
      operatingSystem: "macOS, Linux, Windows",
      url: SITE_URL,
      downloadUrl: `${GITHUB_URL}/releases/latest`,
      license: "Apache-2.0 OR MIT",
      offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
      codeRepository: GITHUB_URL,
    });
    expect(softwareAppJsonLd().license).toContain("Apache-2.0");
  });

  it.each([
    ["home", HOME_FAQ, 6],
    ["pricing", PRICING_FAQ, 4],
  ] as const)("preserves every visible %s FAQ", (_page, items, count) => {
    const data = faqJsonLd(items);
    expect(data["@context"]).toBe("https://schema.org");
    expect(data["@type"]).toBe("FAQPage");
    expect(data.mainEntity).toHaveLength(count);
    expect(data.mainEntity).toEqual(items.map(({ q, a }) => ({
      "@type": "Question",
      name: q,
      acceptedAnswer: { "@type": "Answer", text: a },
    })));
  });

  it("escapes less-than signs without changing parsed JSON", () => {
    const data = { text: '</script><script>alert("injected")</script>', value: "a < b" };
    const html = renderToStaticMarkup(createElement(JsonLd, { data }));
    const document = new DOMParser().parseFromString(html, "text/html");
    const scripts = document.querySelectorAll("script");
    expect(scripts).toHaveLength(1);
    expect(scripts[0].type).toBe("application/ld+json");
    expect(scripts[0].textContent).not.toContain("<");
    expect(scripts[0].textContent).toContain("\\u003c");
    expect(JSON.parse(scripts[0].textContent!)).toEqual(data);
  });
});


describe("static discovery routes", () => {
  it("lists only the 4 public canonical URLs in the sitemap", () => {
    expect(sitemap().map(({ url }) => url)).toEqual([
      "https://atticdb.com/",
      "https://atticdb.com/pricing",
      "https://atticdb.com/compare",
      "https://atticdb.com/benchmarks",
    ]);
  });

  it("allows crawlers and points to the generated sitemap", () => {
    expect(robots()).toEqual({
      rules: { userAgent: "*", allow: "/" },
      sitemap: "https://atticdb.com/sitemap.xml",
    });
  });

  it("renders an accessible 404 heading and a home link without JavaScript", () => {
    const html = renderToStaticMarkup(createElement(NotFound));
    const document = new DOMParser().parseFromString(html, "text/html");
    expect(document.querySelectorAll("h1")).toHaveLength(1);
    expect(document.querySelector("h1")?.getAttribute("aria-label")).toBe("Nothing in the attic.");
    expect(document.querySelector("p")?.textContent).toBe(
      "That page isn't here. It may have moved, or it never existed.",
    );
    expect(document.querySelector("a")?.getAttribute("href")).toBe("/");
    expect(document.querySelector("a")?.textContent).toBe("Back to the home page");
  });
});
