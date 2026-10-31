import type { CSSProperties } from "react";
import Reveal from "@/components/Reveal";
import { INSTALL_CMD } from "@/lib/site";
import styles from "./HowItWorks.module.css";

const STAGES = [
  {
    duration: "About ten seconds",
    clause: "First, install AtticDB.",
    command: INSTALL_CMD,
    grow: 1,
    tone: styles.surface,
  },
  {
    duration: "Under a minute",
    clause: "Then connect your apps. One command wires them all.",
    command: "atticdb setup",
    grow: 2,
    tone: styles.ink,
  },
  {
    duration: "Every chat after that",
    clause: "After that, your agent remembers. No command for this one.",
    command: null,
    grow: 9,
    tone: styles.accent,
  },
];

export default function HowItWorks() {
  return (
    <section className={styles.how} aria-labelledby="how-title">
      <Reveal>
        <h2 id="how-title" className={styles.title}>
          Three steps, then forget about it
        </h2>
      </Reveal>

      <Reveal delay={0.075}>
        <ol className={styles.bar} data-ui="stages">
          {STAGES.map((stage) => (
            <li
              key={stage.duration}
              className={styles.stage}
              style={{ "--grow": stage.grow } as CSSProperties}
            >
              <div className={`${styles.segment} ${stage.tone}`}>
                <span className={styles.duration}>{stage.duration}</span>
              </div>
              <div className={styles.note}>
                <p>{stage.clause}</p>
                {stage.command && (
                  <pre className={styles.command}>
                    <code>{stage.command}</code>
                  </pre>
                )}
              </div>
            </li>
          ))}
        </ol>
      </Reveal>
    </section>
  );
}
