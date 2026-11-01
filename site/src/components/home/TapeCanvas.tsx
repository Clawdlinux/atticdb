"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { NARROW_BELOW, geometry, scene, type Prim, type Tone } from "./tape-scene";
import {
  CUT_AT,
  RESTART_MS,
  cutTape,
  initTape,
  restartTape,
  retuneTape,
  runToCut,
  stepTape,
  stillFrame,
  syncProgress,
  tapeParams,
  type TapeParams,
  type TapeState,
} from "./tape-sim";
import type { Disk, PlugView, Writers } from "./tape-time";

/*
 * Loaded after hydration (next/dynamic, no SSR), so none of this sits in the first paint's path.
 * Until it loads, the server-drawn poster is the picture.
 */

// Lost frames are drawn this far into their drop when nothing animates.
const STILL_CUT_MS = 300;

function prefersStill(): boolean {
  return typeof window !== "undefined" && !!window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
}

type Tape = {
  params: TapeParams;
  stateRef: React.RefObject<TapeState>;
  view: PlugView;
  // Changes once per group commit, so the ack chip's light pulses with the tape.
  groups: number;
  pull: () => void;
  advance: (dtMs: number) => void;
  // The canvas says whether its loop is running, and how to redraw when it is not.
  attach: (running: boolean, redraw: (() => void) | null) => void;
  still: boolean;
};

// The tape's state and the plug demo around it. React only hears about group commits and the plug.
function useTape(disk: Disk, writers: Writers): Tape {
  const params = useMemo(() => tapeParams(disk, writers), [disk, writers]);
  const paramsRef = useRef(params);
  const [initial] = useState(() => stillFrame(params));
  const stateRef = useRef<TapeState>(initial);
  const [view, setView] = useState<PlugView>({ phase: "idle", outcome: null });
  const [groups, setGroups] = useState(0);
  const armed = useRef(false);
  const running = useRef(false);
  const redraw = useRef<(() => void) | null>(null);
  const timer = useRef<number | null>(null);
  const [still] = useState(prefersStill);

  useEffect(() => () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
  }, []);

  const restart = useCallback(() => {
    timer.current = null;
    const p = paramsRef.current;
    const s = restartTape(stateRef.current, p);
    // With nothing animating, the restarted store rests on a finished group again.
    stateRef.current = running.current ? s : { ...stillFrame(p), written: 0, acked: 0 };
    setView((v) => ({ phase: "restarted", outcome: v.outcome }));
    redraw.current?.();
  }, []);

  const onCut = useCallback(
    (s: TapeState) => {
      armed.current = false;
      stateRef.current = running.current ? s : stepTape(s, STILL_CUT_MS, paramsRef.current);
      setView({ phase: "cut", outcome: s.lastRun });
      redraw.current?.();
      timer.current = window.setTimeout(restart, RESTART_MS);
    },
    [restart],
  );

  const advance = useCallback(
    (dt: number) => {
      const p = paramsRef.current;
      const before = stateRef.current;
      let s = stepTape(before, dt, p);
      if (armed.current && syncProgress(s) >= CUT_AT) {
        s = cutTape(s);
        stateRef.current = s;
        onCut(s);
        return;
      }
      stateRef.current = s;
      if (s.groupsDone !== before.groupsDone) setGroups((g) => g + 1);
    },
    [onCut],
  );

  const pull = useCallback(() => {
    if (armed.current || stateRef.current.cut || timer.current !== null) return;
    if (running.current) {
      // Wait for the next group to be part way through its sync, so the cut lands mid-group.
      armed.current = true;
      setView((v) => ({ ...v, phase: "armed" }));
      return;
    }
    const p = paramsRef.current;
    const from = stateRef.current.groupsDone > 0 ? stateRef.current : initTape(p);
    const s = runToCut(from, p);
    stateRef.current = s;
    onCut(s);
  }, [onCut]);

  useEffect(() => {
    if (paramsRef.current === params) return;
    paramsRef.current = params;
    const s = stateRef.current;
    if (!running.current && !s.cut) stateRef.current = stillFrame(params);
    else stateRef.current = retuneTape(s, params);
    redraw.current?.();
  }, [params]);

  const attach = useCallback((isRunning: boolean, draw: (() => void) | null) => {
    running.current = isRunning;
    redraw.current = draw;
  }, []);

  return { params, stateRef, view, groups, pull, advance, attach, still };
}

type Palette = Record<Tone, string>;

function palette(): Palette {
  const css = getComputedStyle(document.documentElement);
  const read = (name: string) => css.getPropertyValue(name).trim();
  return { ink: read("--ink"), muted: read("--muted"), accent: read("--accent"), bg: read("--bg") };
}

function readMs(name: string, fallback: number): number {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  const n = parseFloat(v);
  if (!Number.isFinite(n)) return fallback;
  return v.endsWith("ms") ? n : v.endsWith("s") ? n * 1000 : n;
}

function paint(ctx: CanvasRenderingContext2D, prims: Prim[], colors: Palette, family: string) {
  const digitW = new Map<number, number>();
  for (const p of prims) {
    ctx.globalAlpha = Math.max(0, Math.min(1, p.a));
    ctx.fillStyle = colors[p.c];
    if (p.k === "rect") {
      ctx.fillRect(p.x, p.y, p.w, p.h);
    } else if (p.k === "dot") {
      ctx.beginPath();
      ctx.arc(p.x, p.y, p.r, 0, Math.PI * 2);
      ctx.fill();
    } else {
      ctx.font = `${p.size}px ${family}`;
      ctx.textBaseline = "middle";
      if (!p.num) {
        ctx.textAlign = p.align;
        ctx.fillText(p.s, p.x, p.y);
        continue;
      }
      // Tabular figures: every digit sits in a cell as wide as the widest digit.
      let cell = digitW.get(p.size);
      if (cell === undefined) {
        cell = Math.max(...Array.from("0123456789", (d) => ctx.measureText(d).width));
        digitW.set(p.size, cell);
      }
      // Letters keep their own width; only digits sit in fixed cells.
      const widths = Array.from(p.s, (ch) => (ch >= "0" && ch <= "9" ? cell! : ctx.measureText(ch).width));
      const total = widths.reduce((a, b) => a + b, 0);
      let x = p.align === "center" ? p.x - total / 2 : p.align === "right" ? p.x - total : p.x;
      ctx.textAlign = "center";
      Array.from(p.s).forEach((ch, i) => {
        ctx.fillText(ch, x + widths[i] / 2, p.y);
        x += widths[i];
      });
    }
  }
  ctx.globalAlpha = 1;
}

export type TapeHandle = { pull: () => void };

export type TapeCanvasProps = {
  disk: Disk;
  writers: Writers;
  label: string;
  // The canvas has drawn its first frame: the poster can go.
  onLive: () => void;
  onView: (view: PlugView) => void;
  // Once per group commit, so the ack chip's light pulses with the tape.
  onGroup: () => void;
  handleRef: React.RefObject<TapeHandle | null>;
  // A pull asked for before this module loaded.
  pendingRef: React.RefObject<boolean>;
};

export default function TapeCanvas(props: TapeCanvasProps) {
  const { disk, writers, label, onLive, onView, onGroup, handleRef, pendingRef } = props;
  const ref = useRef<HTMLCanvasElement>(null);
  const tape = useTape(disk, writers);
  const { stateRef, attach, advance, still } = tape;
  const kick = useRef<(() => void) | null>(null);
  const onLiveRef = useRef(onLive);
  useEffect(() => {
    onLiveRef.current = onLive;
  }, [onLive]);

  useEffect(() => {
    const canvas = ref.current;
    // jsdom and very old browsers have no 2D context: the poster stays.
    let ctx: CanvasRenderingContext2D | null = null;
    try {
      ctx = canvas?.getContext("2d") ?? null;
    } catch {
      ctx = null;
    }
    if (!canvas || !ctx) return;
    const c2d = ctx;
    let colors = palette();
    const family = getComputedStyle(canvas).fontFamily || "serif";
    let w = 0;
    let h = 0;
    let dpr = 1;
    let raf = 0;
    let last = 0;
    let started = false;
    let visible = true;
    let disposed = false;
    let startTimer = 0;

    const draw = () => {
      if (!w || !h) return;
      c2d.setTransform(dpr, 0, 0, dpr, 0, 0);
      c2d.clearRect(0, 0, w, h);
      // Lanes, not the writers prop: a settings change redraws before the prop's effect runs.
      const lanes = stateRef.current.lanes.length || 1;
      const G = geometry(w < NARROW_BELOW ? "narrow" : "wide", w, lanes);
      paint(c2d, scene(stateRef.current, lanes, G, { still }), colors, family);
    };

    const size = () => {
      const r = canvas.getBoundingClientRect();
      dpr = Math.min(2, window.devicePixelRatio || 1);
      w = Math.round(r.width);
      h = Math.round(r.height);
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      draw();
    };

    const frame = (now: number) => {
      raf = 0;
      if (disposed) return;
      const dt = last ? Math.min(50, now - last) : 16;
      last = now;
      advance(dt);
      draw();
      loop();
    };

    const loop = () => {
      const go = started && visible && !document.hidden && !still;
      attach(go, go ? null : draw);
      if (go && !raf) raf = requestAnimationFrame(frame);
      if (!go) {
        if (raf) cancelAnimationFrame(raf);
        raf = 0;
        last = 0;
      }
    };

    const start = () => {
      if (started || disposed) return;
      started = true;
      size();
      onLiveRef.current();
      loop();
    };
    kick.current = start;

    const ro = typeof ResizeObserver === "function" ? new ResizeObserver(size) : null;
    ro?.observe(canvas);
    const io =
      typeof IntersectionObserver === "function"
        ? new IntersectionObserver((entries) => {
            visible = entries.some((e) => e.isIntersecting);
            loop();
          })
        : null;
    io?.observe(canvas);
    const onVis = () => loop();
    document.addEventListener("visibilitychange", onVis);
    const onShow = () => {
      colors = palette();
      draw();
    };
    window.addEventListener("pageshow", onShow);
    attach(false, draw);

    // Start after the headline has risen, so the canvas never competes with it for the first paint.
    const reveal = readMs("--t-rise", 900) + 200;
    const fonts = document.fonts?.ready ?? Promise.resolve();
    fonts.then(() => {
      if (disposed) return;
      size();
      if (still) {
        start();
        return;
      }
      startTimer = window.setTimeout(start, Math.max(0, reveal - performance.now()));
    });

    return () => {
      disposed = true;
      kick.current = null;
      window.clearTimeout(startTimer);
      if (raf) cancelAnimationFrame(raf);
      ro?.disconnect();
      io?.disconnect();
      document.removeEventListener("visibilitychange", onVis);
      window.removeEventListener("pageshow", onShow);
      attach(false, null);
    };
  }, [advance, attach, stateRef, still]);

  // A change of settings before the start: start now, so the picture answers the click.
  const firstParams = useRef(tape.params);
  useEffect(() => {
    if (tape.params !== firstParams.current) kick.current?.();
  }, [tape.params]);

  // Hand the plug to the hero, and run a pull that came in while this module loaded.
  const { pull, view, groups } = tape;
  useEffect(() => {
    handleRef.current = { pull };
    if (pendingRef.current) {
      pendingRef.current = false;
      pull();
    }
    return () => {
      handleRef.current = null;
    };
  }, [handleRef, pendingRef, pull]);

  useEffect(() => {
    onView(view);
  }, [onView, view]);

  const firstGroups = useRef(groups);
  useEffect(() => {
    if (groups !== firstGroups.current) onGroup();
  }, [groups, onGroup]);

  return <canvas ref={ref} role="img" aria-label={label} />;
}
