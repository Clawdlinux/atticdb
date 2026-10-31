import type { Metadata } from "next";
import Link from "next/link";
import { absoluteUrl } from "@/lib/site";
import styles from "./not-found.module.css";

export const metadata: Metadata = {
  title: { absolute: "Nothing in the attic. | AtticDB" },
  alternates: { canonical: absoluteUrl("/404") },
  robots: { index: false, follow: true },
};

export default function NotFound() {
  return (
    <section className={styles.page} aria-labelledby="not-found-title">
      <h1 id="not-found-title" className={styles.title} aria-label="Nothing in the attic.">
        <span>Nothing in</span>{" "}
        <span>The attic.</span>
      </h1>
      <p>That page isn&apos;t here. It may have moved, or it never existed.</p>
      <Link className={styles.home} href="/">Back to the home page</Link>
    </section>
  );
}
