import { DOCS_ORIGIN } from "@/lib/site";

export const dynamic = "force-static";

// Served at docs.atticdb.com/robots.txt.
export function GET(): Response {
  const body = `User-Agent: *\nAllow: /\n\nSitemap: ${DOCS_ORIGIN}/sitemap.xml\n`;
  return new Response(body, { headers: { "Content-Type": "text/plain; charset=utf-8" } });
}
