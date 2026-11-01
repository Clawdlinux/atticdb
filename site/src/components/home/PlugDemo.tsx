"use client";

import { motion } from "motion/react";
import { SETTLE, SPRING, SWAP_RISE } from "@/lib/motion";
import type { PlugView } from "./tape-time";
import styles from "./Hero.module.css";

export const TESTING_URL = "https://github.com/Clawdlinux/atticdb/blob/main/TESTING.md#power-loss";

type PowerChipProps = {
  view: PlugView;
  onRun: () => void;
  className?: string;
};

export function PowerChip({ view, onRun, className }: PowerChipProps) {
  const on = view.phase !== "cut";
  const running = view.phase === "armed" || view.phase === "cut";
  const action = view.phase === "restarted" ? "Pull it again" : "Pull the plug";
  return (
    <div className={`${styles.chip} ${className ?? ""}`} data-ui="power">
      <span className={styles.chipLabel}>Power</span>
      <span className={styles.readout}>{on ? "On" : "Off"}</span>
      <button
        type="button"
        className={`button button--field ${styles.switch}`}
        data-on={on}
        aria-disabled={running || undefined}
        onClick={() => {
          if (!running) onRun();
        }}
      >
        <span className={styles.track} aria-hidden="true">
          <motion.span
            className={styles.knob}
            initial={false}
            animate={{ x: on ? KNOB_TRAVEL : 0 }}
            transition={SPRING}
          />
        </span>
        {action}
      </button>
    </div>
  );
}

// 1rem: the track's inner width less the knob.
const KNOB_TRAVEL = 16;

function outcomeLines(view: PlugView): string[] {
  const o = view.outcome;
  if (view.phase === "restarted" && o) {
    return [
      `AtticDB: ${o.attic.kept} of ${o.attic.acked} acked writes kept.`,
      `A store that acks before sync: ${o.ackBeforeSync.kept} of ${o.ackBeforeSync.acked} acked writes kept.`,
    ];
  }
  if (view.phase === "cut") return ["Power off. Frames not yet synced drop away."];
  if (view.phase === "armed") return ["Cutting the power mid-group."];
  return ["Pull the plug. Frames not yet synced drop away. Acked writes stay."];
}

// The plug demo's words. They sit right under the band and never change its height.
export function PlugResult({ view }: { view: PlugView }) {
  const lines = outcomeLines(view);
  return (
    <div className={styles.result} data-phase={view.phase}>
      <p className={styles.outcome} aria-live="polite">
        {lines.map((text) => (
          <motion.span
            key={text}
            className={styles.outcomeLine}
            initial={view.phase === "idle" ? false : { opacity: 0, y: SWAP_RISE }}
            animate={{ opacity: 1, y: 0 }}
            transition={SETTLE}
          >
            {text}
          </motion.span>
        ))}
      </p>
      <p className={styles.note}>
        An illustration of ack-after-sync, not a crash test.{" "}
        <a href={TESTING_URL}>
          The real test cuts power on LazyFS. CI runs crash simulations on 2,000 seeds.
        </a>
      </p>
    </div>
  );
}
