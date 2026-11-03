import Link from "next/link";
import { formatDate, listPosts } from "@/content/blog";
import { pageMetadata } from "@/lib/seo";
import { siteHref } from "@/lib/site";
import styles from "./blog.module.css";

export const metadata = pageMetadata("/blog");

export default async function BlogPage() {
  const posts = await listPosts();

  return (
    <section className={styles.blog} aria-labelledby="blog-title">
      <h1 id="blog-title" className={styles.title}>
        Engineering blog
      </h1>
      <p className={styles.lead}>How we build and measure AtticDB, including where it falls short.</p>

      {posts.length ? (
        <ol className={styles.posts}>
          {posts.map((post) => (
            <li key={post.slug} className={styles.entry}>
              <h2 className={styles.entryTitle}>
                <Link href={siteHref(`/blog/${post.slug}`)}>{post.title}</Link>
              </h2>
              <p className={styles.meta}>
                <time dateTime={post.date}>{formatDate(post.date)}</time> · {post.minutes} min read
              </p>
              <p className={styles.description}>{post.description}</p>
            </li>
          ))}
        </ol>
      ) : (
        <p className={styles.empty}>No posts yet.</p>
      )}
    </section>
  );
}
