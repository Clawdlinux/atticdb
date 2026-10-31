import Link from "next/link";
import { DOCS_URL, siteHref } from "@/lib/site";
import styles from "./SiteNav.module.css";

export default function SiteNav() {
  return (
    <header className={`container ${styles.header}`}>
      <Link className={styles.wordmark} href={siteHref("/")} aria-label="AtticDB home">
        AtticDB
      </Link>
      <nav className={styles.links} aria-label="Main navigation">
        <Link href={siteHref("/pricing")}>Pricing</Link>
        <Link href={siteHref("/compare")}>Compare</Link>
        <Link href={siteHref("/benchmarks")}>Benchmarks</Link>
        <a href={DOCS_URL}>Docs</a>
      </nav>
      <Link className="button button--ink" href={siteHref("/#install")}>
        Install
      </Link>
    </header>
  );
}
