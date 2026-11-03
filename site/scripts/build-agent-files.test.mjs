// @vitest-environment node
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { listPosts } from "../src/content/blog";
import { DOC_PAGES } from "../src/content/docs";
import { AGENT_PROMPT, INSTALL_MD_URL } from "../src/lib/site";
import { docFile, docMarkdown, docMdUrl, llmsTxt, rewriteDocLinks } from "./build-agent-files.mjs";

const read = (slug) => readFileSync(new URL(`../content/docs/${docFile(slug)}`, import.meta.url), "utf8");

describe("llms.txt", () => {
  it("follows the llmstxt.org shape: title, summary, install line, then sections", async () => {
    const txt = llmsTxt(DOC_PAGES, await listPosts());
    const lines = txt.split("\n");
    expect(lines[0]).toBe("# AtticDB");
    expect(lines[2].startsWith("> ")).toBe(true);
    for (const words of ["fdatasync", "Claude Code, Copilot CLI, VS Code and Codex", "MIT or Apache-2.0", "not for production"]) {
      expect(txt).toContain(words);
    }
    const install = txt.indexOf(`To install and connect AtticDB for a user, follow ${INSTALL_MD_URL}.`);
    const sections = ["## Install", "## Docs", "## Optional"].map((h) => txt.indexOf(`\n${h}\n`));
    expect(install).toBeGreaterThan(0);
    expect(sections.every((at) => at > install)).toBe(true);
    expect([...sections].sort((a, b) => a - b)).toEqual(sections);
  });

  it("lists install.md first under Install, then the docs install page", () => {
    const txt = llmsTxt(DOC_PAGES, []);
    const block = txt.split("\n## Install\n")[1].split("\n## ")[0].trim().split("\n");
    expect(block[0]).toMatch(new RegExp(`^- \\[[^\\]]+\\]\\(${INSTALL_MD_URL.replace(/\./g, "\\.")}\\): `));
    expect(block[1]).toContain("(https://docs.atticdb.com/install.md)");
  });

  it("lists every docs page with an absolute .md URL and its description", () => {
    const txt = llmsTxt(DOC_PAGES, []);
    const docs = txt.split("\n## Docs\n")[1].split("\n## ")[0];
    for (const page of DOC_PAGES) {
      expect(docs).toContain(`- [${page.title}](${docMdUrl(page.slug)}): ${page.description}`);
    }
    expect(docMdUrl("")).toBe("https://docs.atticdb.com/index.md");
    expect(docMdUrl("mcp")).toBe("https://docs.atticdb.com/mcp.md");
  });

  it("puts the main pages, the repo and the blog posts under Optional", async () => {
    const posts = await listPosts();
    const optional = llmsTxt(DOC_PAGES, posts).split("\n## Optional\n")[1];
    for (const url of [
      "https://atticdb.com/benchmarks",
      "https://atticdb.com/compare",
      "https://atticdb.com/pricing",
      "https://github.com/Clawdlinux/atticdb",
      ...posts.map((p) => `https://atticdb.com/blog/${p.slug}`),
    ]) {
      expect(optional).toContain(`](${url})`);
    }
  });

  it("has no docs: scheme, no relative links and no em dash", async () => {
    const txt = llmsTxt(DOC_PAGES, await listPosts());
    expect(txt).not.toContain("docs:");
    expect(txt).not.toContain("—");
    for (const [, href] of txt.matchAll(/\]\(([^)]*)\)/g)) expect(href).toMatch(/^https:\/\//);
  });
});

describe("docs Markdown copies", () => {
  it("rewrites docs: links, anchors kept, to absolute .md URLs", () => {
    expect(rewriteDocLinks("[a](docs:mcp) [b](docs:install#set-up) [c](docs:) [d](#x)")).toBe(
      "[a](https://docs.atticdb.com/mcp.md) [b](https://docs.atticdb.com/install.md#set-up) [c](https://docs.atticdb.com/index.md) [d](#x)",
    );
  });

  it.each(DOC_PAGES.map((p) => [p.slug, p]))("page %j starts with its title and has no docs: link", (slug, page) => {
    const md = docMarkdown(page, read(slug));
    expect(md.startsWith(`# ${page.title}\n\n`)).toBe(true);
    expect(md).not.toMatch(/\]\(docs:/);
    expect(md).not.toContain("docs:");
  });

  it("puts the agent prompt after the install page's first line, as the HTML page does", () => {
    const page = DOC_PAGES.find((p) => p.slug === "install");
    const md = docMarkdown(page, read("install"));
    const [, first, quote] = md.split("\n\n");
    expect(first).toBe(read("install").split("\n")[0]);
    expect(quote).toBe(`> ${AGENT_PROMPT}`);
  });
});
