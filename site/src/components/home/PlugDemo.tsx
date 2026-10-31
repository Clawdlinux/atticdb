"use client";

import { useCallback, useEffect, useReducer, useRef } from "react";
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
      <button
        type="button"
        className={styles.switch}
        data-on={on}
        aria-disabled={running || undefined}
        onClick={() => {
          if (!running) onRun();
        }}
      >
        <span className={styles.track} aria-hidden="true">
          <span className={styles.knob} />
        </span>
        {action}
      </button>
    </div>
  );
}

type CellKind = "pending" | "acked" | "lost";

function Cells({ kinds, label }: { kinds: CellKind[]; label: string }) {
  return (
    <span className={styles.cells} aria-hidden="true" data-label={label}>
      {Array.from({ length: WRITES }, (_, i) => (
        <span key={i} className={styles.cell} data-kind={kinds[i] ?? "empty"} />
      ))}
    </span>
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

  return (
    <div
      ref={ref}
      className={styles.result}
      data-phase={state.phase}
      hidden={state.phase === "idle"}
    >
      <div className={styles.rows} aria-live="polite">
        <p className={styles.row} data-store="atticdb">
          <Cells kinds={atticCells(state)} label="AtticDB" />
          <span>{done ? attic : <span aria-hidden="true">{attic}</span>}</span>
        </p>
        <p className={styles.row} data-store="ack-before-sync">
          <Cells kinds={ackBeforeSyncCells(state)} label="Ack before sync" />
          <span>{done ? other : <span aria-hidden="true">{other}</span>}</span>
        </p>
      </div>
      <p className={styles.note}>
        An illustration of ack-after-sync, not a crash test.{" "}
        <a href={TESTING_URL}>
          The real test cuts power on LazyFS. CI runs crash simulations on 2,000 seeds.
        </a>
      </p>
    </div>
  );
}
