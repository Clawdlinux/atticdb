import Image from "next/image";
import Reveal from "@/components/Reveal";
import { creditFor } from "@/content/credits";
import styles from "./Intro.module.css";

const PHOTOS = [
  {
    file: "/images/ibm-350-ramac-disk.jpg",
    width: 900,
    height: 1200,
    alt: "A tall stack of metal disk platters beside its access arm and wiring, behind museum glass.",
    caption: "IBM 350 RAMAC disk mechanism, Computer History Museum.",
    className: styles.portrait,
  },
  {
    file: "/images/titan-core-memory-board.jpg",
    width: 1280,
    height: 976,
    alt: "Two hinged circuit boards, each holding a dense grid of tiny ferrite rings threaded on wire.",
    caption: "Core memory board from a Titan missile guidance computer.",
    className: styles.landscape,
  },
  {
    file: "/images/ncr-304-tape-reel.jpg",
    width: 932,
    height: 960,
    alt: "A metal computer tape reel with three cut-outs, lying on a quilted cloth.",
    caption: "NCR 304 computer tape reel, 1959.",
    className: styles.square,
  },
];

export default function Intro() {
  return (
    <section className={styles.intro} aria-labelledby="intro-title">
      <div className={styles.pin}>
        <div className={styles.words}>
          <Reveal>
            <h2 id="intro-title" className={styles.title}>
              On your own disk
            </h2>
          </Reveal>
          <Reveal delay={0.075}>
            <p className={styles.statement}>
              Every turn your agent takes is written to a tape on your own disk. Synced, then
              acked. Nothing leaves the machine.
            </p>
          </Reveal>
        </div>
      </div>

      <div className={styles.photos}>
        {PHOTOS.map((photo, i) => (
          <Reveal key={photo.file} delay={0.15 + i * 0.075} className={photo.className}>
            <figure className={styles.figure}>
              <div className={styles.frame}>
                <Image
                  className={`archival ${styles.img}`}
                  src={photo.file}
                  width={photo.width}
                  height={photo.height}
                  alt={photo.alt}
                  loading="lazy"
                  sizes="(min-width: 48rem) 40vw, 80vw"
                />
              </div>
              <figcaption className={styles.caption}>
                {photo.caption} {creditFor(photo.file).creator}, {creditFor(photo.file).licence}.
              </figcaption>
            </figure>
          </Reveal>
        ))}
      </div>
    </section>
  );
}
