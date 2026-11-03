import AgentPrompt from "@/components/AgentPrompt";
import Installer from "@/components/Installer";
import Reveal from "@/components/Reveal";
import styles from "./InstallSection.module.css";

export default function InstallSection() {
  return (
    <section id="install" className={`bleed ${styles.install}`} aria-labelledby="install-title">
      <Reveal className={styles.head}>
        <h2 id="install-title" className={styles.title}>
          Install it your way
        </h2>
        <div className={styles.agent}>
          <p className={styles.lead}>Paste this into your coding agent.</p>
          {/* The installer's Copy all is this section's accent. */}
          <AgentPrompt tone="bone" />
        </div>
      </Reveal>
      <Reveal delay={0.075}>
        <p className={styles.or}>Or pick the commands yourself.</p>
        <Installer />
      </Reveal>
    </section>
  );
}
