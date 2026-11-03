import type { MetadataRoute } from "next";
import { listPosts } from "@/content/blog";
import { blogPostUrl, PAGES } from "@/lib/seo";
import { absoluteUrl } from "@/lib/site";

export const dynamic = "force-static";

export default async function sitemap(): Promise<MetadataRoute.Sitemap> {
  const posts = await listPosts();
  return [
    ...PAGES.map(({ path }) => ({ url: absoluteUrl(path) })),
    ...posts.map(({ slug, date }) => ({ url: blogPostUrl(slug), lastModified: date })),
  ];
}
