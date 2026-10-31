import Reveal from "@/components/Reveal";
import { HOME_FAQ, type Qa } from "@/content/faq";
import styles from "./Faq.module.css";

type FaqProps = {
  items?: Qa[];
  heading?: string;
  id?: string;
};

export default function Faq({
  items = HOME_FAQ,
  heading = "Questions",
  id = "faq-title",
}: FaqProps) {
  return (
    <section className={`bleed ${styles.faq}`} aria-labelledby={id}>
      <Reveal className={styles.head}>
        <h2 id={id} className={styles.title}>
          {heading}
        </h2>
      </Reveal>
      {items.map(({ q, a }, i) => (
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
