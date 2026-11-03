import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

type Route = {
  source: string;
  destination: string;
  has?: { type: string; value: string }[];
  permanent?: boolean;
};

type VercelConfig = {
  $schema: string;
  framework: null;
  buildCommand: string;
  outputDirectory: string;
  installCommand: string;
  cleanUrls: boolean;
  trailingSlash: boolean;
  redirects: Route[];
  rewrites?: Route[];
  headers: { source: string; headers: { key: string; value: string }[] }[];
};

function config(file = "vercel.json"): VercelConfig {
  return JSON.parse(readFileSync(resolve(process.cwd(), file), "utf8"));
}

function sourceRegex(source: string): RegExp {
  if (source === "/:path*") return /^\/(.*)$/;
  if (source.endsWith("/:path*")) {
    return new RegExp(`^${source.slice(0, -"/:path*".length)}(?:/(.*))?$`);
  }
  if (source.startsWith("/:path(")) {
    return new RegExp(`^/(${source.slice(7, -1)})$`);
  }
  return new RegExp(`^${source.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`);
}

function routeFor(routes: Route[], host: string, path: string) {
  for (const route of routes) {
    if (!(route.has ?? []).every((condition) => condition.type === "host" && condition.value === host)) continue;
    const match = sourceRegex(route.source).exec(path);
    if (match) {
      return {
        destination: route.destination.replace(/:path\*?/g, match[1] ?? ""),
        permanent: route.permanent,
      };
    }
  }
}

const CSP = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

describe("Vercel static export", () => {
  it("builds the exported files without a framework runtime", () => {
    expect(config()).toMatchObject({
      $schema: "https://openapi.vercel.sh/vercel.json",
      framework: null,
      buildCommand: "npm run build",
      outputDirectory: "out",
      installCommand: "npm ci",
      cleanUrls: true,
      trailingSlash: false,
    });
  });

  it("preserves security headers and immutable asset caching", () => {
    expect(config().headers).toEqual([
      {
        source: "/(.*)",
        headers: [
          { key: "X-Content-Type-Options", value: "nosniff" },
          { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
          { key: "Content-Security-Policy", value: CSP },
        ],
      },
      {
        source: "/_next/static/(.*)",
        headers: [{ key: "Cache-Control", value: "public, max-age=31536000, immutable" }],
      },
      {
        source: "/(.*)\\.md",
        headers: [{ key: "Content-Type", value: "text/markdown; charset=utf-8" }],
      },
      {
        source: "/llms.txt",
        headers: [{ key: "Content-Type", value: "text/plain; charset=utf-8" }],
      },
    ]);
  });

  it.each(["vercel.json", "vercel.docs.json"])("%s serves the agent files as text, under the security headers", (file) => {
    const rules = config(file).headers;
    // Vercel sources are path-to-regexp. These use only groups, escapes and literals, so they read as regexes.
    const headersFor = (path: string) =>
      Object.fromEntries(
        rules
          .filter((rule) => new RegExp(`^${rule.source}$`).test(path))
          .flatMap((rule) => rule.headers.map((h) => [h.key, h.value])),
      );
    for (const path of ["/install.md", "/mcp.md", "/index.md"]) {
      expect(headersFor(path)).toMatchObject({
        "Content-Type": "text/markdown; charset=utf-8",
        "X-Content-Type-Options": "nosniff",
        "Content-Security-Policy": CSP,
      });
    }
    expect(headersFor("/llms.txt")).toMatchObject({
      "Content-Type": "text/plain; charset=utf-8",
      "X-Content-Type-Options": "nosniff",
    });
    for (const path of ["/", "/mcp", "/robots.txt", "/mdfile"]) expect(headersFor(path)["Content-Type"]).toBeUndefined();
  });
});

describe("host redirects", () => {
  it.each(["atticdb.com", "www.atticdb.com"])("sends %s docs paths directly to the docs host", (host) => {
    for (const [path, destination] of [
      ["/docs", "https://docs.atticdb.com/"],
      ["/docs/", "https://docs.atticdb.com/"],
      ["/docs/mcp", "https://docs.atticdb.com/mcp"],
      ["/docs/nested/page", "https://docs.atticdb.com/nested/page"],
    ]) {
      expect(routeFor(config().redirects, host, path)).toEqual({ destination, permanent: true });
    }
  });

  it.each(["/", "/pricing", "/nested/page"])("canonicalizes www%s to the main host", (path) => {
    expect(routeFor(config().redirects, "www.atticdb.com", path)).toEqual({
      destination: `https://atticdb.com${path}`,
      permanent: true,
    });
    expect(routeFor(config().redirects, "atticdb.com", path)).toBeUndefined();
  });

  it("has an explicit www root redirect, since Vercel's /:path* skipped / in production", () => {
    const root = config().redirects.find(
      (rule: { source: string; has?: { value: string }[] }) =>
        rule.source === "/" && rule.has?.[0]?.value === "www.atticdb.com",
    );
    expect(root).toMatchObject({ destination: "https://atticdb.com/", permanent: true });
  });

  it.each(["/", "/mcp", "/docs", "/docs/mcp", "/pricing"])("leaves preview-host path %s alone", (path) => {
    expect(routeFor(config().redirects, "preview.vercel.app", path)).toBeUndefined();
  });

  it.each(["/", "/mcp", "/sitemap.xml", "/robots.txt"])("does not redirect docs page %s", (path) => {
    expect(routeFor(config().redirects, "docs.atticdb.com", path)).toBeUndefined();
  });
});

describe("separate docs project", () => {
  it("removes docs-host routing from the main project", () => {
    const main = config();
    for (const rule of [...main.redirects, ...(main.rewrites ?? [])]) {
      expect(rule.has?.some((condition) => condition.type === "host" && condition.value === "docs.atticdb.com"))
        .not.toBe(true);
    }
  });

  it("does not shadow root static files with rewrites", () => {
    const rootStaticPaths = ["/", "/docs", "/pricing", "/compare", "/benchmarks", "/sitemap.xml", "/robots.txt"];
    for (const rule of config().rewrites ?? []) {
      for (const path of rootStaticPaths) {
        expect(sourceRegex(rule.source).test(path), `The filesystem serves ${path} before rewrites`).toBe(false);
      }
      expect(rootStaticPaths).not.toContain(rule.destination);
    }
  });

  it("builds a docs-only export without a framework runtime", () => {
    expect(config("vercel.docs.json")).toMatchObject({
      $schema: "https://openapi.vercel.sh/vercel.json",
      framework: null,
      installCommand: "npm ci",
      buildCommand: "npm run build:docs",
      outputDirectory: "out-docs",
      cleanUrls: true,
      trailingSlash: false,
    });
    const pkg = JSON.parse(readFileSync(resolve(process.cwd(), "package.json"), "utf8"));
    expect(pkg.scripts["build:docs"]).toBe("npm run build && node scripts/build-docs-out.mjs");
  });

  it("preserves the same security headers and cache policy on both projects", () => {
    expect(config("vercel.docs.json").headers).toEqual(config().headers);
  });

  it("has explicit permanent redirects for docs-prefixed paths", () => {
    expect(config("vercel.docs.json").redirects).toEqual(expect.arrayContaining([
      { source: "/docs", destination: "/", permanent: true },
      { source: "/docs/:path*", destination: "/:path*", permanent: true },
    ]));
  });

  it.each([
    ["/docs", "/"],
    ["/docs/", "/"],
    ["/docs/mcp", "/mcp"],
    ["/docs/nested/page", "/nested/page"],
  ])("strips the docs prefix from %s", (path, destination) => {
    for (const host of ["docs.atticdb.com", "preview.vercel.app"]) {
      expect(routeFor(config("vercel.docs.json").redirects, host, path)).toEqual({
        destination,
        permanent: true,
      });
    }
  });

  it.each(["/pricing", "/compare", "/benchmarks"])("redirects main page %s from docs and previews", (path) => {
    for (const host of ["docs.atticdb.com", "preview.vercel.app"]) {
      expect(routeFor(config("vercel.docs.json").redirects, host, path)).toEqual({
        destination: `https://atticdb.com${path}`,
        permanent: true,
      });
    }
  });

  it("has the blog redirect in the same shape as the other main pages", () => {
    expect(config("vercel.docs.json").redirects).toContainEqual({
      source: "/blog/:path*",
      destination: "https://atticdb.com/blog/:path*",
      permanent: true,
    });
  });

  it.each(["/blog/some-post", "/blog/rss.xml"])("redirects blog path %s from docs and previews", (path) => {
    for (const host of ["docs.atticdb.com", "preview.vercel.app"]) {
      expect(routeFor(config("vercel.docs.json").redirects, host, path)).toEqual({
        destination: `https://atticdb.com${path}`,
        permanent: true,
      });
    }
  });

  it("catches bare /blog with the same rule", () => {
    const docs = config("vercel.docs.json");
    expect(routeFor(docs.redirects, "docs.atticdb.com", "/blog")).toBeDefined();
  });

  it.each(["/", "/mcp", "/sitemap.xml", "/robots.txt"])("serves docs file %s without routing rules", (path) => {
    const docs = config("vercel.docs.json");
    expect(routeFor(docs.redirects, "docs.atticdb.com", path)).toBeUndefined();
    expect(docs.rewrites ?? []).toEqual([]);
  });
});

describe("build-time host bases", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
    vi.resetModules();
  });

  it.each([
    [undefined, "/docs", ""],
    ["1", "", "https://atticdb.com"],
  ])("sets link bases with VERCEL=%j", async (vercel, docsBase, siteBase) => {
    vi.stubEnv("VERCEL", vercel);
    vi.resetModules();
    const { default: nextConfig } = await import("../../next.config");
    expect(nextConfig.env).toMatchObject({ DOCS_BASE: docsBase, SITE_BASE: siteBase });
  });
});
