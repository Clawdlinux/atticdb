"use client";

import { useState, type CSSProperties } from "react";
import CopyCommand from "@/components/CopyCommand";
import { ackFor } from "@/content/bench";
import { WAITLIST_URL } from "@/lib/site";
import AckChip, { formatAck } from "./AckChip";
import { PlugResult, PowerChip, usePlugDemo } from "./PlugDemo";
import styles from "./Hero.module.css";

type Disk = "nvme" | "premium-v2";
type Writers = 1 | 4 | 16;

const DISKS: { value: Disk; label: string }[] = [
  { value: "nvme", label: "Local NVMe" },
  { value: "premium-v2", label: "Premium SSD v2" },
];
const WRITERS: Writers[] = [1, 4, 16];

const numbers = new Intl.NumberFormat("en-US");

function line(i: number): CSSProperties {
  return { "--i": i } as CSSProperties;
}

export default function Hero() {
  const [disk, setDisk] = useState<Disk>("nvme");
  const [writers, setWriters] = useState<Writers>(1);
  const { state, run, running } = usePlugDemo();
  const ack = ackFor(disk, writers);
  const diskLabel = DISKS.find((d) => d.value === disk)?.label;

  return (
    <section className={styles.hero} aria-labelledby="hero-title">
      <div className={styles.stage}>
        <h1 id="hero-title" className={styles.title}>
          <span className={styles.line}>
            <span className={styles.rise} style={line(0)}>
              <span className={styles.words}>Memory that</span>
              <AckChip p50us={ack.p50us} pulse={state.sent} powered={state.phase !== "cut"} />
            </span>
          </span>{" "}
          <span className={styles.line}>
            <span className={styles.rise} style={line(1)}>
              hits the disk
            </span>
          </span>
        </h1>

        <fieldset className={`${styles.chip} ${styles.chipDisk}`} data-ui="disk">
          <legend className={styles.chipLabel}>Disk</legend>
          <span className={styles.options}>
            {DISKS.map((d) => (
              <label key={d.value} className={styles.option}>
                <input
                  type="radio"
                  name="hero-disk"
                  value={d.value}
                  checked={disk === d.value}
                  onChange={() => setDisk(d.value)}
                />
                <span>{d.label}</span>
              </label>
            ))}
          </span>
        </fieldset>

        <div className={styles.pair}>
          <fieldset className={`${styles.chip} ${styles.chipWriters}`} data-ui="writers">
            <legend className={styles.chipLabel}>Writers</legend>
            <span className={styles.readout} aria-hidden="true">
              {numbers.format(ack.acksPerSec)} acks/s
            </span>
            <span className={styles.options}>
              {WRITERS.map((w) => (
                <label key={w} className={styles.option}>
                  <input
                    type="radio"
                    name="hero-writers"
                    value={w}
                    checked={writers === w}
                    onChange={() => setWriters(w)}
                  />
                  <span>{w}</span>
                </label>
              ))}
            </span>
          </fieldset>

          <PowerChip state={state} running={running} onRun={run} className={styles.chipPower} />
        </div>

        <p className="visually-hidden" role="status">
          {`${diskLabel}, ${writers} ${writers === 1 ? "writer" : "writers"}: AtticDB acked in ${formatAck(ack.p50us)} at p50, ${numbers.format(ack.acksPerSec)} acks/s.`}
        </p>
      </div>

      <p className={styles.lead} data-load>
        AtticDB is an open-source memory database for coding agents. It acks a write only after
        fdatasync. One Rust binary, no model calls.
      </p>

      <div className={styles.actions} data-load>
        <CopyCommand id="install" label="Install AtticDB" />
        <a className={styles.waitlist} href={WAITLIST_URL}>
          Join the cloud waitlist
        </a>
      </div>

      <PlugResult state={state} />
    </section>
  );
}
