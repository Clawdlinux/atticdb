"use client";

import { AnimatePresence, motion, useIsPresent, useReducedMotion } from "motion/react";
import { useEffect, useId, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import CopyCommand from "@/components/CopyCommand";
import Swap from "@/components/Swap";
import {
  APPS,
  APP_LABELS,
  DEFAULT_OPTIONS,
  HOOK_APPS,
  METHOD_LABELS,
  OSES,
  OS_LABELS,
  RELEASES_URL,
  buildInstallSteps,
  detectOs,
  methodsFor,
  normalize,
  type App,
  type InstallOptions,
  type InstallStep,
  type Method,
  type Os,
} from "@/content/install";
import { INSTALL } from "@/lib/motion";
import styles from "./Installer.module.css";

type InstallerProps = { initial?: Partial<InstallOptions> };

type Timing = {
  quick: (typeof INSTALL)["quick"] | (typeof INSTALL)["instant"];
  standard: (typeof INSTALL)["standard"] | (typeof INSTALL)["instant"];
  exit: (typeof INSTALL)["exit"] | (typeof INSTALL)["instant"];
  rise: number;
  stagger: number;
};

const MOVING: Timing = {
  quick: INSTALL.quick,
  standard: INSTALL.standard,
  exit: INSTALL.exit,
  rise: INSTALL.risePx,
  stagger: INSTALL.stagger,
};
// Reduced motion: no travel, and every swap is instant.
const STILL: Timing = {
  quick: INSTALL.instant,
  standard: INSTALL.instant,
  exit: INSTALL.instant,
  rise: 0,
  stagger: 0,
};

const WINDOWS_NOTES: { app: App; text: string }[] = [
  { app: "codex", text: "On Windows, Codex gets only its MCP entry for now." },
  { app: "claude-code", text: "Claude Code needs version 2.1.139 or later for the chat-start hook." },
];

const RESET_MS = 2000;

// The checked state as a shape, not only a fill: a tick that shows only for a checked input.
// In a checkbox it is cut out of a solid square; in a radio it stands alone.
function Tick({ boxed }: { boxed?: boolean }) {
  return (
    <svg
      className={boxed ? styles.mark : styles.radioMark}
      data-ui="tick"
      viewBox="0 0 12 12"
      aria-hidden="true"
      focusable="false"
    >
      {boxed && <rect className={styles.square} width="12" height="12" />}
      <path className={styles.tick} d="M2.5 6.25 5 8.75 9.5 3.5" />
    </svg>
  );
}

// The system never changes under a page, so there is nothing to subscribe to.
const noSubscribe = () => () => {};
const clientOs = () => detectOs(navigator.userAgent, navigator.platform);
// The server, and the hydration pass, render macOS. The detected system follows right after.
const serverOs = () => null;

type ChoiceProps<T extends string> = {
  legend: string;
  name: string;
  values: T[];
  labels: Record<T, string>;
  value: T;
  onPick: (value: T) => void;
  t: Timing;
};

// A radio group of solid tiles. The fill slides to the picked tile (a shared layoutId).
function Choice<T extends string>({ legend, name, values, labels, value, onPick, t }: ChoiceProps<T>) {
  return (
    <fieldset className={styles.group}>
      <legend className={styles.legend}>{legend}</legend>
      <div className={styles.tiles} data-count={values.length}>
        {values.map((v) => (
          <label key={v} className={styles.tile}>
            <input
              className={styles.input}
              type="radio"
              name={name}
              value={v}
              checked={v === value}
              onChange={() => onPick(v)}
            />
            {v === value && (
              <motion.span
                layoutId={`${name}-fill`}
                className={styles.fill}
                transition={t.standard}
                aria-hidden="true"
              />
            )}
            <span className={styles.text}>
              <Tick />
              {labels[v]}
            </span>
          </label>
        ))}
      </div>
    </fieldset>
  );
}

type CheckProps = { label: string; checked: boolean; onToggle: () => void };

// A checkbox as a solid tile. The fill fades in, and the square goes solid with a tick cut in it.
function Check({ label, checked, onToggle }: CheckProps) {
  return (
    <label className={`${styles.tile} ${styles.check}`}>
      <input className={styles.input} type="checkbox" checked={checked} onChange={onToggle} />
      <span className={styles.boxFill} aria-hidden="true" />
      <span className={styles.text}>
        <Tick boxed />
        {label}
      </span>
    </label>
  );
}

// One command. A changed command rises in and the old one fades out in the same cell.
function Line({ command, t }: { command: string; t: Timing }) {
  const present = useIsPresent();
  return (
    <motion.div
      className={styles.line}
      data-ui="command"
      data-present={present}
      aria-hidden={present ? undefined : true}
      initial={{ opacity: 0, y: t.rise }}
      animate={{ opacity: 1, y: 0, transition: t.standard }}
      exit={{ opacity: 0, transition: t.exit }}
    >
      <CopyCommand
        command={command}
        tone="ink"
        wrap="tokens"
        swap={{ transition: t.quick, rise: t.rise }}
      />
    </motion.div>
  );
}

function stepText(step: InstallStep): ReactNode {
  const words = "latest release";
  if (step.id !== "download" || !step.text.includes(words)) return step.text;
  const [before, after] = step.text.split(words);
  return (
    <>
      {before}
      <a href={RELEASES_URL}>{words}</a>
      {after}
    </>
  );
}

export default function Installer({ initial }: InstallerProps) {
  const uid = useId();
  const reduced = useReducedMotion();
  const t = reduced ? STILL : MOVING;

  const detected = useSyncExternalStore(noSubscribe, clientOs, serverOs);
  // What the visitor picked, and whether that pick changed the system or method.
  const [picked, setPicked] = useState<{ choice: Partial<InstallOptions>; cascade: boolean }>({
    choice: {},
    cascade: false,
  });

  const os: Os = picked.choice.os ?? initial?.os ?? detected ?? DEFAULT_OPTIONS.os;
  const options = normalize({ ...DEFAULT_OPTIONS, ...initial, ...picked.choice, os });
  const steps = buildInstallSteps(options);
  const commands = steps.flatMap((s) => s.commands);
  const hookShown = options.apps.some((app) => HOOK_APPS.includes(app));
  const notes =
    options.os === "windows" ? WINDOWS_NOTES.filter((n) => options.apps.includes(n.app)) : [];

  function pick(choice: Partial<InstallOptions>, cascade = false) {
    setPicked((p) => ({ choice: { ...p.choice, ...choice }, cascade }));
  }

  function toggleApp(app: App) {
    const apps = options.apps.includes(app)
      ? options.apps.filter((a) => a !== app)
      : [...options.apps, app];
    pick({ apps });
  }

  const [copy, setCopy] = useState<"idle" | "copied" | "failed">("idle");
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  async function copyAll() {
    window.clearTimeout(timer.current);
    try {
      await navigator.clipboard.writeText(commands.join("\n"));
      setCopy("copied");
    } catch {
      setCopy("failed");
    }
    timer.current = window.setTimeout(() => setCopy("idle"), RESET_MS);
  }

  const copyLabel = copy === "copied" ? "Copied" : copy === "failed" ? "Copy failed" : "Copy all";
  // A new system or method rebuilds the list, which cascades in. Other changes edit it in place.
  const flow = `${options.os}-${options.method}`;

  return (
    <div className={styles.installer}>
      <div className={styles.controls}>
        <Choice<Os>
          legend="Your system"
          name={`${uid}-os`}
          values={OSES}
          labels={OS_LABELS}
          value={options.os}
          onPick={(v) => pick({ os: v }, true)}
          t={t}
        />
        <Choice<Method>
          legend="How to install"
          name={`${uid}-method`}
          values={methodsFor(options.os)}
          labels={METHOD_LABELS}
          value={options.method}
          onPick={(v) => pick({ method: v }, true)}
          t={t}
        />
        <fieldset className={styles.group}>
          <legend className={styles.legend}>Apps to connect</legend>
          <div className={styles.tiles} data-count={2}>
            {APPS.map((app) => (
              <Check
                key={app}
                label={APP_LABELS[app]}
                checked={options.apps.includes(app)}
                onToggle={() => toggleApp(app)}
              />
            ))}
          </div>
        </fieldset>
        <fieldset className={styles.group}>
          <legend className={styles.legend}>Options</legend>
          <div className={styles.tiles} data-count={1}>
            <Check
              label="Preview first"
              checked={options.preview}
              onToggle={() => pick({ preview: !options.preview })}
            />
            {hookShown && (
              <Check
                label="Load memory when a chat starts"
                checked={options.hook}
                onToggle={() => pick({ hook: !options.hook })}
              />
            )}
          </div>
        </fieldset>
      </div>

      <div className={styles.panel} data-ui="steps">
        <ol key={flow} className={styles.steps}>
          <AnimatePresence initial={picked.cascade}>
            {steps.map((step, i) => (
              <motion.li
                key={step.id}
                className={styles.step}
                layout="position"
                initial={{ opacity: 0, y: t.rise }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, transition: t.exit }}
                transition={{
                  ...t.standard,
                  // 40 ms a step on a rebuild, capped so the whole cascade stays under 200 ms.
                  delay: picked.cascade ? Math.min(i * t.stagger, INSTALL.staggerMax) : 0,
                  layout: t.standard,
                }}
              >
                <p className={styles.say}>{stepText(step)}</p>
                {step.commands.length > 0 && (
                  <div className={styles.commands}>
                    {step.commands.map((command, j) => (
                      <div key={j} className={styles.cell}>
                        <AnimatePresence initial={false}>
                          <Line key={command} command={command} t={t} />
                        </AnimatePresence>
                      </div>
                    ))}
                  </div>
                )}
              </motion.li>
            ))}
          </AnimatePresence>
        </ol>

        <div className={styles.foot}>
          <button type="button" className={`button button--accent ${styles.copyAll}`} onClick={copyAll}>
            <span className={styles.labels}>
              {["Copy all", "Copied", "Copy failed"].map((text) => (
                <span key={text} className={styles.sizer} aria-hidden="true">
                  {text}
                </span>
              ))}
              <Swap value={copyLabel} className={styles.shown} mode="sync" transition={t.quick} rise={t.rise} />
            </span>
          </button>
          <span className="visually-hidden" role="status">
            {copy === "copied"
              ? "Every command copied, one per line."
              : copy === "failed"
                ? "Copy failed. Use each command's own button."
                : ""}
          </span>
          {notes.length > 0 && (
            <ul className={styles.notes}>
              {notes.map((n) => (
                <li key={n.app}>{n.text}</li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
}
