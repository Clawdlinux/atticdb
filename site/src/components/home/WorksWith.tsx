"use client";

import { Fragment, useEffect, useState } from "react";
import Reveal from "@/components/Reveal";
import styles from "./WorksWith.module.css";

const APPS = ["Claude Code", "GitHub Copilot CLI", "VS Code", "Codex"];
const SYSTEMS = ["macOS", "Linux", "Windows"];
const NAMES = [...APPS, ...SYSTEMS];
const STEP_MS = 2000;

// "a, b, c and d"
function joined(names: string[], offset: number, lit: number) {
  return names.map((name, i) => (
    <Fragment key={name}>
      {i > 0 && (i === names.length - 1 ? " and " : ", ")}
      <span className={styles.name} data-lit={offset + i === lit}>
        {name}
      </span>
    </Fragment>
  ));
}

export default function WorksWith() {
  const [lit, setLit] = useState(0);

  useEffect(() => {
    const still = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    if (still) return;
    const timer = window.setInterval(() => setLit((i) => (i + 1) % NAMES.length), STEP_MS);
    return () => window.clearInterval(timer);
  }, []);

  return (
    <section className={styles.works} aria-labelledby="works-title">
      <Reveal className={styles.statement}>
        <h2 id="works-title" className={styles.lead}>
          Wires itself into
        </h2>
        <p className={styles.names}>
          {joined(APPS, 0, lit)}. On {joined(SYSTEMS, APPS.length, lit)}.
        </p>
      </Reveal>
    </section>
  );
}
