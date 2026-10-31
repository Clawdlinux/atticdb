import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

type Route = {
  source: string;
  destination: string;
  has: { type: string; value: string }[];
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
  rewrites: Route[];
  headers: { source: string; headers: { key: string; value: string }[] }[];
};

function config(): VercelConfig {
  return JSON.parse(readFileSync(resolve(process.cwd(), "vercel.json"), "utf8"));
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
    if (!route.has.every((condition) => condition.type === "host" && condition.value === host)) continue;
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
    ]);
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

  it.each(["/pricing", "/compare", "/benchmarks"])("redirects main page %s from the docs host", (path) => {
    expect(routeFor(config().redirects, "docs.atticdb.com", path)).toEqual({
      destination: `https://atticdb.com${path}`,
      permanent: true,
    });
  });

  it.each(["/", "/mcp", "/docs", "/docs/mcp", "/pricing"])("leaves preview-host path %s alone", (path) => {
    expect(routeFor(config().redirects, "preview.vercel.app", path)).toBeUndefined();
  });

  it.each(["/", "/mcp", "/sitemap.xml", "/robots.txt"])("does not redirect docs page %s", (path) => {
    expect(routeFor(config().redirects, "docs.atticdb.com", path)).toBeUndefined();
  });
});

describe("docs host rewrites", () => {
  it.each([
    ["/", "/docs"],
    ["/mcp", "/docs/mcp"],
    ["/install", "/docs/install"],
    ["/nested/page", "/docs/nested/page"],
    ["/sitemap.xml", "/docs/sitemap.xml"],
    ["/robots.txt", "/docs/robots.txt"],
  ])("serves %s from %s", (path, destination) => {
    expect(routeFor(config().rewrites, "docs.atticdb.com", path)?.destination).toBe(destination);
  });

  it.each([
    "/_next/static/x.js", "/_next/static/media/font.woff2", "/images/a.jpg",
    "/fonts/text.woff2", "/icon.svg", "/og.png", "/docs/", "/docs/mcp",
    "/docs/sitemap.xml", "/docs/robots.txt",
  ])("does not rewrite asset or exported docs path %s", (path) => {
    expect(routeFor(config().rewrites, "docs.atticdb.com", path)).toBeUndefined();
  });

  it("keeps discovery rewrites before the catch-all and excludes them from it", () => {
    const rewrites = config().rewrites;
    const catchAll = rewrites.findIndex((route) => route.source.startsWith("/:path("));
    expect(catchAll).toBeGreaterThan(-1);
    for (const path of ["/sitemap.xml", "/robots.txt"]) {
      const index = rewrites.findIndex((route) => route.source === path);
      expect(index).toBeGreaterThan(-1);
      expect(index).toBeLessThan(catchAll);
      expect(sourceRegex(rewrites[catchAll].source).test(path)).toBe(false);
    }
  });

  it.each(["atticdb.com", "www.atticdb.com", "preview.vercel.app"])("leaves %s routes and discovery files alone", (host) => {
    for (const path of ["/", "/mcp", "/docs", "/docs/mcp", "/sitemap.xml", "/robots.txt"]) {
      expect(routeFor(config().rewrites, host, path)).toBeUndefined();
    }
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
