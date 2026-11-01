import type { CSSProperties } from "react";
import dataStyles from "@/components/DataTable.module.css";
import Reveal from "@/components/Reveal";
import { COMPARE_ROWS, PICK_ATTIC, PICK_OTHER, PRODUCTS } from "@/content/compare";
import { pageMetadata } from "@/lib/seo";
import styles from "./compare.module.css";

export const metadata = pageMetadata("/compare");

type SourceEntry = {
  index: number;
  url: string;
  productNames: string[];
  labels: string[];
};

function collectSources(): {
  byUrl: Map<string, SourceEntry>;
  entries: SourceEntry[];
} {
  const byUrl = new Map<string, SourceEntry>();
  for (const row of COMPARE_ROWS) {
    for (const product of PRODUCTS) {
      if (product.id === "atticdb") continue;
      const cell = row.cells[product.id];
      if (!cell?.source) continue;
      const existing = byUrl.get(cell.source);
      if (existing) {
        if (!existing.productNames.includes(product.name)) {
          existing.productNames.push(product.name);
        }
        if (!existing.labels.includes(row.label)) {
          existing.labels.push(row.label);
        }
      } else {
        byUrl.set(cell.source, {
          index: byUrl.size + 1,
          url: cell.source,
          productNames: [product.name],
          labels: [row.label],
        });
      }
    }
  }
  return { byUrl, entries: [...byUrl.values()] };
}

function step(i: number): CSSProperties {
  return { "--i": i } as CSSProperties;
}

export default function ComparePage() {
  const { byUrl, entries } = collectSources();

  return (
    <>
      <section className={styles.hero} aria-labelledby="compare-hero-title">
        <h1 id="compare-hero-title" className={styles.title}>
          <span className={styles.line}>
            <span className={styles.rise} style={step(0)}>
              Where we win.
            </span>
          </span>{" "}
          <span className={styles.line}>
            <span className={styles.rise} style={step(1)}>
              Where we don&apos;t.
            </span>
          </span>
        </h1>
        <p className={styles.lead}>
          Facts from each product&apos;s own docs, seen 31 October 2026. Where a vendor publishes
          nothing, the cell says so.
        </p>
      </section>

      <section className={`bleed ${styles.matrixSection}`} aria-labelledby="matrix-title">
        <Reveal>
          <h2 id="matrix-title" className={styles.sectionTitle}>
            Nine memory tools
          </h2>
        </Reveal>
        <Reveal delay={0.075}>
          <div
            className={dataStyles.tableRegion}
            role="region"
            aria-label="Product comparison"
            tabIndex={0}
            data-ui="product-comparison"
          >
            <table className={`${dataStyles.table} ${dataStyles.tableWide} ${dataStyles.onBone}`}>
              <caption className="visually-hidden">
                Comparison across AtticDB, Mem0, Zep and Graphiti, Letta, Supermemory, Redis Agent
                Memory, Cognee, Engram and CLAUDE.md files
              </caption>
              <thead>
                <tr>
                  <td>Criterion</td>
                  {PRODUCTS.map((product) => (
                    <th
                      key={product.id}
                      scope="col"
                      className={product.id === "atticdb" ? dataStyles.atticCol : undefined}
                    >
                      {product.name}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {COMPARE_ROWS.map((row) => (
                  <tr key={row.label}>
                    <th scope="row">{row.label}</th>
                    {PRODUCTS.map((product) => {
                      const cell = row.cells[product.id];
                      const sourceEntry =
                        product.id !== "atticdb" && cell.source
                          ? byUrl.get(cell.source)
                          : undefined;
                      return (
                        <td
                          key={product.id}
                          className={product.id === "atticdb" ? dataStyles.atticCol : undefined}
                        >
                          {cell.value}
                          {sourceEntry ? (
                            <sup className={styles.refSup}>
                              <a
                                href={`#src-${sourceEntry.index}`}
                                className={styles.refLink}
                                aria-label={`Source ${sourceEntry.index}: ${product.name}, ${row.label}`}
                              >
                                [{sourceEntry.index}]
                              </a>
                            </sup>
                          ) : null}
                        </td>
                      );
                    })}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Reveal>
      </section>

      <section className={styles.pickSection} aria-labelledby="pick-title">
        <Reveal>
          <h2 id="pick-title" className={styles.sectionTitle}>Which to pick</h2>
        </Reveal>
        <div className={styles.pickColumns}>
          <div>
            <h3 className={styles.pickHeading}>Pick AtticDB if</h3>
            <ul className={styles.pickList}>
              {PICK_ATTIC.map((item, i) => (
                <Reveal as="li" key={item} delay={0.075 * (i + 1)} className={styles.pickItem}>
                  {item}
                </Reveal>
              ))}
            </ul>
          </div>
          <div>
            <h3 className={styles.pickHeading}>Pick something else if</h3>
            <dl className={styles.otherList}>
              {PICK_OTHER.map(({ when, product }, i) => (
                <Reveal key={when} delay={0.075 * (i + 1)} className={styles.otherRow}>
                  <dt className={styles.otherWhen}>{when}</dt>
                  <dd className={styles.otherProduct}>{product}</dd>
                </Reveal>
              ))}
            </dl>
          </div>
        </div>
      </section>

      <section className={`bleed ${styles.sourcesSection}`} aria-labelledby="sources-title">
        <Reveal>
          <h2 id="sources-title" className={styles.sectionTitle}>
            Sources
          </h2>
        </Reveal>
        <ol className={styles.sourcesList}>
          {entries.map((entry, i) => (
            <Reveal
              as="li"
              key={entry.url}
              id={`src-${entry.index}`}
              delay={0.06 * ((i % 6) + 1)}
              className={styles.sourceItem}
            >
              <span className={styles.sourceMeta}>
                <strong className={styles.sourceProduct}>{entry.productNames.join(", ")}</strong>
                {" ("}
                {entry.labels.join(", ")}
                {"): "}
              </span>
              <a href={entry.url} className={styles.sourceLink}>
                {entry.url}
              </a>
            </Reveal>
          ))}
        </ol>
      </section>
    </>
  );
}
