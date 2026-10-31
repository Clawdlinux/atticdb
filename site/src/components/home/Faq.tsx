import Reveal from "@/components/Reveal";
import { HOME_FAQ } from "@/content/faq";
import styles from "./Faq.module.css";

export default function Faq() {
  return (
    <section className={`bleed ${styles.faq}`} aria-labelledby="faq-title">
      <Reveal className={styles.head}>
        <h2 id="faq-title" className={styles.title}>
          Questions
        </h2>
      </Reveal>
      {HOME_FAQ.map(({ q, a }, i) => (
        <div key={q} className={styles.pair}>
          <Reveal delay={0.06 * (i % 2)} className={styles.q}>
            <h3 className={styles.question}>{q}</h3>
          </Reveal>
          <Reveal delay={0.06 * (i % 2) + 0.075} className={styles.a}>
            <p className={styles.answer}>{a}</p>
          </Reveal>
        </div>
      ))}
    </section>
  );
}
