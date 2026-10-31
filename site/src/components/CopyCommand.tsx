"use client";

import type { Transition } from "motion/react";
import { useEffect, useRef, useState } from "react";
import Swap from "@/components/Swap";
import { INSTALL_CMD } from "@/lib/site";
import styles from "./CopyCommand.module.css";

type CopyCommandProps = {
  command?: string;
  id?: string;
  label?: string;
  // "bone" sets the button in oxide ink for a bone field, so the accent stays the page's one action.
  // "ink" sets the button in bone on a dark panel, for a list of commands under one accent action.
  tone?: "accent" | "bone" | "ink";
  // The label swap. Defaults to Swap's own.
  swap?: { transition: Transition; rise: number };
};

type Status = "idle" | "copied" | "selected";

const RESET_MS = 2000;

const BUTTON_TONE = { accent: "button--accent", bone: "button--field", ink: "button--ink" } as const;

export default function CopyCommand({
  command = INSTALL_CMD,
  id,
  label = "Copy",
  tone = "accent",
  swap,
}: CopyCommandProps) {
  const [status, setStatus] = useState<Status>("idle");
  const codeRef = useRef<HTMLElement>(null);
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  function selectCode() {
    const el = codeRef.current;
    const selection = window.getSelection();
    if (!el || !selection) return;
    const range = document.createRange();
    range.selectNodeContents(el);
    selection.removeAllRanges();
    selection.addRange(range);
  }

  async function copy() {
    window.clearTimeout(timer.current);
    try {
      await navigator.clipboard.writeText(command);
      setStatus("copied");
    } catch {
      // No clipboard access (insecure origin or denied). Select it for a manual copy.
      selectCode();
      setStatus("selected");
    }
    timer.current = window.setTimeout(() => setStatus("idle"), RESET_MS);
  }

  const shown = status === "copied" ? "Copied" : status === "selected" ? "Selected" : label;

  return (
    <div className={`${styles.box} ${tone === "bone" ? styles.bone : ""}`} id={id}>
      <pre className={styles.code}>
        <code ref={codeRef}>{command}</code>
      </pre>
      <button
        type="button"
        className={`button ${BUTTON_TONE[tone]} ${styles.action}`}
        onClick={copy}
        data-status={status}
      >
        {/*
          Every label sits in one grid cell, so the button never changes width.
          The hidden ones only size the cell; the shown one swaps in with a short rise.
        */}
        <span className={styles.labels}>
          {[label, "Copied", "Selected"].map((text) => (
            <span key={text} className={styles.sizer} aria-hidden="true">
              {text}
            </span>
          ))}
          <Swap value={shown} className={styles.shown} mode="sync" {...swap} />
        </span>
      </button>
      <span className="visually-hidden" role="status">
        {status === "copied"
          ? "Command copied."
          : status === "selected"
            ? "Command selected. Copy it with your keyboard."
            : ""}
      </span>
    </div>
  );
}
