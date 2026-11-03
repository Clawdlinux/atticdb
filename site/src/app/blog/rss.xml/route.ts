import { BLOG_DESCRIPTION, BLOG_TITLE, listPosts } from "@/content/blog";
import { escapeHtml } from "@/content/docs";
import { blogPostUrl } from "@/lib/seo";
import { absoluteUrl } from "@/lib/site";

export const dynamic = "force-static";

const FEED_SIZE = 20;

function rfc822(date: string): string {
  return new Date(`${date}T00:00:00Z`).toUTCString();
}

// RSS 2.0 for the newest posts. Every text node is escaped and every link is absolute.
export async function GET(): Promise<Response> {
  const posts = (await listPosts()).slice(0, FEED_SIZE);
  const items = posts.map((post) => {
    const url = blogPostUrl(post.slug);
    return [
      "    <item>",
      `      <title>${escapeHtml(post.title)}</title>`,
      `      <link>${url}</link>`,
      `      <guid isPermaLink="true">${url}</guid>`,
      `      <description>${escapeHtml(post.description)}</description>`,
      `      <pubDate>${rfc822(post.date)}</pubDate>`,
      ...post.tags.map((tag) => `      <category>${escapeHtml(tag)}</category>`),
      "    </item>",
    ].join("\n");
  });
  const channel = [
    `    <title>${escapeHtml(BLOG_TITLE)}</title>`,
    `    <link>${absoluteUrl("/blog")}</link>`,
    `    <description>${escapeHtml(BLOG_DESCRIPTION)}</description>`,
    "    <language>en</language>",
    `    <atom:link href="${absoluteUrl("/blog/rss.xml")}" rel="self" type="application/rss+xml"/>`,
    ...(posts.length ? [`    <lastBuildDate>${rfc822(posts[0].date)}</lastBuildDate>`] : []),
    ...items,
  ].join("\n");
  const xml = `<?xml version="1.0" encoding="UTF-8"?>\n<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">\n  <channel>\n${channel}\n  </channel>\n</rss>\n`;
  return new Response(xml, { headers: { "Content-Type": "application/rss+xml; charset=utf-8" } });
}
