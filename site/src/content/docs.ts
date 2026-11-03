import { readFile } from "node:fs/promises";
import path from "node:path";
import { Marked, type Tokens } from "marked";
import { DOCS_ORIGIN } from "@/lib/site";

export type DocSlug = "" | "install" | "connect" | "mcp" | "cli" | "http-api" | "how-it-works" | "windows";

export type DocPage = {
  slug: DocSlug;
  title: string;
  description: string;
  source: "README.md" | "crates/atticdb/README.md";
};

export type DocHeading = { id: string; text: string };

export type RenderedDoc = {
  html: string;
  /** The HTML before the first h2. */
  lead: string;
  /** The HTML from the first h2 on. */
  body: string;
  headings: DocHeading[];
};

export const DOC_PAGES: DocPage[] = [
  {
    slug: "",
    title: "Overview",
    description:
      "AtticDB is an agent memory database in Rust. It stores turn history and keyed facts. A write is acked only after it is synced to disk.",
    source: "README.md",
  },
  {
    slug: "install",
    title: "Install",
    description:
      "Install AtticDB on macOS, Linux or Windows. atticdb setup connects your apps, and atticdb doctor checks the install.",
    source: "README.md",
  },
  {
    slug: "connect",
    title: "Connect your apps",
    description:
      "How atticdb connect adds AtticDB to VS Code, Copilot CLI, Claude Code and Codex, and what each part writes.",
    source: "crates/atticdb/README.md",
  },
  {
    slug: "mcp",
    title: "MCP tools",
    description:
      "atticdb mcp gives the model memory_append, memory_recent, memory_search and the fact tools. How it finds the home and the server.",
    source: "crates/atticdb/README.md",
  },
  {
    slug: "cli",
    title: "Shell verbs and studio",
    description:
      "Run the same 6 memory tools from a shell. atticdb studio serves a read-only web page about your memory, on loopback.",
    source: "README.md",
  },
  {
    slug: "http-api",
    title: "Server and HTTP API",
    description:
      "atticdb serve listens on loopback TCP and a Unix socket. Its HTTP routes, write errors and the outcome rule for retries.",
    source: "crates/atticdb/README.md",
  },
  {
    slug: "how-it-works",
    title: "How it works",
    description:
      "1 append-only tape is both WAL and store. Group commit, CRC32C frames, idempotent writes and the read-only floor.",
    source: "README.md",
  },
  {
    slug: "windows",
    title: "Windows",
    description:
      "AtticDB on Windows x86_64: the profile rule, loopback TCP with a proof, weaker durability, and what each app gets.",
    source: "crates/atticdb/README.md",
  },
];

const DOCS_DIR = path.join(process.cwd(), "content/docs");

// next.config.ts inlines DOCS_BASE: "" on Vercel, where the docs host serves /docs/* at its root.
export function docsHref(slug: string, base = process.env.DOCS_BASE ?? "/docs"): string {
  return slug ? `${base}/${slug}` : base || "/";
}

export function docCanonical(slug: string): string {
  return slug ? `${DOCS_ORIGIN}/${slug}` : DOCS_ORIGIN;
}

export function findDoc(slug: string): DocPage | undefined {
  return DOC_PAGES.find((page) => page.slug === slug);
}

const ENTITIES: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };

export function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/g, (c) => ENTITIES[c]);
}

function decodeHtml(html: string): string {
  return html
    .replace(/<[^>]*>/g, "")
    .replace(/&(amp|lt|gt|quot|#39);/g, (entity) => {
      const found = Object.entries(ENTITIES).find(([, e]) => e === entity);
      return found ? found[0] : entity;
    });
}

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

function rewriteDocsLink(href: string, docLink: (slug: string) => string): string {
  const [target, hash] = href.slice("docs:".length).split("#");
  return docLink(target) + (hash ? `#${hash}` : "");
}

// docLink maps a docs: link target to its URL. Docs pages link within their own host.
// Blog posts live on the main host, so they pass docCanonical.
export function render(
  md: string,
  docLink: (slug: string) => string = (slug) => docsHref(slug),
): { html: string; headings: DocHeading[] } {
  const headings: DocHeading[] = [];
  const marked = new Marked({
    gfm: true,
    walkTokens(token) {
      if (token.type === "link" && token.href.startsWith("docs:")) {
        token.href = rewriteDocsLink(token.href, docLink);
      }
    },
    renderer: {
      // The READMEs and blog posts never need raw HTML, so any that slips in shows as text.
      html({ text }) {
        return escapeHtml(text);
      },
      heading({ tokens, depth }: Tokens.Heading) {
        const inner = this.parser.parseInline(tokens);
        if (depth !== 2) return `<h${depth}>${inner}</h${depth}>\n`;
        const text = decodeHtml(inner);
        const base = slugify(text) || "section";
        let id = base;
        for (let n = 2; headings.some((h) => h.id === id); n++) id = `${base}-${n}`;
        headings.push({ id, text });
        return `<h2 id="${id}">${inner}</h2>\n`;
      },
    },
  });

  const html = (marked.parse(md, { async: false }) as string)
    .replaceAll("<pre><code", '<pre tabindex="0"><code')
    .replaceAll("<table>", '<div class="doc-table" tabindex="0" role="region" aria-label="Table"><table>')
    .replaceAll("</table>", "</table></div>");

  return { html, headings };
}

export function renderMarkdown(md: string): string {
  return render(md).html;
}

export async function renderDoc(slug: string): Promise<RenderedDoc> {
  const md = await readFile(path.join(DOCS_DIR, `${slug || "index"}.md`), "utf8");
  const { html, headings } = render(md);
  const split = html.indexOf("<h2");
  return {
    html,
    headings,
    lead: split === -1 ? html : html.slice(0, split),
    body: split === -1 ? "" : html.slice(split),
  };
}
