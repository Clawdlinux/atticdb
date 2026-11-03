import AgentPrompt from "@/components/AgentPrompt";
import Installer from "@/components/Installer";
import Reveal from "@/components/Reveal";
import { HOME_FAQ } from "@/content/faq";
import Commands from "./Commands";
import styles from "./InstallSection.module.css";

// The agent prompt and the folded commands on the left, the first-visit questions on the right.
export default function InstallSection() {
  return (
    <section id="install" className={`bleed ${styles.install}`} aria-labelledby="install-title">
      <Reveal className={styles.agent}>
        <h2 id="install-title" className={styles.title}>
          Install it your way
        </h2>
        <div className={styles.prompt}>
          <p className={styles.lead}>Paste this into your coding agent.</p>
          {/* The installer's Copy all is this section's accent. */}
          <AgentPrompt tone="bone" />
        </div>
      </Reveal>
      <Commands className={styles.commands} summaryClassName={styles.summary}>
        <Installer />
      </Commands>
      <div className={styles.questions}>
        {HOME_FAQ.map(({ q, a }, i) => (
          <Reveal key={q} delay={0.06 * (i % 2) + 0.075} className={styles.pair}>
            <h3 className={styles.question}>{q}</h3>
            <p className={styles.answer}>{a}</p>
          </Reveal>
        ))}
      </div>
    </section>
  );
}
