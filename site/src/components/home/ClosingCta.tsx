import ArchivalPicture from "@/components/ArchivalPicture";
import CopyCommand from "@/components/CopyCommand";
import Reveal from "@/components/Reveal";
import { creditFor } from "@/content/credits";
import { GITHUB_URL } from "@/lib/site";
import styles from "./ClosingCta.module.css";

const TAPE = "/images/punched-paper-tape.jpg";

export default function ClosingCta() {
  const credit = creditFor(TAPE);
  return (
    <section className={styles.cta} aria-labelledby="cta-title">
      <h2 id="cta-title" className={styles.title}>
        <Reveal as="span" className={styles.top}>
          Ready to
        </Reveal>{" "}
        <Reveal as="span" delay={0.075} className={styles.bottom}>
          remember?
        </Reveal>
      </h2>

      <div className={styles.band}>
        <Reveal delay={0.15}>
          <figure className={styles.figure}>
            <div className={styles.strip}>
              <ArchivalPicture
                className={`archival ${styles.img}`}
                src={TAPE}
                width={1600}
                height={1066}
                alt="Two coils of punched paper tape, one 5-hole and one 8-hole, on a pale ground."
                sizes="(min-width: 90rem) 1280px, 92vw"
              />
            </div>
            <figcaption className={styles.caption}>
              Punched paper tape, 5-hole and 8-hole. {credit.creator}, {credit.licence}.
            </figcaption>
          </figure>
        </Reveal>

        <Reveal delay={0.225} className={styles.choice}>
          <div className={styles.install}>
            <CopyCommand label="Install AtticDB" />
          </div>
          <span className={styles.or}>or</span>
          <a className={`button button--ink ${styles.star}`} href={GITHUB_URL}>
            Star on GitHub
          </a>
        </Reveal>

        <Reveal delay={0.3}>
          <p className={styles.after}>
            Then run <code>atticdb setup</code> to connect your apps.
          </p>
        </Reveal>
      </div>
    </section>
  );
}
