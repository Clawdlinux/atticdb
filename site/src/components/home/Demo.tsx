import Reveal from "@/components/Reveal";
import { DOCS_URL } from "@/lib/site";
import styles from "./Demo.module.css";

export const DEMO_SRC = "/demo/remember.svg";
// The SVG's #still fragment turns its animation off, so it shows the last frame.
export const DEMO_STILL = `${DEMO_SRC}#still`;
// Phones get a taller, narrower layout of the same story, so its text stays 16px or more at 375px.
export const DEMO_NARROW_SRC = "/demo/remember-narrow.svg";
export const DEMO_NARROW_STILL = `${DEMO_NARROW_SRC}#still`;
export const DEMO_NARROW_MEDIA = "(max-width: 40rem)";
export const DEMO_ALT =
  'A terminal in the shop repo runs atticdb fact set --key repo/shop/rule --content "Use pnpm, not npm." The rule is saved. ' +
  "Later, a new chat in Claude Code starts with the AtticDB memory block, which lists that rule. " +
  "Asked to add lodash, the agent says the repo uses pnpm and runs pnpm add lodash.";
export const TRY_IT_URL = `${DOCS_URL}/install#try-it-in-1-minute`;

// The 1-minute story, as an animated SVG. Reduced motion gets its last frame.
export default function Demo() {
  return (
    <section className={`bleed ${styles.demo}`} aria-labelledby="demo-heading">
      <Reveal className={styles.head}>
        <h2 id="demo-heading" className={styles.title}>
          Save it once
        </h2>
        <p className={styles.lead}>
          Save a rule from your terminal. Every new chat in that repo starts with it.
        </p>
        <a className={styles.link} href={TRY_IT_URL}>
          Try it in 1 minute
        </a>
      </Reveal>
      <Reveal delay={0.075} className={styles.figure}>
        <picture>
          {/* The first match wins: phone and still, phone, then still. */}
          <source
            media={`${DEMO_NARROW_MEDIA} and (prefers-reduced-motion: reduce)`}
            srcSet={DEMO_NARROW_STILL}
            width={400}
            height={884}
          />
          <source media={DEMO_NARROW_MEDIA} srcSet={DEMO_NARROW_SRC} width={400} height={884} />
          <source media="(prefers-reduced-motion: reduce)" srcSet={DEMO_STILL} width={640} height={580} />
          <img
            className={styles.img}
            src={DEMO_SRC}
            alt={DEMO_ALT}
            width={640}
            height={580}
            loading="lazy"
            decoding="async"
          />
        </picture>
      </Reveal>
    </section>
  );
}
