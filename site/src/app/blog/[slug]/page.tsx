import type { Metadata } from "next";
import { notFound } from "next/navigation";
import docStyles from "@/app/docs/docs.module.css";
import JsonLd from "@/components/JsonLd";
import { findPost, formatDate, listPosts, renderPost } from "@/content/blog";
import { blogPostingJsonLd, blogPostMetadata } from "@/lib/seo";
import { GITHUB_URL, siteHref } from "@/lib/site";
import styles from "../blog.module.css";

type Props = { params: Promise<{ slug: string }> };

export const dynamicParams = false;

// With output: export, Next fails the build on an empty list. While there are no posts, it gets
// 1 placeholder that 404s. Post slugs are lowercase-hyphenated, so it can't clash with one.
const NO_POSTS_SLUG = "_none";

export async function generateStaticParams() {
  const posts = await listPosts();
  return posts.length ? posts.map(({ slug }) => ({ slug })) : [{ slug: NO_POSTS_SLUG }];
}

export async function generateMetadata({ params }: Props): Promise<Metadata> {
  const { slug } = await params;
  const post = await findPost(slug);
  if (!post) notFound();
  return blogPostMetadata(post);
}

export default async function BlogPostPage({ params }: Props) {
  const { slug } = await params;
  if (!(await findPost(slug))) notFound();
  const { post, html } = await renderPost(slug);

  return (
    <article className={styles.blog}>
      <JsonLd data={blogPostingJsonLd(post)} />
      <h1 className={styles.title}>{post.title}</h1>
      <p className={styles.meta}>
        <time dateTime={post.date}>{formatDate(post.date)}</time> · {post.author} · {post.minutes} min read
      </p>

      {/* The docs prose look, plus the blog's image rules. */}
      <div className={`${docStyles.prose} ${styles.prose}`} dangerouslySetInnerHTML={{ __html: html }} />

      <aside className={styles.cta} aria-label="Try it and tell us">
        <div>
          <h2 className={styles.ctaTitle}>Try it</h2>
          <p>
            AtticDB is one binary, free on your own disk.{" "}
            <a href={siteHref("/#install")}>Install AtticDB</a>, then run <code>atticdb setup</code> to connect your apps.
          </p>
        </div>
        <div>
          <h2 className={styles.ctaTitle}>Tell us</h2>
          <p>
            Found a mistake, or want something measured?{" "}
            <a href={`${GITHUB_URL}/issues/new/choose`}>Open an issue</a>. The code is in the{" "}
            <a href={GITHUB_URL}>AtticDB repo on GitHub</a>.
          </p>
        </div>
      </aside>
    </article>
  );
}
