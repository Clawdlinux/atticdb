"use client";

import { MotionConfig, motion } from "motion/react";
import dynamic from "next/dynamic";
import {
  Component,
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import CopyCommand from "@/components/CopyCommand";
import { ackFor } from "@/content/bench";
import { HOVER, PRESS, SPRING, TAP } from "@/lib/motion";
import { WAITLIST_URL } from "@/lib/site";
import AckChip, { formatAck } from "./AckChip";
import { PlugResult, PowerChip } from "./PlugDemo";
import type { TapeHandle } from "./TapeCanvas";
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
};

// One radio in a chip. The selected one carries the highlight, which slides between options.
function Option({ name, value, label, checked, onPick }: OptionProps) {
  return (
    <motion.label
      className={styles.option}
      whileHover={HOVER}
      whileTap={TAP}
      transition={PRESS}
    >
      <input type="radio" name={name} value={value} checked={checked} onChange={onPick} />
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

function tapeLabel(writers: Writers, disk: string) {
  const who = writers === 1 ? "1 writer" : `${writers} writers`;
  return `The tape. ${who} on ${disk} send writes to a queue. Each group is laid on the tape, synced once, then acked.`;
}

// The sim and the canvas load after hydration, so they never sit in the first paint's path.
// The server-drawn poster shows until they draw, and stays if they never load.
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

function HeroBody({ poster }: { poster?: ReactNode }) {
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
              <span className={styles.words}>Memory that</span>
              <AckChip p50us={ack.p50us} pulse={groups} powered={powered} />
            </span>
          </span>{" "}
          <span className={styles.line}>
            <span className={styles.rise} style={line(1)}>
              hits the disk
            </span>
          </span>
        </h1>

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
            {poster}
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
          Time slowed {numbers.format(slowdownFor(disk))}x. Real p50 on {diskLabel}:{" "}
          <span className={styles.readout}>{formatAck(ack.p50us)}</span>
        </p>
      </div>

      <PlugResult view={view} />

      <p className={styles.lead}>
        AtticDB is an open-source memory database for coding agents. It acks a write only after
        fdatasync. One Rust binary, no model calls.
      </p>

      <div className={styles.actions}>
        <CopyCommand label="Install AtticDB" />
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
    </section>
  );
}

// Its own MotionConfig: motion stays out of the root layout, so pages without motion never load it.
export default function HeroClient({ poster }: { poster?: ReactNode }) {
  return (
    <MotionConfig reducedMotion="user">
      <HeroBody poster={poster} />
    </MotionConfig>
  );
}
