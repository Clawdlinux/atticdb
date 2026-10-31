import Link from "next/link";
import { DOCS_URL } from "@/lib/site";
import styles from "./SiteNav.module.css";

export default function SiteNav() {
  return (
    <header className={`container ${styles.header}`}>
      <Link className={styles.wordmark} href="/" aria-label="AtticDB home">
        AtticDB
      </Link>
      <nav className={styles.links} aria-label="Main navigation">
        <Link href="/pricing">Pricing</Link>
        <Link href="/compare">Compare</Link>
        <Link href="/benchmarks">Benchmarks</Link>
        <a href={DOCS_URL}>Docs</a>
      </nav>
      <Link className={styles.install} href="/#install">
        Install
      </Link>
    </header>
  );
}
