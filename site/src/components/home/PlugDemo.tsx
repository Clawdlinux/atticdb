"use client";

import { motion } from "motion/react";
import { useCallback, useEffect, useReducer, useRef, type ReactNode } from "react";
import { HOVER, PRESS, SETTLE, SPRING, SWAP, SWAP_RISE, TAP } from "@/lib/motion";
import { initialPlug, stepPlug, type PlugState } from "./plug";
import styles from "./Hero.module.css";

export const TESTING_URL = "https://github.com/Clawdlinux/atticdb/blob/main/TESTING.md#power-loss";

export const WRITES = 10;
export const TICK_MS = 120;
// Writes stream for about 1.5 s, the power is out for a second, then the store restarts.
export const CUT_AT_MS = 1500;
export const RESTART_AT_MS = 2500;

export function usePlugDemo() {
  const [state, dispatch] = useReducer(stepPlug, undefined, initialPlug);
  const timers = useRef<number[]>([]);

  const clear = useCallback(() => {
    timers.current.forEach((t) => window.clearTimeout(t));
    timers.current = [];
  }, []);

  useEffect(() => clear, [clear]);

  const run = useCallback(() => {
    clear();
    dispatch("reset");
    const at = (ms: number, ev: Parameters<typeof stepPlug>[1]) =>
      timers.current.push(window.setTimeout(() => dispatch(ev), ms));
    for (let i = 0; i < WRITES; i += 1) at(i * TICK_MS, "tick");
    at(CUT_AT_MS, "cut");
    at(RESTART_AT_MS, "restart");
  }, [clear]);

  const running = state.phase === "writing" || state.phase === "cut";
  return { state, run, running };
}

type PowerChipProps = {
  state: PlugState;
  running: boolean;
  onRun: () => void;
  className?: string;
};

export function PowerChip({ state, running, onRun, className }: PowerChipProps) {
  const on = state.phase !== "cut";
  const action = state.phase === "restarted" ? "Pull it again" : "Pull the plug";
  return (
    <div className={`${styles.chip} ${className ?? ""}`} data-ui="power">
      <span className={styles.chipLabel}>Power</span>
      <span className={styles.readout}>{on ? "On" : "Off"}</span>
      <motion.button
        type="button"
        className={styles.switch}
        data-on={on}
        aria-disabled={running || undefined}
        onClick={() => {
          if (!running) onRun();
        }}
        whileHover={HOVER}
        whileTap={TAP}
        transition={PRESS}
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
      </motion.button>
    </div>
  );
}

// 1rem: the track's inner width less the knob.
const KNOB_TRAVEL = 16;
// Lost blocks drop one after another, this many seconds apart.
const LOST_STAGGER = 0.06;

type CellKind = "pending" | "acked" | "lost";

// Each write drops a block into its slot as it is sent. Kept blocks hold; lost ones fade and drop.
function Cells({ kinds, label }: { kinds: CellKind[]; label: string }) {
  const firstLost = kinds.indexOf("lost");
  return (
    <span className={styles.cells} aria-hidden="true" data-label={label}>
      {Array.from({ length: WRITES }, (_, i) => {
        const kind = kinds[i];
        const lost = kind === "lost";
        return (
          <span key={i} className={styles.cell} data-kind={kind ?? "empty"}>
            {kind && (
              <motion.span
                className={styles.block}
                data-kind={kind}
                initial={{ opacity: 0, y: -SWAP_RISE }}
                animate={lost ? { opacity: 0.25, y: SWAP_RISE, scale: 0.6 } : { opacity: 1, y: 0, scale: 1 }}
                transition={lost ? { ...SETTLE, delay: (i - firstLost) * LOST_STAGGER } : SWAP}
              />
            )}
          </span>
        );
      })}
    </span>
  );
}

const resultMotion = {
  idle: { opacity: 0, y: SWAP_RISE },
  shown: { opacity: 1, y: 0, transition: { ...SETTLE, staggerChildren: 0.08 } },
};

const lineMotion = {
  idle: { opacity: 0, y: SWAP_RISE },
  shown: { opacity: 1, y: 0, transition: SETTLE },
};

// A result line that fades up whenever its words change from the running count to the outcome.
function Line({ done, children }: { done: boolean; children: ReactNode }) {
  return (
    <motion.span
      initial={done ? { opacity: 0, y: SWAP_RISE } : false}
      animate={{ opacity: 1, y: 0 }}
      transition={SETTLE}
    >
      {children}
    </motion.span>
  );
}

function atticCells(s: PlugState): CellKind[] {
  return Array.from({ length: s.sent }, (_, i) => (i < s.acked ? "acked" : "pending"));
}

function ackBeforeSyncCells(s: PlugState): CellKind[] {
  return Array.from({ length: s.sent }, (_, i) =>
    s.phase === "restarted" && i >= s.keptAckBeforeSync ? "lost" : "acked",
  );
}

export function PlugResult({ state }: { state: PlugState }) {
  const ref = useRef<HTMLDivElement>(null);
  const started = state.phase === "writing" && state.sent === 1;

  // On a phone the result lands below the fold. Bring it into view once per run.
  useEffect(() => {
    if (!started) return;
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    ref.current?.scrollIntoView?.({ block: "nearest", behavior: reduce ? "auto" : "smooth" });
  }, [started]);

  const done = state.phase === "restarted";
  const attic = done
    ? `AtticDB: ${state.keptAttic} of ${state.acked} acked writes kept.`
    : `AtticDB: ${state.acked} acked.`;
  const other = done
    ? `A store that acks before sync: ${state.keptAckBeforeSync} of ${state.sent} acked writes kept.`
    : `A store that acks before sync: ${state.sent} acked.`;

  const idle = state.phase === "idle";
  return (
    <motion.div
      ref={ref}
      className={styles.result}
      data-phase={state.phase}
      hidden={idle}
      initial={false}
      animate={idle ? "idle" : "shown"}
      variants={resultMotion}
    >
      <div className={styles.rows} aria-live="polite">
        <motion.p className={styles.row} data-store="atticdb" variants={lineMotion}>
          <Cells kinds={atticCells(state)} label="AtticDB" />
          <Line key={done ? "done" : "live"} done={done}>
            {done ? attic : <span aria-hidden="true">{attic}</span>}
          </Line>
        </motion.p>
        <motion.p className={styles.row} data-store="ack-before-sync" variants={lineMotion}>
          <Cells kinds={ackBeforeSyncCells(state)} label="Ack before sync" />
          <Line key={done ? "done" : "live"} done={done}>
            {done ? other : <span aria-hidden="true">{other}</span>}
          </Line>
        </motion.p>
      </div>
      <motion.p className={styles.note} variants={lineMotion}>
        An illustration of ack-after-sync, not a crash test.{" "}
        <a href={TESTING_URL}>
          The real test cuts power on LazyFS. CI runs crash simulations on 2,000 seeds.
        </a>
      </motion.p>
    </motion.div>
  );
}
