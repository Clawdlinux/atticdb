import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { docCanonical, render, type DocHeading } from "@/content/docs";

export const BLOG_TITLE = "AtticDB engineering blog";
export const BLOG_DESCRIPTION =
  "How AtticDB is built and measured: durable writes, the append-only tape, benchmarks and memory for coding agents.";

export type Post = {
  slug: string;
  title: string;
  description: string;
  /** YYYY-MM-DD */
  date: string;
  author: string;
  tags: string[];
  /** A path under /public, such as /blog/<slug>/cover.svg. */
  image?: string;
  draft: boolean;
  minutes: number;
};

export type RenderedPost = { post: Post; html: string; headings: DocHeading[] };

const BLOG_DIR = path.join(process.cwd(), "content/blog");
const REQUIRED = ["title", "description", "date", "author", "tags"] as const;
const FIELDS: string[] = [...REQUIRED, "image", "draft"];
const WORDS_PER_MINUTE = 230;

export function readingMinutes(markdown: string): number {
  const words = markdown.split(/\s+/).filter(Boolean).length;
  return Math.max(1, Math.ceil(words / WORDS_PER_MINUTE));
}

export function formatDate(date: string): string {
  return new Date(`${date}T00:00:00Z`).toLocaleDateString("en-US", {
    timeZone: "UTC",
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}

function unquote(value: string): string {
  const quoted = /^(["'])(.*)\1$/.exec(value);
  return quoted ? quoted[2] : value;
}

function isDate(value: string): boolean {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  const parsed = new Date(`${value}T00:00:00Z`);
  return !Number.isNaN(parsed.getTime()) && parsed.toISOString().startsWith(value);
}

// Frontmatter is "key: value" lines between --- lines. Any mistake throws with the file name,
// so a bad post fails the build instead of shipping half a page.
function parsePost(file: string, source: string): { post: Post; markdown: string } {
  const fail: (problem: string) => never = (problem) => {
    throw new Error(`Blog post ${file}: ${problem}`);
  };
  const match = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(source);
  if (!match) fail("frontmatter must start the file, between --- lines");

  const fields: Record<string, string> = {};
  for (const line of match[1].split(/\r?\n/)) {
    if (!line.trim()) continue;
    const colon = line.indexOf(":");
    if (colon === -1) fail(`frontmatter line ${JSON.stringify(line)} is not "key: value"`);
    const key = line.slice(0, colon).trim();
    if (!FIELDS.includes(key)) fail(`unknown frontmatter field "${key}"`);
    fields[key] = unquote(line.slice(colon + 1).trim());
  }

  for (const key of REQUIRED) {
    if (!fields[key]) fail(`frontmatter is missing "${key}"`);
  }
  if (!isDate(fields.date)) fail(`date ${JSON.stringify(fields.date)} is not a YYYY-MM-DD date`);
  const tags = fields.tags.split(",").map((tag) => tag.trim()).filter(Boolean);
  if (!tags.length) fail(`frontmatter is missing "tags"`);
  if (fields.image !== undefined && !fields.image.startsWith("/")) {
    fail(`image ${JSON.stringify(fields.image)} must be a path under /public, starting with /`);
  }
  if (fields.draft !== undefined && !["true", "false"].includes(fields.draft)) {
    fail(`draft must be true or false, not ${JSON.stringify(fields.draft)}`);
  }

  const slug = path.basename(file, ".md");
  if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(slug)) fail("the file name must be a lowercase-hyphenated slug");

  const markdown = source.slice(match[0].length);
  return {
    markdown,
    post: {
      slug,
      title: fields.title,
      description: fields.description,
      date: fields.date,
      author: fields.author,
      tags,
      ...(fields.image ? { image: fields.image } : {}),
      draft: fields.draft === "true",
      minutes: readingMinutes(markdown),
    },
  };
}

// Every published post with its Markdown, newest first. Drafts never leave this function.
async function readPosts(dir: string): Promise<{ post: Post; markdown: string }[]> {
  const files = (await readdir(dir)).filter((name) => name.endsWith(".md"));
  const posts = await Promise.all(
    files.map(async (name) => {
      const file = path.join(dir, name);
      return parsePost(file, await readFile(file, "utf8"));
    }),
  );
  return posts
    .filter(({ post }) => !post.draft)
    .sort((a, b) => b.post.date.localeCompare(a.post.date) || a.post.slug.localeCompare(b.post.slug));
}

export async function listPosts(dir = BLOG_DIR): Promise<Post[]> {
  return (await readPosts(dir)).map(({ post }) => post);
}

export async function findPost(slug: string, dir = BLOG_DIR): Promise<Post | undefined> {
  return (await listPosts(dir)).find((post) => post.slug === slug);
}

export async function renderPost(slug: string, dir = BLOG_DIR): Promise<RenderedPost> {
  const found = (await readPosts(dir)).find(({ post }) => post.slug === slug);
  if (!found) throw new Error(`No blog post ${JSON.stringify(slug)}`);
  // Posts are on the main host, so docs: links go to the docs host in full.
  return { post: found.post, ...render(found.markdown, docCanonical) };
}
