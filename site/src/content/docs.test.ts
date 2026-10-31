import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { GET as docsRobots } from "@/app/docs/robots.txt/route";
import { GET as docsSitemap } from "@/app/docs/sitemap.xml/route";
import {
  DOC_PAGES,
  docCanonical,
  docsHref,
  renderDoc,
  renderMarkdown,
} from "@/content/docs";
import { docMetadata } from "@/lib/seo";

const DOCS_DIR = path.join(process.cwd(), "content/docs");
const SLUGS = ["", "install", "connect", "mcp", "cli", "http-api", "how-it-works", "windows"];

function fileFor(slug: string): string {
  return path.join(DOCS_DIR, `${slug || "index"}.md`);
}

function read(slug: string): string {
  return readFileSync(fileFor(slug), "utf8");
}

function stripFences(md: string): string {
  return md.replace(/^```[^\n]*\n[\s\S]*?^```$/gm, "");
}

function shellBlocks(md: string): string[] {
  return [...md.matchAll(/^```(?:sh|bash|shell)\n([\s\S]*?)^```$/gm)].map((m) => m[1]);
}

describe("DOC_PAGES", () => {
  it("lists the 8 pages in spec order", () => {
    expect(DOC_PAGES).toHaveLength(8);
    expect(DOC_PAGES.map((page) => page.slug)).toEqual(SLUGS);
  });

  it("names a README as the source of every page", () => {
    for (const page of DOC_PAGES) {
      expect(["README.md", "crates/atticdb/README.md"]).toContain(page.source);
    }
  });

  it("has a Markdown file for every page and no stray files", () => {
    const files = readdirSync(DOCS_DIR).filter((f) => f.endsWith(".md")).sort();
    expect(files).toEqual(SLUGS.map((s) => `${s || "index"}.md`).sort());
  });
});

describe("docsHref", () => {
  it.each([
    ["mcp", "/docs", "/docs/mcp"],
    ["mcp", "", "/mcp"],
    ["", "", "/"],
    ["", "/docs", "/docs"],
  ])("maps %j on base %j to %s", (slug, base, expected) => {
    expect(docsHref(slug, base)).toBe(expected);
  });

  it("defaults to the /docs base outside Vercel", () => {
    expect(docsHref("cli")).toBe("/docs/cli");
  });
});

describe("docCanonical", () => {
  it("uses the docs host without a trailing slash", () => {
    expect(docCanonical("")).toBe("https://docs.atticdb.com");
    expect(docCanonical("mcp")).toBe("https://docs.atticdb.com/mcp");
  });
});

describe("renderMarkdown", () => {
  it("escapes raw HTML", () => {
    const html = renderMarkdown("Hi.\n\n<script>alert(1)</script>\n\nAnd <b>inline</b>.");
    expect(html).not.toContain("<script");
    expect(html).not.toContain("<b>");
    expect(html).toContain("&lt;script&gt;");
  });

  it("rewrites docs: links, keeping the hash", () => {
    const html = renderMarkdown("[MCP](docs:mcp) and [set up](docs:install#set-up) and [home](docs:)");
    expect(html).toContain('href="/docs/mcp"');
    expect(html).toContain('href="/docs/install#set-up"');
    expect(html).toContain('href="/docs"');
    expect(html).not.toContain("docs:");
  });

  it("makes code blocks focusable scroll regions", () => {
    expect(renderMarkdown("```sh\natticdb doctor\n```")).toContain('<pre tabindex="0">');
  });
});

describe("renderDoc", () => {
  it("renders the MCP page with h2 ids", async () => {
    const { html, headings } = await renderDoc("mcp");
    expect(html).toContain("memory_append");
    expect(html).toMatch(/<h2 id="[a-z0-9-]+">/);
    expect(headings.length).toBeGreaterThan(0);
    expect(headings[0]).toEqual({ id: expect.stringMatching(/^[a-z0-9-]+$/), text: expect.any(String) });
  });

  it("gives every page unique heading ids and no unresolved docs: links", async () => {
    for (const slug of SLUGS) {
      const { html, headings } = await renderDoc(slug);
      const ids = headings.map((h) => h.id);
      expect(new Set(ids).size).toBe(ids.length);
      expect(html).not.toContain('href="docs:');
    }
  });

  it("splits the install page so the Installer sits after its first line", async () => {
    const { lead, body } = await renderDoc("install");
    expect(lead).toMatch(/^<p>[^\n]+<\/p>\s*$/);
    expect(body.startsWith("<h2")).toBe(true);
  });

  it("points docs: anchors at headings that exist", async () => {
    for (const slug of SLUGS) {
      for (const [, target, hash] of read(slug).matchAll(/\]\(docs:([a-z-]*)#([a-z0-9-]+)\)/g)) {
        const { headings } = await renderDoc(target);
        expect(headings.map((h) => h.id)).toContain(hash);
      }
    }
  });
});

describe("docs Markdown", () => {
  it.each(SLUGS)("page %j has no em dash, no h1 and ends with its source", (slug) => {
    const md = read(slug);
    const page = DOC_PAGES.find((p) => p.slug === slug)!;
    expect(md).not.toContain("—");
    expect(stripFences(md)).not.toMatch(/^# /m);
    const last = md.trimEnd().split("\n").at(-1)!;
    expect(last.startsWith("Source: [")).toBe(true);
    expect(last).toContain(`[${page.source}](https://github.com/Clawdlinux/atticdb/blob/main/${page.source})`);
  });

  it.each(SLUGS)("page %j links only to absolute URLs, docs pages or anchors", (slug) => {
    const links = [...stripFences(read(slug)).matchAll(/\]\(([^)\s]+)\)/g)].map((m) => m[1]);
    for (const href of links) expect(href).toMatch(/^(https:\/\/|docs:|#)/);
  });

  it.each(["cli", "connect"])("shell blocks on %s run only real verbs", (slug) => {
    const blocks = shellBlocks(read(slug));
    expect(blocks.length).toBeGreaterThan(0);
    for (const block of blocks) {
      for (const line of block.split("\n")) {
        const command = line.trim();
        // Blank lines, comments and variable assignments are not commands.
        if (!command || command.startsWith("#") || /^[A-Z_][A-Z0-9_]*=/.test(command)) continue;
        expect(command).toMatch(/^(atticdb |curl |cargo |export )/);
      }
    }
  });
});

describe("docMetadata", () => {
  it.each(SLUGS)("gives %j a short title, description and docs canonical", (slug) => {
    const page = DOC_PAGES.find((p) => p.slug === slug)!;
    const metadata = docMetadata(slug);
    const title = typeof metadata.title === "string" ? metadata.title : (metadata.title as { absolute: string }).absolute;
    expect(title).toBe(`${page.title} · AtticDB docs`);
    expect(title.length).toBeLessThanOrEqual(60);
    expect(metadata.description).toBe(page.description);
    expect(page.description.length).toBeLessThanOrEqual(155);
    expect(metadata.alternates?.canonical).toBe(docCanonical(slug));
    expect(metadata.openGraph).toMatchObject({ url: docCanonical(slug), title });
  });

  it("keeps titles and descriptions unique", () => {
    expect(new Set(DOC_PAGES.map((p) => p.title)).size).toBe(8);
    expect(new Set(DOC_PAGES.map((p) => p.description)).size).toBe(8);
  });
});

describe("docs host discovery", () => {
  it("lists every docs canonical in its own sitemap", async () => {
    const res = docsSitemap();
    expect(res.headers.get("content-type")).toContain("application/xml");
    const xml = await res.text();
    const locs = [...xml.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]);
    expect(locs).toEqual(SLUGS.map(docCanonical));
  });

  it("allows crawlers and points at the docs sitemap", async () => {
    const res = docsRobots();
    expect(res.headers.get("content-type")).toContain("text/plain");
    const text = await res.text();
    expect(text).toContain("User-Agent: *");
    expect(text).toContain("Allow: /");
    expect(text).toContain("Sitemap: https://docs.atticdb.com/sitemap.xml");
  });
});
