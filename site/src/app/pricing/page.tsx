import type { CSSProperties } from "react";
import CopyCommand from "@/components/CopyCommand";
import Faq from "@/components/home/Faq";
import JsonLd from "@/components/JsonLd";
import Reveal from "@/components/Reveal";
import { PRICING_FAQ } from "@/content/faq";
import { PLAN_FEATURES, PLANS, PRICING_NOTES } from "@/content/pricing";
import { faqJsonLd, pageMetadata } from "@/lib/seo";
import styles from "./pricing.module.css";

export const metadata = pageMetadata("/pricing");

function step(i: number): CSSProperties {
  return { "--i": i } as CSSProperties;
}

export default function PricingPage() {
  return (
    <>
      <JsonLd data={faqJsonLd(PRICING_FAQ)} />
      <section className={styles.hero} aria-labelledby="pricing-hero-title">
        <h1 id="pricing-hero-title" className={styles.title}>
          <span className={styles.line}>
            <span className={styles.rise} style={step(0)}>
              Free on your disk.
            </span>
          </span>{" "}
          <span className={styles.line}>
            <span className={styles.rise} style={step(1)}>
              Cloud when you want it.
            </span>
          </span>
        </h1>
        <p className={styles.lead}>
          Run AtticDB free on your own machine. The cloud is planned, and you can join the waitlist
          now.
        </p>
      </section>

      <section className={styles.plansSection} aria-label="Plans">
        <div className={styles.plans}>
          {PLANS.map((plan, i) => {
            const isLocal = plan.id === "local";
            const isPro = plan.id === "pro";
            return (
              <article
                key={plan.id}
                className={`${styles.plan} ${isLocal ? styles.local : ""}`}
                style={step(i)}
              >
                <h2 className={styles.planName}>{plan.name}</h2>
                <p className={styles.price}>{plan.price}</p>
                <p className={styles.terms}>
                  {plan.unit ? `${plan.price} ${plan.unit}. ` : null}
                  <span className={styles.status}>{plan.status}.</span>
                </p>
                <ul className={styles.includes}>
                  {plan.includes.map((item) => (
                    <li key={item}>{item}</li>
                  ))}
                </ul>
                <div className={styles.action}>
                  {isLocal ? (
                    <CopyCommand label={plan.cta.label} tone="bone" />
                  ) : (
                    <a
                      className={`${styles.cta} ${isPro ? styles.ctaAccent : ""}`}
                      href={plan.cta.href}
                    >
                      {plan.cta.label}
                    </a>
                  )}
                </div>
              </article>
            );
          })}
        </div>
      </section>

      <section className={styles.notesSection} aria-labelledby="notes-title">
        <Reveal className={styles.notesHead}>
          <h2 id="notes-title" className={styles.notesTitle}>
            No per-call fees
          </h2>
        </Reveal>
        <div className={styles.notesBody}>
          {PRICING_NOTES.map((note, i) => (
            <Reveal key={note} delay={0.075 * (i + 1)}>
              <p className={styles.note}>{note}</p>
            </Reveal>
          ))}
        </div>
      </section>

      <section className={`bleed ${styles.comparison}`} aria-labelledby="comparison-title">
        <Reveal>
          <h2 id="comparison-title" className={styles.comparisonTitle}>
            Compare plans
          </h2>
        </Reveal>
        <Reveal delay={0.075}>
          <div
            className={styles.tableRegion}
            role="region"
            aria-label="Plan comparison"
            tabIndex={0}
            data-ui="plan-comparison"
          >
            <table className={styles.table}>
              <caption className="visually-hidden">
                Feature comparison across Local, Cloud Pro, Cloud Team and Your cloud plans
              </caption>
              <thead>
                <tr>
                  <th scope="col">Feature</th>
                  {PLANS.map((plan) => (
                    <th key={plan.id} scope="col">
                      {plan.name}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {PLAN_FEATURES.map((row) => (
                  <tr key={row.feature}>
                    <th scope="row">{row.feature}</th>
                    {PLANS.map((plan) => (
                      <td key={plan.id}>{row.values[plan.id]}</td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Reveal>
      </section>

      <Faq items={PRICING_FAQ} id="pricing-faq-title" />
    </>
  );
}
