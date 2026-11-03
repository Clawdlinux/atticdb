import styles from "./Hero.module.css";

export function formatAck(us: number): string {
  if (us < 1000) return `${Math.round(us)} µs`;
  return `${(Math.round(us / 100) / 10).toFixed(1)} ms`;
}

type AckChipProps = {
  // Changes once per simulated write so the light pulses.
  pulse: number;
  powered: boolean;
};

// Plain words, no unit: the light pulses as each write is saved. The status line and the
// time caption under the tape carry the numbers.
export default function AckChip({ pulse, powered }: AckChipProps) {
  return (
    <span className={styles.ack} data-ui="ack" aria-hidden="true">
      <span className={styles.ackInner}>
        <span key={pulse} className={styles.light} data-on={powered} data-pulse={pulse > 0} />
        <span className={styles.ackText}>saved to disk</span>
      </span>
    </span>
  );
}
