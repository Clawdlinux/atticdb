import type { CSSProperties } from "react";
import BenchChart from "@/components/BenchChart";
import dataStyles from "@/components/DataTable.module.css";
import Reveal from "@/components/Reveal";
import { BENCH_META, BENCH_ROWS, type BenchRow, type Disk } from "@/content/bench";
import { GITHUB_URL } from "@/lib/site";
import { pageMetadata } from "@/lib/seo";
import styles from "./benchmarks.module.css";

export const metadata = pageMetadata("/benchmarks");

const fmt = new Intl.NumberFormat("en-US");

const METHOD_ITEMS = [
  "Each client keeps 1 request in flight. Every system acks only after fsync.",
  "Closed loop: 5,000 appends of 256 bytes per writer, at 1, 4 and 16 writers.",
  "Agent-shaped: 64 writers with 5 ms think, or 256 with 50 ms think. Sleeps are seeded and jittered.",
  `${BENCH_META.date}: ${BENCH_META.machine}, 8 vCPU, Linux 7.0, ext4. Medians of ${BENCH_META.reps} interleaved runs, each on fresh data.`,
  "Local NVMe and 128 GiB Premium SSD v2. Premium runs at 3,000 IOPS and 125 MB/s.",
  "AtticDB groups commits with fdatasync. Other servers use appendfsync always; SQLite uses WAL and synchronous=FULL.",
];

const WHERE_WE_LOSE = [
  "SQLite has a lower p50 in closed loop at every writer count on both disks. It runs in-process, without sockets.",
  "On Premium SSD v2, SQLite has lower closed-loop p99 and p99.9 at 4 and 16 writers.",
  "On NVMe, SQLite has lower closed-loop p99 at 4 and 16 writers, and lower p99.9 at 16. Its agent-load p50 is lower too.",
  "On NVMe at 16 closed-loop writers, Redis and DiceDB beat AtticDB on p50, p99.9 and max latency.",
  "On NVMe under agent load, Redis has lower p99 at 64 writers. DiceDB and Valkey win at 256.",
];

const ARTIFACT_LINKS = [
  {
    label: "Raw NVMe TSV",
    href: `${GITHUB_URL}/blob/main/bench/h2h-nvme.tsv`,
  },
  {
    label: "Raw Premium SSD v2 TSV",
    href: `${GITHUB_URL}/blob/main/bench/h2h-premium-v2.tsv`,
  },
  {
    label: "Benchmark runbook",
    href: `${GITHUB_URL}/blob/main/bench/h2h.sh`,
  },
  {
    label: "Full benchmark write-up",
    href: `${GITHUB_URL}/blob/main/bench/H2H.md`,
  },
];

function step(i: number): CSSProperties {
  return { "--i": i } as CSSProperties;
}

function getRows(disk: Disk, load: "closed" | "agent"): BenchRow[] {
  return BENCH_ROWS.filter((r) => r.disk === disk && r.load === load);
}

function BenchTable({
  rows,
  load,
  caption,
  regionLabel,
  uiId,
}: {
  rows: BenchRow[];
  load: "closed" | "agent";
  caption: string;
  regionLabel: string;
  uiId: string;
}) {
  const firstColHeader = load === "closed" ? "Writers" : "Load";
  return (
    <div>
      <p className={styles.tableCaption} aria-hidden="true">
        {caption}
      </p>
      <div
        className={dataStyles.tableRegion}
        role="region"
        aria-label={regionLabel}
        tabIndex={0}
        data-ui={uiId}
      >
        <table className={dataStyles.table}>
          <caption className="visually-hidden">{caption}</caption>
        <thead>
          <tr>
            <th scope="col">{firstColHeader}</th>
            <th scope="col">System</th>
            <th scope="col" className={dataStyles.num}>
              Acks/s
            </th>
            <th scope="col" className={dataStyles.num}>
              p50 µs
            </th>
            <th scope="col" className={dataStyles.num}>
              p99 µs
            </th>
            <th scope="col" className={dataStyles.num}>
              p99 range µs
            </th>
            <th scope="col" className={dataStyles.num}>
              p99.9 µs
            </th>
            <th scope="col" className={dataStyles.num}>
              max µs
            </th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => {
            const firstColValue =
              load === "closed"
                ? String(row.writers)
                : `${row.writers} writers, ${row.thinkMs} ms`;
            const isAttic = row.system === "atticdb";
            return (
              <tr
                key={`${row.disk}-${row.load}-${row.writers}-${row.thinkMs}-${row.system}`}
                className={isAttic ? dataStyles.atticRow : undefined}
              >
                <th scope="row">{firstColValue}</th>
                <td>{row.system}</td>
                <td className={dataStyles.num}>{fmt.format(row.acksPerSec)}</td>
                <td className={dataStyles.num}>{fmt.format(row.p50us)}</td>
                <td className={dataStyles.num}>{fmt.format(row.p99us)}</td>
                <td className={dataStyles.num}>
                  {`${fmt.format(row.p99RangeUs[0])} to ${fmt.format(row.p99RangeUs[1])}`}
                </td>
                <td className={dataStyles.num}>{fmt.format(row.p999us)}</td>
                <td className={dataStyles.num}>{fmt.format(row.maxUs)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      </div>
    </div>
  );
}

function SummaryTable({ disk, label }: { disk: Disk; label: string }) {
  const rows = BENCH_ROWS.filter((row) => row.disk === disk && row.system === "atticdb");
  return (
    <div
      className={dataStyles.tableRegion}
      role="region"
      aria-label={`${label} summary results`}
      tabIndex={0}
      data-ui={`bench-summary-${disk}`}
    >
      <table className={`${dataStyles.table} ${styles.summaryTable}`}>
        <caption className={styles.summaryCaption}>
          {label}: acks/s and p99 µs. Best server is chosen separately for each metric from Redis, Valkey and DiceDB.
        </caption>
        <thead>
          <tr>
            <th scope="col" rowSpan={2}>Load</th>
            <th scope="colgroup" colSpan={2}>AtticDB</th>
            <th scope="colgroup" colSpan={2}>Best server</th>
            <th scope="colgroup" colSpan={2}>SQLite</th>
          </tr>
          <tr>
            {["atticdb", "server", "sqlite"].flatMap((system) => [
              <th key={`${system}-acks`} scope="col" className={dataStyles.num}>Acks/s</th>,
              <th key={`${system}-p99`} scope="col" className={dataStyles.num}>p99 µs</th>,
            ])}
          </tr>
        </thead>
        <tbody>
          {rows.map((ours) => {
            const peers = BENCH_ROWS.filter((row) => row.disk === disk && row.load === ours.load
              && row.writers === ours.writers && row.thinkMs === ours.thinkMs);
            const servers = peers.filter((row) => ["redis", "valkey", "dicedb"].includes(row.system));
            const fastest = servers.reduce((best, row) => row.acksPerSec > best.acksPerSec ? row : best);
            const lowest = servers.reduce((best, row) => row.p99us < best.p99us ? row : best);
            const sqlite = peers.find((row) => row.system === "sqlite")!;
            return (
              <tr key={`${ours.load}-${ours.writers}`}>
                <th scope="row">
                  {ours.writers} {ours.writers === 1 ? "writer" : "writers"}, {ours.load === "closed" ? "closed loop" : `${ours.thinkMs} ms think`}
                </th>
                <td className={dataStyles.num}>{fmt.format(ours.acksPerSec)}</td>
                <td className={dataStyles.num}>{fmt.format(ours.p99us)}</td>
                <td className={dataStyles.num}>{fmt.format(fastest.acksPerSec)} ({fastest.system})</td>
                <td className={dataStyles.num}>{fmt.format(lowest.p99us)} ({lowest.system})</td>
                <td className={dataStyles.num}>{fmt.format(sqlite.acksPerSec)}</td>
                <td className={dataStyles.num}>{fmt.format(sqlite.p99us)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

export default function BenchmarksPage() {
  return (
    <>
      <section className={styles.hero} aria-labelledby="bench-hero-title">
        <h1 id="bench-hero-title" className={styles.title}>
          <span className={styles.line}>
            <span className={styles.rise} style={step(0)}>
              Durable acks,
            </span>
          </span>{" "}
          <span className={styles.line}>
            <span className={styles.rise} style={step(1)}>
              Measured.
            </span>
          </span>
        </h1>
        <p className={styles.lead}>
          How fast can each system ack a write that survives power loss? 5 systems, 2 disks, 7 runs
          each.
        </p>
      </section>

      <section className={styles.splitSection} aria-labelledby="method-title">
        <Reveal className={styles.splitHead}>
          <h2 id="method-title" className={styles.sectionTitle}>
            Method and setup
          </h2>
        </Reveal>
        <ul className={`${styles.splitBody} ${styles.methodGrid}`}>
          {METHOD_ITEMS.map((item, i) => (
            <Reveal
              as="li"
              key={item}
              delay={0.06 * ((i % 6) + 1)}
              className={styles.methodItem}
            >
              {item}
            </Reveal>
          ))}
        </ul>
      </section>

      {([
        { disk: "premium-v2", label: "Premium SSD v2", id: "premium" },
        { disk: "nvme", label: "Local NVMe", id: "nvme" },
      ] as const).map(({ disk, label, id }) => (
        <section key={disk} className={`bleed ${styles.diskSection} ${disk === "nvme" ? styles.alternate : ""}`} aria-labelledby={`${id}-title`}>
          <Reveal>
            <h2 id={`${id}-title`} className={styles.sectionTitle}>{label}</h2>
          </Reveal>
          <Reveal delay={0.075} className={styles.chartBlock}>
            <BenchChart disk={disk} />
          </Reveal>
          <Reveal delay={0.14} className={styles.tableBlock}>
            <SummaryTable disk={disk} label={label} />
          </Reveal>
          <details className={styles.rawRows}>
            <summary>All rows, {label}</summary>
            <div className={styles.rawTables}>
              <BenchTable
                rows={getRows(disk, "closed")}
                load="closed"
                caption={`${label}: closed loop (medians of ${BENCH_META.reps} runs)`}
                regionLabel={`${label} closed-loop results`}
                uiId={`bench-${id}-closed`}
              />
              <BenchTable
                rows={getRows(disk, "agent")}
                load="agent"
                caption={`${label}: agent-shaped load (medians of ${BENCH_META.reps} runs)`}
                regionLabel={`${label} agent-shaped results`}
                uiId={`bench-${id}-agent`}
              />
            </div>
          </details>
        </section>
      ))}

      <section className={`bleed ${styles.loseSection}`} aria-labelledby="lose-title">
        <div className={styles.splitInner}>
          <Reveal className={styles.splitHead}>
            <h2 id="lose-title" className={styles.sectionTitle}>
              Where we lose
            </h2>
          </Reveal>
          <ul className={`${styles.splitBody} ${styles.loseList}`}>
            {WHERE_WE_LOSE.map((item, i) => (
              <Reveal as="li" key={item} delay={0.07 * (i + 1)} className={styles.loseItem}>
                {item}
              </Reveal>
            ))}
          </ul>
        </div>
      </section>

      <section className={styles.splitSection} aria-labelledby="raw-title">
        <Reveal className={styles.splitHead}>
          <h2 id="raw-title" className={styles.sectionTitle}>
            Raw data
          </h2>
        </Reveal>
        <ul className={`${styles.splitBody} ${styles.linksList}`}>
          {ARTIFACT_LINKS.map(({ label, href }, i) => (
            <Reveal as="li" key={href} delay={0.075 * (i + 1)} className={styles.linkItem}>
              <a href={href} className={styles.artifactLink}>
                {label}
              </a>
            </Reveal>
          ))}
        </ul>
      </section>
    </>
  );
}
