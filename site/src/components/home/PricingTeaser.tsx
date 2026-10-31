import Link from "next/link";
import Reveal from "@/components/Reveal";
import { PLANS } from "@/content/pricing";
import { WAITLIST_URL } from "@/lib/site";
import styles from "./PricingTeaser.module.css";

const pro = PLANS.find((p) => p.id === "pro")!;

export default function PricingTeaser() {
  return (
    <section className={styles.pricing} aria-labelledby="pricing-title">
      <Reveal>
        <h2 id="pricing-title" className={styles.title}>
          <span className={styles.line}>Local is free.</span>{" "}
          <span className={styles.line}>
            Cloud is <span className={styles.price}>{pro.price}</span> a month.
          </span>
        </h2>
      </Reveal>

      <div className={styles.foot}>
        <Reveal delay={0.075} className={styles.support}>
          <p>
            The binary is free forever. Cloud adds sync across your machines and backups. Planned
            pricing.
          </p>
        </Reveal>
        <Reveal delay={0.15} className={styles.actions}>
          <Link className={styles.button} href="/pricing">
            See pricing
          </Link>
          <a className={styles.second} href={WAITLIST_URL}>
            Join the cloud waitlist
          </a>
        </Reveal>
      </div>
    </section>
  );
}
