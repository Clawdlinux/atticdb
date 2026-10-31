import styles from "./Hero.module.css";

export function formatAck(us: number): string {
  if (us < 1000) return `${Math.round(us)} µs`;
  return `${(Math.round(us / 100) / 10).toFixed(1)} ms`;
}

type AckChipProps = {
  p50us: number;
  // Changes once per simulated write so the light pulses.
  pulse: number;
  powered: boolean;
};

// Visual twin of the hero's status line, so it stays out of the h1's accessible name.
export default function AckChip({ p50us, pulse, powered }: AckChipProps) {
  return (
    <span className={styles.ack} data-ui="ack" aria-hidden="true">
      <span className={styles.ackInner}>
        <span key={pulse} className={styles.light} data-on={powered} data-pulse={pulse > 0} />
        <span className={styles.ackText}>
          acked <span className={styles.ackValue}>{formatAck(p50us)}</span>
        </span>
      </span>
    </span>
  );
}
