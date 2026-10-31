import Link from "next/link";
import { CREDITS } from "@/content/credits";
import { DOCS_URL, GITHUB_URL } from "@/lib/site";
import styles from "./SiteFooter.module.css";

const LINKS = [
  { label: "GitHub", href: GITHUB_URL },
  { label: "Docs", href: DOCS_URL },
  { label: "Pricing", href: "/pricing" },
  { label: "Compare", href: "/compare" },
  { label: "Benchmarks", href: "/benchmarks" },
  { label: "Issues", href: `${GITHUB_URL}/issues` },
];

export default function SiteFooter() {
  return (
    <footer className={styles.footer}>
      <div className={`container ${styles.inner}`}>
        <div className={styles.top}>
          <p className={styles.notes}>
            <span>AtticDB is an open-source memory database for coding agents.</span>{" "}
            <span>It&apos;s early. Don&apos;t run it in production.</span>
          </p>
          <a className={styles.back} href="#main">
            Back to top
          </a>
        </div>

        <p className={styles.display}>
          <a href={GITHUB_URL} aria-label="AtticDB on GitHub">
            AtticDB
          </a>
        </p>

        <div className={styles.meta}>
          <nav aria-label="Footer">
            <ul className={styles.links}>
              {LINKS.map(({ label, href }) => (
                <li key={label}>
                  {href.startsWith("/") ? (
                    <Link href={href}>{label}</Link>
                  ) : (
                    <a href={href}>{label}</a>
                  )}
                </li>
              ))}
            </ul>
          </nav>

          <div className={styles.credits}>
            <p>Pictures, via Wikimedia Commons:</p>
            <ul>
              {CREDITS.map((c) => (
                <li key={c.file}>
                  <a href={c.source}>{c.title}</a>, {c.creator},{" "}
                  <a href={c.licenceUrl}>{c.licence}</a>
                </li>
              ))}
            </ul>
          </div>

          <p className={styles.licence}>AtticDB is Apache-2.0 or MIT.</p>
        </div>
      </div>
    </footer>
  );
}
