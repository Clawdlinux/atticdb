import Installer from "@/components/Installer";
import { DOC_PAGES, docsHref, findDoc, renderDoc } from "@/content/docs";
import styles from "./docs.module.css";

// The page list, the page and its prev and next links. Each page passes its slug,
// so the current page is marked in static HTML with no client code.
export default async function DocView({ slug }: { slug: string }) {
  const page = findDoc(slug);
  if (!page) throw new Error(`No docs page for ${JSON.stringify(slug)}`);
  const { lead, body } = await renderDoc(slug);
  const index = DOC_PAGES.indexOf(page);
  const prev = DOC_PAGES[index - 1];
  const next = DOC_PAGES[index + 1];

  return (
    <div className={styles.grid}>
      <nav className={styles.sidebar} aria-label="Docs pages">
        <ul className={styles.pages}>
          {DOC_PAGES.map((p) => (
            <li key={p.slug}>
              <a
                className={styles.pageLink}
                href={docsHref(p.slug)}
                aria-current={p.slug === slug ? "page" : undefined}
              >
                {p.title}
              </a>
            </li>
          ))}
        </ul>
      </nav>

      <article className={styles.article}>
        <h1 className={styles.title}>{page.title}</h1>
        <div className={styles.prose} dangerouslySetInnerHTML={{ __html: lead }} />
        {slug === "install" ? (
          <div className={styles.installer}>
            <Installer />
          </div>
        ) : null}
        <div className={styles.prose} dangerouslySetInnerHTML={{ __html: body }} />

        <nav className={styles.pager} aria-label="Previous and next page">
          {prev ? (
            <a className={styles.pagerLink} href={docsHref(prev.slug)} rel="prev">
              <span className={styles.pagerLabel}>Previous</span>
              <span className={styles.pagerTitle}>{prev.title}</span>
            </a>
          ) : null}
          {next ? (
            <a className={`${styles.pagerLink} ${styles.pagerNext}`} href={docsHref(next.slug)} rel="next">
              <span className={styles.pagerLabel}>Next</span>
              <span className={styles.pagerTitle}>{next.title}</span>
            </a>
          ) : null}
        </nav>
      </article>
    </div>
  );
}
