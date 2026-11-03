import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { findPost, formatDate, listPosts, readingMinutes, renderPost } from "@/content/blog";

const FIXTURES = path.join(process.cwd(), "src/content/__fixtures__/blog");

function blogDir(files: Record<string, string>): string {
  const dir = mkdtempSync(path.join(tmpdir(), "atticdb-blog-"));
  for (const [name, source] of Object.entries(files)) writeFileSync(path.join(dir, name), source);
  return dir;
}

const VALID = ["title: A post", "description: About it.", "date: 2026-02-01", "author: AtticDB team", "tags: one"];

function frontmatter(lines: string[]): string {
  return `---\n${lines.join("\n")}\n---\n\nBody.\n`;
}

describe("listPosts", () => {
  it("lists published posts newest first, whatever the file order", async () => {
    expect((await listPosts(FIXTURES)).map((post) => post.slug)).toEqual(["group-commit", "acks-and-fsync"]);
  });

  it("parses every frontmatter field, unquoting values and splitting tags", async () => {
    const [newer, older] = await listPosts(FIXTURES);
    expect(older).toEqual({
      slug: "acks-and-fsync",
      title: "Acks: what a durable write means",
      description: "Why AtticDB acks a write only after fdatasync.",
      date: "2026-01-15",
      author: "AtticDB team",
      tags: ["durability", "fsync"],
      draft: false,
      minutes: 1,
    });
    expect(newer).toMatchObject({
      title: "Group commit & the <tape>",
      tags: ["storage", "group commit"],
      image: "/blog/group-commit/cover.svg",
    });
  });

  it("is empty for a folder with no posts", async () => {
    expect(await listPosts(blogDir({ ".gitkeep": "" }))).toEqual([]);
  });

  it.each([
    ["no-author.md", VALID.filter((line) => !line.startsWith("author")), /no-author\.md: frontmatter is missing "author"/],
    ["no-tags.md", [...VALID.slice(0, 4), "tags: ,"], /no-tags\.md: frontmatter is missing "tags"/],
    ["bad-date.md", [...VALID.slice(0, 2), "date: 2026-02-30", ...VALID.slice(3)], /bad-date\.md: date "2026-02-30"/],
    ["typo.md", [...VALID, "drafts: true"], /typo\.md: unknown frontmatter field "drafts"/],
    ["bad-draft.md", [...VALID, "draft: yes"], /bad-draft\.md: draft must be true or false/],
    ["bad-image.md", [...VALID, "image: cover.svg"], /bad-image\.md: image "cover.svg" must be a path under \/public/],
  ])("rejects %s with an error naming the file", async (name, lines, error) => {
    await expect(listPosts(blogDir({ [name]: frontmatter(lines) }))).rejects.toThrow(error);
  });

  it("rejects a post without frontmatter and a file name that is not a slug", async () => {
    await expect(listPosts(blogDir({ "bare.md": "# Hi\n" }))).rejects.toThrow(/bare\.md: frontmatter must start the file/);
    await expect(listPosts(blogDir({ "Bad Name.md": frontmatter(VALID) }))).rejects.toThrow(/Bad Name\.md: the file name/);
  });
});

describe("findPost", () => {
  it("finds published posts and never drafts", async () => {
    expect(await findPost("acks-and-fsync", FIXTURES)).toMatchObject({ title: "Acks: what a durable write means" });
    expect(await findPost("unfinished-draft", FIXTURES)).toBeUndefined();
    expect(await findPost("missing", FIXTURES)).toBeUndefined();
  });
});

describe("renderPost", () => {
  it("renders like the docs: h2 ids, scrollable code and images from public/blog/<slug>/", async () => {
    const { post, html, headings } = await renderPost("group-commit", FIXTURES);
    expect(post.slug).toBe("group-commit");
    expect(headings).toEqual([{ id: "how-it-works", text: "How it works" }]);
    expect(html).toContain('<h2 id="how-it-works">How it works</h2>');
    expect(html).toContain('<img src="/blog/group-commit/diagram.svg" alt="Two writers, one sync">');
    expect(html).toContain('<pre tabindex="0">');
  });

  it("escapes raw HTML and sends docs: links to the docs host", async () => {
    const { html } = await renderPost("acks-and-fsync", FIXTURES);
    expect(html).not.toContain("<script");
    expect(html).toContain("&lt;script&gt;");
    expect(html).toContain('href="https://docs.atticdb.com/mcp"');
    expect(html).toContain('href="https://docs.atticdb.com/how-it-works#group-commit"');
    expect(html).not.toContain("docs:");
  });

  it("refuses drafts and unknown slugs", async () => {
    await expect(renderPost("unfinished-draft", FIXTURES)).rejects.toThrow('No blog post "unfinished-draft"');
    await expect(renderPost("missing", FIXTURES)).rejects.toThrow('No blog post "missing"');
  });
});

describe("readingMinutes", () => {
  const words = (n: number) => Array.from({ length: n }, () => "word").join(" ");

  it.each([
    ["no words", 1, ""],
    ["only whitespace", 1, " \n\t "],
    ["230 words", 1, words(230)],
    ["231 words", 2, words(231)],
    ["461 words", 3, words(461)],
  ])("reads %s in %i minute(s)", (_label, minutes, markdown) => {
    expect(readingMinutes(markdown)).toBe(minutes);
  });
});

describe("formatDate", () => {
  it("writes the date out in UTC, so it never shifts a day", () => {
    expect(formatDate("2026-03-02")).toBe("March 2, 2026");
  });
});
