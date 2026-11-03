// Writes the files agents read, before every build, so they never drift from the docs:
//   public/llms.txt          the llmstxt.org index of the site and the docs
//   public/docs/<slug>.md    a Markdown copy of each docs page. build-docs-out.mjs puts them
//                            at the docs host's root, so they serve at docs.atticdb.com/<slug>.md
// Run: node --import ./scripts/ts-hooks.mjs scripts/build-agent-files.mjs
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { listPosts } from "../src/content/blog.ts";
import { DOC_PAGES } from "../src/content/docs.ts";
import { PAGES } from "../src/lib/seo.ts";
import { AGENT_PROMPT, DOCS_ORIGIN, GITHUB_URL, INSTALL_MD_URL, SITE_URL } from "../src/lib/site.ts";

const ROOT = new URL("../", import.meta.url);

/** @param {string} slug */
export function docFile(slug) {
  return `${slug || "index"}.md`;
}

/** @param {string} slug */
export function docMdUrl(slug) {
  return `${DOCS_ORIGIN}/${docFile(slug)}`;
}

// [text](docs:slug#hash) becomes [text](https://docs.atticdb.com/slug.md#hash).
/** @param {string} md */
export function rewriteDocLinks(md) {
  return md.replace(/\]\(docs:([a-z0-9-]*)(#[^)\s]*)?\)/g, (_, slug, hash = "") => `](${docMdUrl(slug)}${hash})`);
}

/**
 * The install page's HTML shows the agent prompt after its first line. Its copy does too.
 * @param {{ slug: string; title: string }} page
 * @param {string} md
 */
export function docMarkdown(page, md) {
  let body = rewriteDocLinks(md).trimEnd();
  if (page.slug === "install") body = body.replace("\n\n", `\n\n> ${AGENT_PROMPT}\n\n`);
  return `# ${page.title}\n\n${body}\n`;
}

/**
 * @param {{ slug: string; title: string; description: string }[]} pages
 * @param {{ slug: string; title: string; description: string }[]} posts
 */
export function llmsTxt(pages, posts) {
  const lines = [
    "# AtticDB",
    "",
    "> Local, durable memory for coding agents. One Rust binary. It acks a write only after fdatasync.",
    "> It wires into Claude Code, Copilot CLI, VS Code and Codex. MIT or Apache-2.0.",
    "> Early: fine for a laptop, not for production yet.",
    "",
    `To install and connect AtticDB for a user, follow ${INSTALL_MD_URL}.`,
    "",
    "## Install",
    "",
    `- [Install steps for agents](${INSTALL_MD_URL}): what to run, in order, to install AtticDB and connect the app you run in.`,
  ];
  const install = pages.find((p) => p.slug === "install");
  if (install) lines.push(`- [${install.title}](${docMdUrl(install.slug)}): ${install.description}`);
  lines.push("", "## Docs", "");
  for (const page of pages) lines.push(`- [${page.title}](${docMdUrl(page.slug)}): ${page.description}`);
  lines.push("", "## Optional", "");
  for (const [path, title] of [["/benchmarks", "Benchmarks"], ["/compare", "Compare"], ["/pricing", "Pricing"]]) {
    const page = PAGES.find((p) => p.path === path);
    lines.push(`- [${title}](${SITE_URL}${path})${page ? `: ${page.description}` : ""}`);
  }
  lines.push(`- [Source on GitHub](${GITHUB_URL}): the code, releases and issues.`);
  for (const post of posts) lines.push(`- [${post.title}](${SITE_URL}/blog/${post.slug}): ${post.description}`);
  return `${lines.join("\n")}\n`;
}

export async function writeAgentFiles(root = ROOT) {
  const docsOut = new URL("public/docs/", root);
  mkdirSync(docsOut, { recursive: true });
  for (const page of DOC_PAGES) {
    const md = readFileSync(new URL(`content/docs/${docFile(page.slug)}`, root), "utf8");
    writeFileSync(new URL(docFile(page.slug), docsOut), docMarkdown(page, md));
  }
  const posts = await listPosts(fileURLToPath(new URL("content/blog", root)));
  writeFileSync(new URL("public/llms.txt", root), llmsTxt(DOC_PAGES, posts));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  await writeAgentFiles();
}
