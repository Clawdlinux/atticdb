"use client";

import { MotionConfig, motion, useReducedMotion } from "motion/react";
import dynamic from "next/dynamic";
import {
  Component,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import AgentPrompt from "@/components/AgentPrompt";
import CopyCommand from "@/components/CopyCommand";
import { ackFor } from "@/content/bench";
import { HEADLINE, HONEST, SUB, TRUST } from "@/content/hero";
import { HOVER, PRESS, SPRING, SWAP, SWAP_RISE, TAP } from "@/lib/motion";
import { WAITLIST_URL } from "@/lib/site";
import AckChip, { formatAck } from "./AckChip";
import HeroStage from "./HeroStage";
import { PlugResult, PowerChip } from "./PlugDemo";
import type { TapeHandle } from "./TapeCanvas";
import { POSTER_SRC, POSTER_VIEWS } from "./tape-poster";
import { slowdownFor, type Disk, type PlugView, type Writers } from "./tape-time";
import styles from "./Hero.module.css";

const DISKS: { value: Disk; label: string }[] = [
  { value: "nvme", label: "Local NVMe" },
  { value: "premium-v2", label: "Premium SSD v2" },
];
const WRITERS: Writers[] = [1, 4, 16];

const numbers = new Intl.NumberFormat("en-US");

function line(i: number): CSSProperties {
  return { "--i": i } as CSSProperties;
}

type OptionProps = {
  name: string;
  value: string;
  label: string;
  checked: boolean;
  onPick: () => void;
  // The id of the panel this option shows.
  controls?: string;
};

// One radio in a chip. The selected one carries the highlight, which slides between options.
function Option({ name, value, label, checked, onPick, controls }: OptionProps) {
  return (
    <motion.label
      className={styles.option}
      whileHover={HOVER}
      whileTap={TAP}
      transition={PRESS}
    >
      <input
        type="radio"
        name={name}
        value={value}
        checked={checked}
        onChange={onPick}
        aria-controls={controls}
      />
      {checked && (
        <motion.span
          layoutId={`${name}-highlight`}
          className={styles.highlight}
          transition={SPRING}
          aria-hidden="true"
        />
      )}
      <span className={styles.optionText}>{label}</span>
    </motion.label>
  );
}

type Method = "agent" | "terminal";
const METHODS: { value: Method; label: string }[] = [
  { value: "agent", label: "Ask your agent" },
  { value: "terminal", label: "Terminal" },
];

/*
 * The install switch: paste one line into your agent, or run the command yourself.
 * Both panels share one grid cell, so a switch never moves the page. CSS shows the panel of the
 * checked radio, so the switch works without JS too. With JS, the new panel rises in.
 */
function InstallSwitch() {
  const [method, setMethod] = useState<Method>("agent");
  const [moved, setMoved] = useState(false);
  const still = useReducedMotion();
  const id = useId();
  const enter = moved && !still ? { opacity: 0, y: SWAP_RISE } : false;

  return (
    <>
      <fieldset className={styles.method} data-ui="install-method">
        <legend className="visually-hidden">Install with</legend>
        <span className={styles.options}>
          {METHODS.map((m) => (
            <Option
              key={m.value}
              name="hero-install"
              value={m.value}
              label={m.label}
              checked={method === m.value}
              controls={`${id}-${m.value}`}
              onPick={() => {
                setMethod(m.value);
                setMoved(true);
              }}
            />
          ))}
        </span>
      </fieldset>
      <div className={styles.panels}>
        {METHODS.map((m) => {
          const active = method === m.value;
          return (
            <div
              key={m.value}
              id={`${id}-${m.value}`}
              className={styles.panel}
              data-method={m.value}
              data-active={active}
              role="group"
              aria-label={m.label}
            >
              {/* A new key on each switch replays the entrance for the panel that comes in. */}
              <motion.div
                key={String(active)}
                initial={active ? enter : false}
                animate={{ opacity: 1, y: 0 }}
                transition={SWAP}
              >
                {m.value === "agent" ? <AgentPrompt /> : <CopyCommand label="Install AtticDB" />}
              </motion.div>
            </div>
          );
        })}
      </div>
    </>
  );
}

function tapeLabel(writers: Writers, disk: string) {
  const who = writers === 1 ? "1 writer" : `${writers} writers`;
  return `The tape. ${who} on ${disk} send writes to a queue. Each group is laid on the tape, synced once, then acked.`;
}

// The sim and the canvas load after hydration, so they never sit in the first paint's path.
// The build-time poster shows until they draw, and stays if they never load.
const TapeCanvas = dynamic(() => import("./TapeCanvas"), { ssr: false });

// A pull made before the canvas loads waits this long for it, then gives up.
const PENDING_MS = 6000;

// If the canvas chunk fails to load, the poster stays and the rest of the hero keeps working.
class KeepPoster extends Component<{ children: ReactNode; onFail: () => void }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch() {
    this.props.onFail();
  }

  render() {
    return this.state.failed ? null : this.props.children;
  }
}

// The still frame from public/tape-poster.svg: one static file, four parts, fetched once.
// An inline <svg><use> rather than an <img>: an <img> of it became the page's LCP element.
function Poster() {
  return POSTER_VIEWS.map((v) => {
    const art = (
      <svg width={v.w} height={v.h} viewBox={`0 ${v.y} ${v.w} ${v.h}`} aria-hidden="true" focusable="false">
        <use href={`${POSTER_SRC}#${v.id}`} />
      </svg>
    );
    return v.layer === "left" ? (
      <div key={v.id} className={styles.posterLeft} data-mode={v.mode}>
        {art}
      </div>
    ) : (
      <div key={v.id} className={styles.posterTape} data-mode={v.mode} style={{ "--tx": `${v.tx}px` } as CSSProperties}>
        {art}
      </div>
    );
  });
}

type HeroProps = {
  // Shown right after the honest line, so the first screen has no benchmark words.
  // The ack chip and the plug band follow it, below the fold.
  afterHonest?: ReactNode;
};

function HeroBody({ afterHonest }: HeroProps) {
  const [disk, setDisk] = useState<Disk>("nvme");
  const [writers, setWriters] = useState<Writers>(1);
  const [view, setView] = useState<PlugView>({ phase: "idle", outcome: null });
  const [groups, setGroups] = useState(0);
  const [live, setLive] = useState(false);
  const handle = useRef<TapeHandle | null>(null);
  const pending = useRef(false);
  const ack = ackFor(disk, writers);
  const diskLabel = DISKS.find((d) => d.value === disk)?.label ?? "";
  const powered = view.phase !== "cut";
  const label = tapeLabel(writers, diskLabel);

  const pull = useCallback(() => {
    if (handle.current) {
      handle.current.pull();
      return;
    }
    pending.current = true;
    setView((v) => ({ ...v, phase: "armed" }));
    giveUp.current = window.setTimeout(() => {
      if (!pending.current) return;
      pending.current = false;
      setView((v) => ({ ...v, phase: "idle" }));
    }, PENDING_MS);
  }, []);
  const onLive = useCallback(() => setLive(true), []);
  const onFail = useCallback(() => {
    pending.current = false;
    setView({ phase: "idle", outcome: null });
  }, []);
  const giveUp = useRef<number | null>(null);
  useEffect(() => () => {
    if (giveUp.current !== null) window.clearTimeout(giveUp.current);
  }, []);
  const onGroup = useCallback(() => setGroups((g) => g + 1), []);

  return (
    <section className={styles.hero} aria-labelledby="hero-title">
      <div className={styles.stage}>
        <h1 id="hero-title" className={styles.title}>
          <span className={styles.line}>
            <span className={styles.rise} style={line(0)}>
              {HEADLINE[0]}
            </span>
          </span>{" "}
          <span className={styles.line}>
            <span className={styles.rise} style={line(1)}>
              {HEADLINE[1]}
            </span>
          </span>
        </h1>
        <div className={styles.copy}>
          <p className={styles.lead}>{SUB}</p>
          <div className={styles.actions}>
            <InstallSwitch />
            <motion.a
              className={styles.waitlist}
              href={WAITLIST_URL}
              whileHover={HOVER}
              whileTap={TAP}
              transition={PRESS}
            >
              Join the cloud waitlist
            </motion.a>
          </div>
        </div>
        <div className={styles.hub}>
          <HeroStage />
        </div>
      </div>

      <p className={styles.honest} data-ui="honest">
        {HONEST}
      </p>

      {afterHonest}

      <div className={styles.proof} data-ui="proof">
        <p className={styles.trust}>{TRUST}</p>
        <span className={styles.ackBox}>
          <AckChip pulse={groups} powered={powered} />
        </span>
        <p className="visually-hidden" role="status">
          {`${diskLabel}, ${writers} ${writers === 1 ? "writer" : "writers"}: AtticDB acked in ${formatAck(ack.p50us)} at p50, ${numbers.format(ack.acksPerSec)} acks/s.`}
        </p>
      </div>

      <div className={styles.band} data-ui="band">
        <div className={styles.strip}>
          <fieldset className={styles.chip} data-ui="disk">
            <legend className={styles.chipLabel}>Disk</legend>
            <span className={styles.options}>
              {DISKS.map((d) => (
                <Option
                  key={d.value}
                  name="hero-disk"
                  value={d.value}
                  label={d.label}
                  checked={disk === d.value}
                  onPick={() => setDisk(d.value)}
                />
              ))}
            </span>
          </fieldset>

          <fieldset className={styles.chip} data-ui="writers">
            <legend className={styles.chipLabel}>Writers</legend>
            <span className={styles.readout} aria-hidden="true">
              {numbers.format(ack.acksPerSec)} acks/s
            </span>
            <span className={styles.options}>
              {WRITERS.map((w) => (
                <Option
                  key={w}
                  name="hero-writers"
                  value={String(w)}
                  label={String(w)}
                  checked={writers === w}
                  onPick={() => setWriters(w)}
                />
              ))}
            </span>
          </fieldset>

          <PowerChip view={view} onRun={pull} />
        </div>
        <div className={styles.tape} data-ui="band-canvas" data-live={live}>
          {/* Before the canvas draws, and without JS, this is the picture. One of the two is visible. */}
          <div className={styles.posters} data-ui="poster" role="img" aria-label={label}>
            <Poster />
          </div>
          <KeepPoster onFail={onFail}>
            <TapeCanvas
              disk={disk}
              writers={writers}
              label={label}
              onLive={onLive}
              onView={setView}
              onGroup={onGroup}
              handleRef={handle}
              pendingRef={pending}
            />
          </KeepPoster>
        </div>
        <p className={styles.timeCaption}>
          Time slowed {numbers.format(slowdownFor(disk, writers))}x. Real p50, {diskLabel},{" "}
          {writers === 1 ? "1 writer" : `${writers} writers`}:{" "}
          <span className={styles.readout}>{numbers.format(ack.p50us)} µs</span>
        </p>
      </div>

      <PlugResult view={view} />
    </section>
  );
}

// Its own MotionConfig: motion stays out of the root layout, so pages without motion never load it.
export default function Hero(props: HeroProps) {
  return (
    <MotionConfig reducedMotion="user">
      <HeroBody {...props} />
    </MotionConfig>
  );
}
