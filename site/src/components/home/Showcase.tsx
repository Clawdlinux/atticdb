"use client";

import { AnimatePresence, motion, useIsPresent } from "motion/react";
import { useId, useRef, useState, type ReactNode } from "react";
import Reveal from "@/components/Reveal";
import { HOVER, PRESS, SPRING, SWAP, SWAP_RISE, TAP } from "@/lib/motion";
import { SAMPLE_MEMORY, searchMemory, type MemoryRecord } from "@/content/memory";
import styles from "./Showcase.module.css";

const START = "why did we pick ext4";
const EXAMPLES = ["deploy region", "user prefs", "flaky test"];

const day = new Intl.DateTimeFormat("en-GB", {
  day: "numeric",
  month: "short",
  year: "numeric",
  timeZone: "UTC",
});

function escape(word: string) {
  return word.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Wraps every query word found in the text in <mark>, case-insensitively.
export function markWords(text: string, query: string): ReactNode[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean).map(escape);
  if (!words.length) return [text];
  const parts = text.split(new RegExp(`(${words.join("|")})`, "gi"));
  return parts.map((part, i) => (i % 2 === 1 ? <mark key={i}>{part}</mark> : part));
}

// A result row: rises in when it starts to match, fades out when it stops, slides as others go.
function Row({ record, query }: { record: MemoryRecord; query: string }) {
  const present = useIsPresent();
  return (
    <motion.li
      className={styles.row}
      data-present={present}
      aria-hidden={present ? undefined : true}
      layout="position"
      initial={{ opacity: 0, y: SWAP_RISE }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, transition: SWAP }}
      transition={{ ...SWAP, layout: SPRING }}
    >
      <span className={styles.meta}>
        <span className={styles.kind}>{record.kind}</span>
        <code>{record.session ?? record.key}</code>
        <time className={styles.date} dateTime={record.at}>
          {day.format(new Date(record.at))}
        </time>
      </span>
      <span className={styles.text}>{markWords(record.text, query)}</span>
    </motion.li>
  );
}

export default function Showcase() {
  const [query, setQuery] = useState(START);
  const inputRef = useRef<HTMLInputElement>(null);
  const id = useId();
  const results = searchMemory(query);
  const empty = query.trim() === "";

  function fill(example: string) {
    setQuery(example);
    inputRef.current?.focus();
  }

  return (
    <section className={`bleed ${styles.showcase}`} aria-labelledby={`${id}-title`}>
      <Reveal>
        <h2 id={`${id}-title`} className={styles.title}>
          <label htmlFor={`${id}-q`}>Search what your agent remembered</label>
        </h2>
      </Reveal>

      <Reveal delay={0.075} className={styles.line}>
        <input
          ref={inputRef}
          id={`${id}-q`}
          className={styles.input}
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          autoComplete="off"
          spellCheck={false}
          data-ui="query"
        />
      </Reveal>

      <div className={styles.body}>
        <Reveal delay={0.15} className={styles.answer}>
          <div className={styles.panel} data-ui="results" aria-live="polite">
            <p className={styles.count}>
              {empty
                ? "Newest records"
                : `${results.length} of ${SAMPLE_MEMORY.length} records match, newest first`}
            </p>
            {results.length ? (
              <ol className={styles.rows}>
                <AnimatePresence initial={false} mode="popLayout">
                  {results.map((r) => (
                    <Row key={r.at} record={r} query={query} />
                  ))}
                </AnimatePresence>
              </ol>
            ) : (
              <p className={styles.none}>Nothing matches every word. Try fewer words.</p>
            )}
          </div>
        </Reveal>

        <Reveal delay={0.225} className={styles.aside}>
          <p className={styles.note}>
            The same search as <code>memory_search</code>. Every word must match, across every
            session and fact. No embeddings, no model call.
          </p>
          <ul className={styles.examples}>
            {EXAMPLES.map((example) => (
              <li key={example}>
                <motion.button
                  type="button"
                  className={styles.example}
                  onClick={() => fill(example)}
                  whileHover={HOVER}
                  whileTap={TAP}
                  transition={PRESS}
                >
                  {example}
                </motion.button>
              </li>
            ))}
          </ul>
        </Reveal>
      </div>
    </section>
  );
}
