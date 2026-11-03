import type { Metadata } from "next";
import { BLOG_DESCRIPTION, BLOG_TITLE, type Post } from "@/content/blog";
import { docCanonical, findDoc } from "@/content/docs";
import type { Qa } from "@/content/faq";
import { absoluteUrl, GITHUB_URL, SITE_URL } from "@/lib/site";

type PagePath = "/" | "/pricing" | "/compare" | "/benchmarks" | "/blog";

export const PAGES: { path: PagePath; title: string; description: string }[] = [
  {
    path: "/",
    title: "AtticDB: durable memory for coding agents",
    description:
      "AtticDB is an open-source memory database for coding agents. One Rust binary. Every write is acked after fdatasync. No model calls.",
  },
  {
    path: "/pricing",
    title: "AtticDB pricing: free locally, cloud planned",
    description:
      "AtticDB is free on your own disk, forever. Cloud sync, backups and team memory are planned. No per-memory or per-call fees.",
  },
  {
    path: "/compare",
    title: "AtticDB vs Mem0, Zep, Letta and Supermemory",
    description:
      "How AtticDB compares with Mem0, Zep, Letta, Supermemory, Redis Agent Memory, Cognee and Engram. Sourced facts, including where we lose.",
  },
  {
    path: "/benchmarks",
    title: "AtticDB benchmarks: durable acks vs Redis and SQLite",
    description:
      "Durable acks per second and latency for AtticDB, Redis, Valkey, DiceDB and SQLite on NVMe and Premium SSD v2. 7 runs, raw data linked.",
  },
  {
    path: "/blog",
    title: BLOG_TITLE,
    description: BLOG_DESCRIPTION,
  },
];

const OG_IMAGE = {
  url: "/og.png",
  width: 1200,
  height: 630,
  alt: "AtticDB: memory that hits the disk. Acked 94 µs.",
};

// Next replaces a layout's alternates with a page's, so every page repeats the feed link.
export const RSS_FEED = {
  "application/rss+xml": [{ url: absoluteUrl("/blog/rss.xml"), title: BLOG_TITLE }],
};

export function pageMetadata(path: PagePath): Metadata {
  const page = PAGES.find((page) => page.path === path);
  if (!page) throw new Error(`No metadata for ${path}`);
  const { title, description } = page;

  return {
    title: path === "/" ? { absolute: title } : title,
    description,
    alternates: { canonical: absoluteUrl(path), types: RSS_FEED },
    openGraph: {
      title,
      description,
      url: absoluteUrl(path),
      siteName: "AtticDB",
      images: [OG_IMAGE],
      type: "website",
    },
    twitter: {
      card: "summary_large_image",
      title,
      description,
      images: ["/og.png"],
    },
  };
}

// Docs pages live on their own host, so the canonical and og:url point there.
export function docMetadata(slug: string): Metadata {
  const page = findDoc(slug);
  if (!page) throw new Error(`No docs page for ${JSON.stringify(slug)}`);
  const title = `${page.title} · AtticDB docs`;
  const { description } = page;
  const url = docCanonical(slug);

  return {
    title: { absolute: title },
    description,
    alternates: { canonical: url, types: RSS_FEED },
    openGraph: {
      title,
      description,
      url,
      siteName: "AtticDB",
      images: [OG_IMAGE],
      type: "website",
    },
    twitter: {
      card: "summary_large_image",
      title,
      description,
      images: ["/og.png"],
    },
  };
}

export function blogPostUrl(slug: string): string {
  return absoluteUrl(`/blog/${slug}`);
}

export function blogPostMetadata(post: Post): Metadata {
  const title = `${post.title} · AtticDB blog`;
  const { description } = post;
  const url = blogPostUrl(post.slug);
  const image = post.image ? { url: absoluteUrl(post.image), alt: post.title } : OG_IMAGE;

  return {
    title: { absolute: title },
    description,
    alternates: { canonical: url, types: RSS_FEED },
    openGraph: {
      title,
      description,
      url,
      siteName: "AtticDB",
      images: [image],
      type: "article",
      publishedTime: post.date,
      authors: [post.author],
      tags: post.tags,
    },
    twitter: {
      card: "summary_large_image",
      title,
      description,
      images: [image.url],
    },
  };
}

export function blogPostingJsonLd(post: Post) {
  const url = blogPostUrl(post.slug);
  return {
    "@context": "https://schema.org",
    "@type": "BlogPosting",
    headline: post.title,
    description: post.description,
    datePublished: post.date,
    author: { "@type": "Person", name: post.author },
    publisher: { "@type": "Organization", name: "AtticDB", url: SITE_URL },
    url,
    mainEntityOfPage: url,
    keywords: post.tags.join(", "),
    ...(post.image ? { image: absoluteUrl(post.image) } : {}),
  };
}

export function softwareAppJsonLd() {
  return {
    "@context": "https://schema.org",
    "@type": "SoftwareApplication",
    name: "AtticDB",
    applicationCategory: "DeveloperApplication",
    operatingSystem: "macOS, Linux, Windows",
    url: SITE_URL,
    downloadUrl: `${GITHUB_URL}/releases/latest`,
    license: "Apache-2.0 OR MIT",
    offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
  };
}

export function faqJsonLd(items: Qa[]) {
  return {
    "@context": "https://schema.org",
    "@type": "FAQPage",
    mainEntity: items.map(({ q, a }) => ({
      "@type": "Question",
      name: q,
      acceptedAnswer: { "@type": "Answer", text: a },
    })),
  };
}
