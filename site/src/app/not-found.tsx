import type { Metadata } from "next";
import Image from "next/image";
import Link from "next/link";
import Reveal from "@/components/Reveal";
import { creditFor } from "@/content/credits";
import { STAGGER } from "@/lib/motion";
import { absoluteUrl } from "@/lib/site";
import styles from "./not-found.module.css";

export const metadata: Metadata = {
  title: { absolute: "Nothing in the attic. | AtticDB" },
  alternates: { canonical: absoluteUrl("/404") },
  robots: { index: false, follow: true },
};

// What is in the attic: the same archival pictures as the home page, in one row.
const PHOTOS = [
  {
    file: "/images/ibm-350-ramac-disk.jpg",
    width: 900,
    height: 1200,
    alt: "A tall stack of metal disk platters beside its access arm and wiring, behind museum glass.",
    caption: "IBM 350 RAMAC disk mechanism.",
  },
  {
    file: "/images/titan-core-memory-board.jpg",
    width: 1280,
    height: 976,
    alt: "Two hinged circuit boards, each holding a dense grid of tiny ferrite rings threaded on wire.",
    caption: "Titan core memory board.",
  },
  {
    file: "/images/ncr-304-tape-reel.jpg",
    width: 932,
    height: 960,
    alt: "A metal computer tape reel with three cut-outs, lying on a quilted cloth.",
    caption: "NCR 304 tape reel, 1959.",
  },
  {
    file: "/images/punched-paper-tape.jpg",
    width: 1600,
    height: 1066,
    alt: "Two coils of punched paper tape, one 5-hole and one 8-hole, on a pale ground.",
    caption: "Punched paper tape.",
  },
];

export default function NotFound() {
  return (
    <section className={styles.page} aria-labelledby="not-found-title">
      <h1 id="not-found-title" className={styles.title} aria-label="Nothing in the attic.">
        <span className={styles.line}>
          <span className={styles.rise}>Nothing in</span>
        </span>{" "}
        <span className={styles.line}>
          <span className={styles.rise}>The attic.</span>
        </span>
      </h1>
      <p className={styles.lead}>That page isn&apos;t here. It may have moved, or it never existed.</p>
      <Link className={styles.home} href="/">
        Back to the home page
      </Link>

      <div className={styles.photos}>
        {PHOTOS.map((photo, i) => {
          const credit = creditFor(photo.file);
          return (
            <Reveal key={photo.file} delay={i * STAGGER}>
              <figure className={styles.figure}>
                <div className={styles.frame}>
                  <Image
                    className={`archival ${styles.img}`}
                    src={photo.file}
                    width={photo.width}
                    height={photo.height}
                    alt={photo.alt}
                    sizes="(min-width: 48rem) 25vw, 50vw"
                  />
                </div>
                <figcaption className={styles.caption}>
                  {photo.caption} {credit.creator}, {credit.licence}.
                </figcaption>
              </figure>
            </Reveal>
          );
        })}
      </div>
    </section>
  );
}
