import ArchivalPicture from "@/components/ArchivalPicture";
import Reveal from "@/components/Reveal";
import { creditFor } from "@/content/credits";
import styles from "./Intro.module.css";

// sizes: the rendered width, measured at 390, 768 and 1440 px. The container stops at 1280 px.
const PHOTOS = [
  {
    file: "/images/ibm-350-ramac-disk.jpg",
    width: 900,
    height: 1200,
    alt: "A tall stack of metal disk platters beside its access arm and wiring, behind museum glass.",
    caption: "IBM 350 RAMAC disk mechanism, Computer History Museum.",
    className: styles.portrait,
    sizes: "(min-width: 90rem) 400px, (min-width: 48rem) 28vw, 62vw",
  },
  {
    file: "/images/titan-core-memory-board.jpg",
    width: 1280,
    height: 976,
    alt: "Two hinged circuit boards, each holding a dense grid of tiny ferrite rings threaded on wire.",
    caption: "Core memory board from a Titan missile guidance computer.",
    className: styles.landscape,
    sizes: "(min-width: 90rem) 670px, (min-width: 48rem) 47vw, 78vw",
  },
  {
    file: "/images/ncr-304-tape-reel.jpg",
    width: 932,
    height: 960,
    alt: "A metal computer tape reel with three cut-outs, lying on a quilted cloth.",
    caption: "NCR 304 computer tape reel, 1959.",
    className: styles.square,
    sizes: "(min-width: 90rem) 360px, (min-width: 48rem) 25vw, 52vw",
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
                <ArchivalPicture
                  className={`archival ${styles.img}`}
                  src={photo.file}
                  width={photo.width}
                  height={photo.height}
                  alt={photo.alt}
                  sizes={photo.sizes}
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
