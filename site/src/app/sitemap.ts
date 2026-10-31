import type { MetadataRoute } from "next";
import { PAGES } from "@/lib/seo";
import { absoluteUrl } from "@/lib/site";

export const dynamic = "force-static";

export default function sitemap(): MetadataRoute.Sitemap {
  return PAGES.map(({ path }) => ({ url: absoluteUrl(path) }));
}
