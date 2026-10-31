import type { Metadata } from "next";
import type { Qa } from "@/content/faq";
import { absoluteUrl, GITHUB_URL, SITE_URL } from "@/lib/site";

type PagePath = "/" | "/pricing" | "/compare" | "/benchmarks";

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
];

export function pageMetadata(path: PagePath): Metadata {
  const page = PAGES.find((page) => page.path === path);
  if (!page) throw new Error(`No metadata for ${path}`);
  const { title, description } = page;

  return {
    title: path === "/" ? { absolute: title } : title,
    description,
    alternates: { canonical: absoluteUrl(path) },
    openGraph: {
      title,
      description,
      url: absoluteUrl(path),
      siteName: "AtticDB",
      images: [{
        url: "/og.png",
        width: 1200,
        height: 630,
        alt: "AtticDB: memory that hits the disk. Acked 94 µs.",
      }],
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
    codeRepository: GITHUB_URL,
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
