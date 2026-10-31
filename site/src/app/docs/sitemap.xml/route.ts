import { DOC_PAGES, docCanonical } from "@/content/docs";

export const dynamic = "force-static";

// Served at docs.atticdb.com/sitemap.xml. The main host's sitemap leaves the docs out.
export function GET(): Response {
  const urls = DOC_PAGES.map(({ slug }) => `  <url>\n    <loc>${docCanonical(slug)}</loc>\n  </url>`).join("\n");
  const xml = `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${urls}\n</urlset>\n`;
  return new Response(xml, { headers: { "Content-Type": "application/xml; charset=utf-8" } });
}
