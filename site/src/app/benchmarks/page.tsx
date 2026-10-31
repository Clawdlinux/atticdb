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
  "Every client keeps 1 request in flight. Every system acks only after fsync.",
  "Closed loop: each writer sends its next append as soon as the last one is acked.",
  "Agent-shaped: each writer sleeps 0 to 2x the think time before every append, seeded by writer index.",
  `Run on ${BENCH_META.date} on an ${BENCH_META.machine} (8 vCPU) with Linux 7.0 and ext4.`,
  "Two disks: the VM's local NVMe, and a 128 GiB Premium SSD v2 at 3,000 IOPS and 125 MB/s.",
  "Closed loop runs 5,000 appends of 256 bytes per writer at 1, 4 and 16 writers.",
  "Agent-shaped load runs 64 writers with 5 ms think, and 256 writers with 50 ms think.",
  `Each cell is the median of ${BENCH_META.reps} interleaved reps. Every run starts on a fresh data directory.`,
  "AtticDB commits groups and acks after fdatasync. Redis 8.10.2, Valkey 9.1.2 and DiceDB 1.0.0 use appendfsync always.",
  "SQLite 3.45.1 runs in process with WAL and synchronous=FULL, 1 row per transaction.",
  "Dragonfly has no append-only file, so it cannot ack a write after fsync.",
];

const PREMIUM_SUMMARY = [
  "AtticDB beats Redis, Valkey and DiceDB in every cell on this disk. It acks 1.8x to 2.8x as many writes in closed loop.",
  "In every run its worst p99 rep beats their best rep. Under agent load its p50 is 1.3 ms against 2.8 to 3.1 ms.",
  "In closed loop SQLite has a lower p50 at every writer count. Its p99 and p99.9 are lower at 4 and 16 writers.",
];

const NVME_SUMMARY = [
  "In closed loop AtticDB beats Redis, Valkey and DiceDB on every median at 1 and 4 writers.",
  "At 16 writers it beats them on acks/s and p99 only: 48,611/s and 502 µs. Redis and DiceDB have a lower p50: 299 and 300 against 305 µs.",
  "AtticDB's p99.9 and max at 16 writers are 674 and 1,210 µs against 604 to 947 µs.",
  "Under agent load AtticDB has the best p50 and p99.9 of the 4 servers. Its p99 median trails Redis at 64 writers, and trails DiceDB and Valkey at 256 writers.",
];

const WHERE_WE_LOSE = [
  "SQLite has a lower p50 in closed loop at every writer count on both disks.",
  "SQLite runs in the writer's process with no socket. On NVMe with 1 writer SQLite gets 19,410 acks/s and a 32 µs p50 against AtticDB's 9,799 and 94 µs.",
  "On Premium SSD v2 in closed loop, SQLite has a lower p99 and p99.9 at 4 and 16 writers.",
  "On NVMe in closed loop, SQLite keeps a lower p50 and p99 at 4 and 16 writers.",
  "On NVMe its p99.9 is lower at 16 writers, and its agent-load p50 is 57 and 68 µs.",
  "On NVMe at 16 closed-loop writers, Redis and DiceDB beat AtticDB on p50, p99.9 and max latency.",
  "On NVMe under agent load, Redis has a lower p99 at 64 writers, and DiceDB and Valkey at 256.",
];

const ARTIFACT_LINKS = [
  {
    label: "Raw NVMe TSV (bench/h2h-nvme.tsv)",
    href: `${GITHUB_URL}/blob/main/bench/h2h-nvme.tsv`,
  },
  {
    label: "Raw Premium SSD v2 TSV (bench/h2h-premium-v2.tsv)",
    href: `${GITHUB_URL}/blob/main/bench/h2h-premium-v2.tsv`,
  },
  {
    label: "Benchmark runbook (bench/h2h.sh)",
    href: `${GITHUB_URL}/blob/main/bench/h2h.sh`,
  },
  {
    label: "Full benchmark write-up (bench/H2H.md)",
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

export default function BenchmarksPage() {
  const premiumClosed = getRows("premium-v2", "closed");
  const premiumAgent = getRows("premium-v2", "agent");
  const nvmeClosed = getRows("nvme", "closed");
  const nvmeAgent = getRows("nvme", "agent");

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

      <section className={styles.diskSection} aria-labelledby="premium-title">
        <Reveal>
          <h2 id="premium-title" className={styles.sectionTitle}>
            Premium SSD v2
          </h2>
        </Reveal>
        <Reveal delay={0.075} className={styles.chartBlock}>
          <BenchChart disk="premium-v2" />
        </Reveal>
        <Reveal delay={0.14} className={styles.tableBlock}>
          <BenchTable
            rows={premiumClosed}
            load="closed"
            caption="Premium SSD v2: closed loop (medians of 7 runs)"
            regionLabel="Premium SSD v2 closed-loop results"
            uiId="bench-premium-closed"
          />
        </Reveal>
        <Reveal delay={0.2} className={styles.tableBlock}>
          <BenchTable
            rows={premiumAgent}
            load="agent"
            caption="Premium SSD v2: agent-shaped load (medians of 7 runs)"
            regionLabel="Premium SSD v2 agent-shaped results"
            uiId="bench-premium-agent"
          />
        </Reveal>
        <div className={styles.summaryGrid}>
          {PREMIUM_SUMMARY.map((sentence, i) => (
            <Reveal key={sentence} delay={0.07 * (i + 1)}>
              <p className={styles.summaryText}>{sentence}</p>
            </Reveal>
          ))}
        </div>
      </section>

      <section className={styles.diskSection} aria-labelledby="nvme-title">
        <Reveal>
          <h2 id="nvme-title" className={styles.sectionTitle}>
            Local NVMe
          </h2>
        </Reveal>
        <Reveal delay={0.075} className={styles.chartBlock}>
          <BenchChart disk="nvme" />
        </Reveal>
        <Reveal delay={0.14} className={styles.tableBlock}>
          <BenchTable
            rows={nvmeClosed}
            load="closed"
            caption="Local NVMe: closed loop (medians of 7 runs)"
            regionLabel="Local NVMe closed-loop results"
            uiId="bench-nvme-closed"
          />
        </Reveal>
        <Reveal delay={0.2} className={styles.tableBlock}>
          <BenchTable
            rows={nvmeAgent}
            load="agent"
            caption="Local NVMe: agent-shaped load (medians of 7 runs)"
            regionLabel="Local NVMe agent-shaped results"
            uiId="bench-nvme-agent"
          />
        </Reveal>
        <div className={styles.summaryGrid}>
          {NVME_SUMMARY.map((sentence, i) => (
            <Reveal key={sentence} delay={0.07 * (i + 1)}>
              <p className={styles.summaryText}>{sentence}</p>
            </Reveal>
          ))}
        </div>
      </section>

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
            Raw data and runbook
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
