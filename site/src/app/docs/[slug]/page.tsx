import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { DOC_PAGES, findDoc } from "@/content/docs";
import { docMetadata } from "@/lib/seo";
import DocView from "../DocView";

type Props = { params: Promise<{ slug: string }> };

export const dynamicParams = false;

// /docs/install has its own route, so its installer stays off the other pages.
export function generateStaticParams() {
  return DOC_PAGES.filter(({ slug }) => slug && slug !== "install").map(({ slug }) => ({ slug }));
}

export async function generateMetadata({ params }: Props): Promise<Metadata> {
  const { slug } = await params;
  return docMetadata(slug);
}

export default async function DocsPage({ params }: Props) {
  const { slug } = await params;
  if (!slug || !findDoc(slug)) notFound();
  return <DocView slug={slug} />;
}
