"use client";

import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { useEffect, useState } from "react";
import { EASE_OUT, HOVER, PRESS, SPRING, TAP } from "@/lib/motion";
import styles from "./HeroStage.module.css";

export const HERO_AGENTS = [
  {
    id: "claude",
    name: "Claude Code",
    session: "claude · ~/api",
    prompt: "Reject stale updates on PATCH /orders.",
    reply: "Rule repo/api/etag says to return 412, not 409:",
    cmd: "return StatusCode::PRECONDITION_FAILED;",
  },
  {
    id: "codex",
    name: "Codex",
    session: "codex · ~/api",
    prompt: "Add the precondition check to /invoices.",
    reply: "Loaded repo/api/etag at chat start:",
    cmd: "if !etag.matches(req) { return Err(Status::PreconditionFailed); }",
  },
  {
    id: "copilot",
    name: "Copilot",
    session: "copilot · ~/api",
    prompt: "Write the test for a stale ETag on /orders.",
    reply: "Testing 412 from repo/api/etag:",
    cmd: "assert_eq!(res.status(), 412);",
  },
  {
    id: "vscode",
    name: "VS Code",
    session: "vscode · ~/api",
    prompt: "Commit the stale ETag handler.",
    reply: "Using user/git/signoff from AtticDB:",
    cmd: 'git commit -s -m "fix(orders): return 412 on stale ETag"',
  },
] as const;

const CARD_SPRING = { type: "spring", stiffness: 380, damping: 34, mass: 0.9 } as const;
const LID_SPRING = { type: "spring", stiffness: 320, damping: 24, mass: 0.8 } as const;
const AUTO_MS = 3600;

export default function HeroStage() {
  const [agentIdx, setAgentIdx] = useState(0);
  const [paused, setPaused] = useState(false);
  const still = useReducedMotion();

  useEffect(() => {
    if (still || paused) return;
    const timer = window.setInterval(() => {
      setAgentIdx((i) => (i + 1) % HERO_AGENTS.length);
    }, AUTO_MS);
    return () => window.clearInterval(timer);
  }, [still, paused]);

  const agent = HERO_AGENTS[agentIdx];

  return (
    <div
      className={styles.stage}
      data-ui="hero-stage"
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
      onFocusCapture={() => setPaused(true)}
      onBlurCapture={() => setPaused(false)}
    >
      {/* Top surface: the durable local store on your machine */}
      <div className={styles.store}>
        <div className={styles.storeRow}>
          <span className={styles.storeBrand}>
            <svg className={styles.mark} viewBox="0 0 64 64" aria-hidden="true">
              <motion.path
                key={`roof-${agent.id}`}
                className={styles.roof}
                d="M32 3 63 30 55 38.5 32 19 9 38.5 1 30z"
                initial={still ? false : { y: -8 }}
                animate={{ y: 0 }}
                transition={LID_SPRING}
              />
              <motion.rect
                key={`box-${agent.id}`}
                className={styles.box}
                x="19"
                y="39"
                width="26"
                height="22"
                rx="3"
                initial={still ? false : { y: 4, scale: 0.94 }}
                animate={{ y: 0, scale: 1 }}
                transition={CARD_SPRING}
              />
            </svg>
            <span>Saved once on your machine</span>
          </span>
          <span className={styles.storeStatus}>
            <span className={styles.dot} aria-hidden="true" />
            <span>2 rules in ~/.local/share/atticdb</span>
          </span>
        </div>

        <div className={styles.facts}>
          <code className={styles.fact}>
            <span className={styles.factKey}>repo/api/etag</span>{" "}
            <span className={styles.factVal}>Return 412 on stale ETag, not 409.</span>
          </code>
          <code className={styles.fact}>
            <span className={styles.factKey}>user/git/signoff</span>{" "}
            <span className={styles.factVal}>Sign off every commit with git commit -s.</span>
          </code>
        </div>
      </div>

      {/* Animated handoff beam */}
      <div className={styles.handoff} aria-hidden="true">
        <span className={styles.beamTrack}>
          <motion.span
            key={`beam-${agent.id}`}
            className={styles.beamFill}
            initial={still ? false : { scaleY: 0 }}
            animate={{ scaleY: 1 }}
            transition={{ duration: 0.36, ease: EASE_OUT }}
          />
        </span>
        <span className={styles.handoffText}>Loaded into every new chat</span>
      </div>

      {/* Interactive agent switcher + spring-animated chat card */}
      <div className={styles.deck}>
        <fieldset className={styles.switcher}>
          <legend className="visually-hidden">Coding agent</legend>
          <span className={styles.tabs}>
            {HERO_AGENTS.map((a, idx) => {
              const checked = idx === agentIdx;
              return (
                <motion.label
                  key={a.id}
                  className={styles.tab}
                  whileHover={HOVER}
                  whileTap={TAP}
                  transition={PRESS}
                >
                  <input
                    type="radio"
                    name="hero-agent"
                    value={a.id}
                    checked={checked}
                    onChange={() => setAgentIdx(idx)}
                  />
                  {checked && (
                    <motion.span
                      layoutId="hero-agent-pill"
                      className={styles.tabPill}
                      transition={SPRING}
                      aria-hidden="true"
                    />
                  )}
                  <span className={styles.tabText}>{a.name}</span>
                  {checked && !still && !paused && (
                    <motion.span
                      key={`bar-${agent.id}`}
                      className={styles.progress}
                      initial={{ scaleX: 0 }}
                      animate={{ scaleX: 1 }}
                      transition={{ duration: AUTO_MS / 1000, ease: "linear" }}
                      aria-hidden="true"
                    />
                  )}
                </motion.label>
              );
            })}
          </span>
        </fieldset>

        <div className={styles.viewport}>
          <AnimatePresence mode="popLayout" initial={false}>
            <motion.div
              key={agent.id}
              className={styles.chatCard}
              initial={still ? false : { opacity: 0, y: 16, scale: 0.985 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={still ? undefined : { opacity: 0, y: -12, scale: 0.985 }}
              transition={CARD_SPRING}
            >
              <div className={styles.chatHead}>
                <span>{`New chat in ${agent.name}`}</span>
                <code className={styles.sessionCode}>{agent.session}</code>
              </div>

              <div className={styles.promptBlock}>
                <span className={styles.speaker}>You</span>
                <p className={styles.promptText}>{agent.prompt}</p>
              </div>

              <motion.div
                className={styles.replyBlock}
                initial={still ? false : { opacity: 0, y: 8 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ duration: 0.3, delay: still ? 0 : 0.08, ease: EASE_OUT }}
              >
                <span className={styles.speaker}>{agent.name}</span>
                <p className={styles.replyText}>{agent.reply}</p>
                <code className={styles.cmdCode}>{agent.cmd}</code>
              </motion.div>
            </motion.div>
          </AnimatePresence>
        </div>
      </div>
    </div>
  );
}
