"use client";

import { MotionConfig, type Transition } from "motion/react";
import { Fragment, useEffect, useRef, useState } from "react";
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
    // "tokens" wraps only at spaces, so a flag like --dry-run never splits at its hyphen.
  // A long token breaks after a comma or slash, or before "=". One with none of those
  // may break anywhere. The copied text is unchanged.
  wrap?: "tokens";
};

type Status = "idle" | "copied" | "selected";

const RESET_MS = 2000;

// Longer than this, a token may not fit a phone's line on its own, so it gets break points.
const LONG_TOKEN = 20;

// A long token's break points: after a comma or a slash, and before an equals sign.
export function breakPoints(token: string): string[] {
  return token.split(/(?<=[,/])|(?==)/).filter(Boolean);
}

function longToken(token: string) {
  const parts = breakPoints(token);
  if (parts.length === 1) {
    return (
      <span className={styles.long} data-token="long">
        {token}
      </span>
    );
  }
  // <wbr> adds no text, so the selection and the copy read back as the exact token.
  return (
    <span data-token="split">
      {parts.map((part, i) => (
        <Fragment key={i}>
          {i > 0 && <wbr />}
          <span className={part.length > LONG_TOKEN ? styles.long : styles.token}>{part}</span>
        </Fragment>
      ))}
    </span>
  );
}

// Joined by plain spaces, the tokens read back as the exact command.
function tokens(command: string) {
  return command.split(" ").map((token, i) => (
    <Fragment key={i}>
      {i > 0 && " "}
      {token.length > LONG_TOKEN ? (
        longToken(token)
      ) : (
        <span className={styles.token} data-token="word">
          {token}
        </span>
      )}
    </Fragment>
  ));
}

const BUTTON_TONE = { accent: "button--accent", bone: "button--field", ink: "button--ink" } as const;

function CopyCommandBody({
  command = INSTALL_CMD,
  id,
  label = "Copy",
  tone = "accent",
  swap,
  wrap,
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
      <pre className={`${styles.code} ${wrap === "tokens" ? styles.tokens : ""}`}>
        <code ref={codeRef}>{wrap === "tokens" ? tokens(command) : command}</code>
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

// Its own MotionConfig: motion stays out of the root layout, so pages without motion never load it.
export default function CopyCommand(props: CopyCommandProps) {
  return (
    <MotionConfig reducedMotion="user">
      <CopyCommandBody {...props} />
    </MotionConfig>
  );
}
