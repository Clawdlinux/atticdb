import Link from "next/link";
import Reveal from "@/components/Reveal";
import { BENCH_ROWS, type BenchRow } from "@/content/bench";
import styles from "./Numbers.module.css";

function row(match: Partial<BenchRow>): BenchRow {
  const found = BENCH_ROWS.find((r) =>
    Object.entries(match).every(([k, v]) => r[k as keyof BenchRow] === v),
  );
  if (!found) throw new Error(`No bench row for ${JSON.stringify(match)}`);
  return found;
}

// The home figures, worked out from the H2H medians rather than typed in.
export function homeFigures() {
  const closed = BENCH_ROWS.filter((r) => r.disk === "premium-v2" && r.load === "closed");
  const ratios = closed
    .filter((r) => ["redis", "valkey", "dicedb"].includes(r.system))
    .map((r) => {
      const ours = row({ disk: "premium-v2", load: "closed", writers: r.writers, system: "atticdb" });
      return ours.acksPerSec / r.acksPerSec;
    });
  const agent = BENCH_ROWS.filter(
    (r) => r.disk === "premium-v2" && r.load === "agent" && r.system === "atticdb",
  );
  const agentP50 = Math.max(...agent.map((r) => r.p50us)) / 1000;
  return {
    ratio: Math.max(...ratios).toFixed(1),
    agentP50ms: agentP50.toFixed(1),
    nvmeP50us: String(row({ disk: "nvme", load: "closed", writers: 1, system: "atticdb" }).p50us),
  };
}

function split(figure: string) {
  const [whole, decimals] = figure.split(".");
  return { whole, decimals: decimals ? `.${decimals}` : "" };
}

export default function Numbers() {
  const f = homeFigures();
  const rows = [
    {
      figure: f.ratio,
      caption: "times the durable acks of Redis, Valkey and DiceDB, at most. Premium SSD v2",
    },
    { figure: f.agentP50ms, caption: "ms p50 under agent load, Premium SSD v2" },
    { figure: f.nvmeP50us, caption: "µs p50, 1 writer, local NVMe" },
    { figure: "0", caption: "model calls per write" },
    { figure: "1", caption: "binary, no other services" },
  ];

  return (
    <section className={`bleed ${styles.numbers}`} aria-labelledby="numbers-title">
      <div className={styles.side}>
        <Reveal>
          <h2 id="numbers-title" className={styles.title}>
            <span className={styles.soft}>Measured,</span> <span>not promised</span>
          </h2>
        </Reveal>
        <Reveal delay={0.075}>
          <p className={styles.more}>
            Medians of 7 runs on Azure, 24 October 2026. SQLite wins closed-loop p50, and we say
            so.{" "}
            <Link href="/benchmarks" className={styles.link}>
              Read the benchmarks
            </Link>
          </p>
        </Reveal>
      </div>

      <Reveal delay={0.15} className={styles.ledger}>
        <dl className={styles.list}>
          {rows.map(({ figure, caption }) => {
            const { whole, decimals } = split(figure);
            return (
              <div key={caption} className={styles.row}>
                <dt className={styles.figure}>
                  <span className={styles.whole}>{whole}</span>
                  <span className={styles.decimals}>{decimals}</span>
                </dt>
                <dd className={styles.caption}>{caption}</dd>
              </div>
            );
          })}
        </dl>
      </Reveal>
    </section>
  );
}
