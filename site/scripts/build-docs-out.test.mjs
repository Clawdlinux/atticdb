// @vitest-environment node
import { randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

let fixture;
let out;
let docsOut;

function write(path, content) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, content);
}

async function assemble() {
  const { buildDocsOut } = await import("./build-docs-out.mjs");
  buildDocsOut(out, docsOut);
}

beforeEach(() => {
  fixture = resolve("../.superpowers/test-fixtures", randomUUID());
  out = join(fixture, "out");
  docsOut = join(fixture, "out-docs");
  for (const [path, content] of Object.entries({
    "index.html": "MAIN",
    "index.txt": "MAIN RSC",
    "docs.html": "DOCS",
    "docs/mcp.html": "MCP",
    "docs/sitemap.xml": "DOCSMAP",
    "docs/robots.txt": "DOCSROBOTS",
    "sitemap.xml": "MAINMAP",
    "robots.txt": "MAINROBOTS",
    "pricing.html": "PRICING",
    "compare.html": "COMPARE",
    "benchmarks.html": "BENCHMARKS",
    "_next/static/x.js": "SHARED SCRIPT",
  })) write(join(out, path), content);
});

afterEach(() => rmSync(fixture, { recursive: true, force: true }));

describe("buildDocsOut", () => {
  it("puts docs content at the root before filesystem routing runs", async () => {
    await assemble();
    expect(readFileSync(join(docsOut, "index.html"), "utf8")).toBe("DOCS");
    expect(readFileSync(join(docsOut, "mcp.html"), "utf8")).toBe("MCP");
    expect(readFileSync(join(docsOut, "sitemap.xml"), "utf8")).toBe("DOCSMAP");
    expect(readFileSync(join(docsOut, "robots.txt"), "utf8")).toBe("DOCSROBOTS");
    expect(readFileSync(join(docsOut, "_next/static/x.js"), "utf8")).toBe("SHARED SCRIPT");
    for (const path of ["pricing.html", "compare.html", "benchmarks.html", "index.txt", "docs.html", "docs"]) {
      expect(existsSync(join(docsOut, path)), path).toBe(false);
    }
    expect(readFileSync(join(out, "index.html"), "utf8")).toBe("MAIN");
  });

  it("copies RSC files, nested docs directories, and shared assets", async () => {
    const copies = {
      "docs.txt": ["index.txt", "DOCS RSC"],
      "docs/mcp.txt": ["mcp.txt", "MCP RSC"],
      "docs/mcp/segment.txt": ["mcp/segment.txt", "NESTED RSC"],
      "images/a.jpg": ["images/a.jpg", "IMAGE"],
      "fonts/text.woff2": ["fonts/text.woff2", "FONT"],
      "icon.svg": ["icon.svg", "ICON"],
      "og.png": ["og.png", "OG"],
      "404.html": ["404.html", "NOT FOUND"],
    };
    for (const [source, [, content]] of Object.entries(copies)) write(join(out, source), content);
    write(join(out, "docs/unused.json"), "NOT EXPORTED");
    await assemble();
    for (const [destination, content] of Object.values(copies)) {
      expect(readFileSync(join(docsOut, destination), "utf8")).toBe(content);
    }
    expect(existsSync(join(docsOut, "unused.json"))).toBe(false);
  });

  it("clears stale files and directories before assembling again", async () => {
    write(join(docsOut, "pricing.html"), "STALE MAIN PAGE");
    write(join(docsOut, "old/nested.html"), "STALE DOC");
    await assemble();
    expect(existsSync(join(docsOut, "pricing.html"))).toBe(false);
    expect(existsSync(join(docsOut, "old"))).toBe(false);
    expect(readFileSync(join(docsOut, "index.html"), "utf8")).toBe("DOCS");
  });
});
