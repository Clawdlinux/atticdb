import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  output: "export",
  images: { unoptimized: true },
  trailingSlash: false,
  // Docs links: Vercel serves /docs/* at the docs host root, so the base is empty there.
  env: {
    DOCS_BASE: process.env.VERCEL ? "" : "/docs",
    SITE_BASE: process.env.VERCEL ? "https://atticdb.com" : "",
  },
};

export default nextConfig;
