import Installer from "@/components/Installer";
import Reveal from "@/components/Reveal";
import styles from "./InstallSection.module.css";

// A bone field, so it parts from How it works on the dark page above.
export default function InstallSection() {
  return (
    <section id="install" className={`bleed ${styles.install}`} aria-labelledby="install-title">
      <Reveal className={styles.head}>
        <h2 id="install-title" className={styles.title}>
          Install it your way
        </h2>
        <p className={styles.lead}>
          Pick your system and your apps. The commands below are exactly what to run.
        </p>
      </Reveal>
      <Reveal delay={0.075}>
        <Installer />
      </Reveal>
    </section>
  );
}
