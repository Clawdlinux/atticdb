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
        <p className={styles.lead}>
          Wires itself into Claude Code, GitHub Copilot CLI, VS Code and Codex.
          Pick your system and apps for the exact commands to run.
        </p>
      </Reveal>
      <Reveal delay={0.075}>
        <Installer />
      </Reveal>
    </section>
  );
}
