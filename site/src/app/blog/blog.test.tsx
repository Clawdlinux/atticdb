import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import BlogPostPage, { generateMetadata, generateStaticParams } from "@/app/blog/[slug]/page";
import BlogPage, { metadata as blogMetadata } from "@/app/blog/page";
import { GET as rss } from "@/app/blog/rss.xml/route";
import sitemap from "@/app/sitemap";

// The routes read content/blog. Here they read the fixtures, or whatever folder a test sets.
const blog = vi.hoisted(() => ({ dir: "" }));
vi.mock("@/content/blog", async (importOriginal) => {
  const real = await importOriginal<typeof import("@/content/blog")>();
  return {
    ...real,
    listPosts: () => real.listPosts(blog.dir),
    findPost: (slug: string) => real.findPost(slug, blog.dir),
    renderPost: (slug: string) => real.renderPost(slug, blog.dir),
  };
});

const FIXTURES = path.join(process.cwd(), "src/content/__fixtures__/blog");

function emptyDir(): string {
  return mkdtempSync(path.join(tmpdir(), "atticdb-blog-"));
}

function parse(html: string): Document {
  return new DOMParser().parseFromString(html, "text/html");
}

async function postPage(slug: string): Promise<Document> {
  return parse(renderToStaticMarkup(await BlogPostPage({ params: Promise.resolve({ slug }) })));
}

beforeEach(() => {
  blog.dir = FIXTURES;
});

describe("blog index", () => {
  it("uses the blog page metadata", () => {
    expect(blogMetadata.title).toBe("AtticDB engineering blog");
    expect(blogMetadata.alternates?.canonical).toBe("https://atticdb.com/blog");
  });

  it("lists each published post, newest first, with its date, reading time and description", async () => {
    const document = parse(renderToStaticMarkup(await BlogPage()));
    expect(document.querySelector("h1")?.textContent).toBe("Engineering blog");
    const entries = Array.from(document.querySelectorAll("li"));
    expect(entries.map((li) => li.querySelector("h2 a")?.getAttribute("href"))).toEqual([
      "/blog/group-commit",
      "/blog/acks-and-fsync",
    ]);
    expect(entries[0].querySelector("h2")?.textContent).toBe("Group commit & the <tape>");
    expect(entries[0].querySelector("time")?.getAttribute("dateTime")).toBe("2026-03-02");
    expect(entries[0].textContent).toContain("March 2, 2026 · 1 min read");
    expect(entries[0].textContent).toContain("How queued writers share 1 fdatasync & why.");
    expect(document.body.textContent).not.toContain("Unfinished");
    expect(document.body.textContent).not.toContain("No posts yet.");
  });

  it("shows the empty state when there are no posts", async () => {
    blog.dir = emptyDir();
    const document = parse(renderToStaticMarkup(await BlogPage()));
    expect(document.querySelector("ol")).toBeNull();
    expect(document.body.textContent).toContain("No posts yet.");
  });
});

describe("blog post page", () => {
  afterEach(() => vi.unstubAllEnvs());

  it("generates only published posts, and 1 placeholder when there are none", async () => {
    expect(await generateStaticParams()).toEqual([{ slug: "group-commit" }, { slug: "acks-and-fsync" }]);
    blog.dir = emptyDir();
    expect(await generateStaticParams()).toEqual([{ slug: "_none" }]);
    await expect(postPage("_none")).rejects.toThrow();
  });

  it("shows the title, date, author, reading time and rendered post", async () => {
    const document = await postPage("group-commit");
    expect(document.querySelector("h1")?.textContent).toBe("Group commit & the <tape>");
    expect(document.querySelector("time")?.getAttribute("dateTime")).toBe("2026-03-02");
    expect(document.body.textContent).toContain("March 2, 2026 · AtticDB team · 1 min read");
    expect(document.querySelector('h2[id="how-it-works"]')).not.toBeNull();
    expect(document.querySelector("img")?.getAttribute("src")).toBe("/blog/group-commit/diagram.svg");
  });

  it.each(["", "https://atticdb.com"])("ends with the Try it / Tell us links on site base %j", async (base) => {
    vi.stubEnv("SITE_BASE", base);
    const cta = (await postPage("acks-and-fsync")).querySelector('aside[aria-label="Try it and tell us"]');
    expect(Array.from(cta?.querySelectorAll("h2") ?? []).map((h) => h.textContent)).toEqual(["Try it", "Tell us"]);
    const links = Array.from(cta?.querySelectorAll("a") ?? []).map((a) => [a.textContent, a.getAttribute("href")]);
    expect(links).toEqual([
      ["Install AtticDB", `${base}/#install`],
      ["Open an issue", "https://github.com/Clawdlinux/atticdb/issues/new/choose"],
      ["AtticDB repo on GitHub", "https://github.com/Clawdlinux/atticdb"],
    ]);
  });

  it("adds BlogPosting structured data", async () => {
    const script = (await postPage("group-commit")).querySelector('script[type="application/ld+json"]');
    expect(JSON.parse(script!.textContent!)).toEqual({
      "@context": "https://schema.org",
      "@type": "BlogPosting",
      headline: "Group commit & the <tape>",
      description: "How queued writers share 1 fdatasync & why.",
      datePublished: "2026-03-02",
      author: { "@type": "Person", name: "AtticDB team" },
      publisher: { "@type": "Organization", name: "AtticDB", url: "https://atticdb.com" },
      url: "https://atticdb.com/blog/group-commit",
      mainEntityOfPage: "https://atticdb.com/blog/group-commit",
      keywords: "storage, group commit",
      image: "https://atticdb.com/blog/group-commit/cover.svg",
    });
  });

  it("gives a post article metadata with its own canonical and image", async () => {
    const metadata = await generateMetadata({ params: Promise.resolve({ slug: "group-commit" }) });
    const title = "Group commit & the <tape> · AtticDB blog";
    const image = "https://atticdb.com/blog/group-commit/cover.svg";
    expect(metadata.title).toEqual({ absolute: title });
    expect(metadata.description).toBe("How queued writers share 1 fdatasync & why.");
    expect(metadata.alternates?.canonical).toBe("https://atticdb.com/blog/group-commit");
    expect(metadata.openGraph).toMatchObject({
      title,
      url: "https://atticdb.com/blog/group-commit",
      type: "article",
      publishedTime: "2026-03-02",
      images: [{ url: image, alt: "Group commit & the <tape>" }],
    });
    expect(metadata.twitter).toMatchObject({ card: "summary_large_image", title, images: [image] });
  });

  it("falls back to the site image when a post sets none", async () => {
    const metadata = await generateMetadata({ params: Promise.resolve({ slug: "acks-and-fsync" }) });
    expect(metadata.openGraph).toMatchObject({ type: "article", images: [{ url: "/og.png" }] });
    expect(metadata.twitter).toMatchObject({ images: ["/og.png"] });
  });

  it("has no page for a draft", async () => {
    await expect(postPage("unfinished-draft")).rejects.toThrow();
    await expect(generateMetadata({ params: Promise.resolve({ slug: "unfinished-draft" }) })).rejects.toThrow();
  });
});

describe("blog feed", () => {
  it("serves RSS 2.0 with absolute links, newest first and no drafts", async () => {
    const res = await rss();
    expect(res.headers.get("content-type")).toBe("application/rss+xml; charset=utf-8");
    const xml = await res.text();
    expect(xml).toMatch(/^<\?xml version="1.0" encoding="UTF-8"\?>\n<rss version="2.0"/);
    expect([...xml.matchAll(/<link>([^<]+)<\/link>/g)].map((m) => m[1])).toEqual([
      "https://atticdb.com/blog",
      "https://atticdb.com/blog/group-commit",
      "https://atticdb.com/blog/acks-and-fsync",
    ]);
    expect(xml).toContain("<pubDate>Mon, 02 Mar 2026 00:00:00 GMT</pubDate>");
    expect(xml).not.toContain("unfinished-draft");
    expect(xml).not.toContain("Unfinished");
  });

  it("escapes & and < in titles and descriptions", async () => {
    const xml = await (await rss()).text();
    expect(xml).toContain("<title>Group commit &amp; the &lt;tape&gt;</title>");
    expect(xml).toContain("<description>How queued writers share 1 fdatasync &amp; why.</description>");
    expect(xml).not.toContain("& the <tape>");
  });

  it("keeps the newest 20 posts", async () => {
    blog.dir = emptyDir();
    for (let day = 1; day <= 21; day++) {
      const date = `2026-01-${String(day).padStart(2, "0")}`;
      writeFileSync(
        path.join(blog.dir, `day-${day}.md`),
        `---\ntitle: Day ${day}\ndescription: Post ${day}.\ndate: ${date}\nauthor: AtticDB team\ntags: test\n---\n\nBody.\n`,
      );
    }
    const xml = await (await rss()).text();
    const titles = [...xml.matchAll(/<item>\n\s*<title>([^<]+)<\/title>/g)].map((m) => m[1]);
    expect(titles).toHaveLength(20);
    expect(titles[0]).toBe("Day 21");
    expect(titles).not.toContain("Day 1");
  });
});

describe("sitemap", () => {
  it("lists the blog and every published post, never a draft", async () => {
    const entries = await sitemap();
    expect(entries.map(({ url }) => url)).toEqual(expect.arrayContaining(["https://atticdb.com/blog"]));
    expect(entries.slice(-2)).toEqual([
      { url: "https://atticdb.com/blog/group-commit", lastModified: "2026-03-02" },
      { url: "https://atticdb.com/blog/acks-and-fsync", lastModified: "2026-01-15" },
    ]);
    expect(entries.some(({ url }) => url.includes("unfinished-draft"))).toBe(false);
  });
});
