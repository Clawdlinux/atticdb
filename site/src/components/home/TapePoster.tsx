import { geometry, scene, type Layer, type Mode, type Prim } from "./tape-scene";
import { stillFrame, tapeParams } from "./tape-sim";
import styles from "./Hero.module.css";

// Wide enough for a 2560 px screen (and under 768 px for the stacked band).
// The tape part is right-aligned and clipped by its box.
const TAPE_SPAN: Record<Mode, number> = { wide: 1800, narrow: 800 };

// One decimal is finer than a CSS pixel and keeps the inline SVG small.
const r1 = (n: number) => Math.round(n * 10) / 10;

function Shapes({ prims, layer }: { prims: Prim[]; layer: Layer }) {
  return prims
    .filter((p) => p.layer === layer)
    .map((p, i) => {
      const fill = `var(--${p.c})`;
      const opacity = p.a < 1 ? Number(p.a.toFixed(2)) : undefined;
      if (p.k === "rect") {
        return <rect key={i} x={r1(p.x)} y={r1(p.y)} width={r1(p.w)} height={r1(p.h)} fill={fill} fillOpacity={opacity} />;
      }
      if (p.k === "dot") {
        return <circle key={i} cx={r1(p.x)} cy={r1(p.y)} r={p.r} fill={fill} fillOpacity={opacity} />;
      }
      const anchor = p.align === "center" ? "middle" : p.align === "right" ? "end" : "start";
      return (
        <text
          key={i}
          x={r1(p.x)}
          y={r1(p.y)}
          fill={fill}
          fillOpacity={opacity}
          fontSize={p.size}
          textAnchor={anchor}
          dominantBaseline="central"
        >
          {p.s}
        </text>
      );
    });
}

function Poster({ mode }: { mode: Mode }) {
  const p = tapeParams("nvme", 1);
  const s = stillFrame(p);
  const tx = geometry(mode, 10000, p.writers, 0).tx;
  const span = TAPE_SPAN[mode];
  const G = geometry(mode, tx + span, p.writers, 0);
  const prims = scene(s, p.writers, G, { still: true });
  const leftW = G.tx + 60;
  return (
    <div className={styles.poster} data-mode={mode}>
      <svg
        className={styles.posterLeft}
        width={leftW}
        height={G.h}
        viewBox={`0 0 ${leftW} ${G.h}`}
        focusable="false"
      >
        <Shapes prims={prims} layer="left" />
      </svg>
      <div className={styles.posterTape} style={{ "--tx": `${G.tx}px` } as React.CSSProperties}>
        <svg
          width={span}
          height={G.h}
          viewBox={`${G.tx} 0 ${span} ${G.h}`}
          focusable="false"
        >
          <Shapes prims={prims} layer="tape" />
        </svg>
      </div>
    </div>
  );
}

// The tape's still frame as inline SVG, drawn at build time. No JS needed, so the band is never empty.
export default function TapePoster() {
  return (
    <>
      <Poster mode="wide" />
      <Poster mode="narrow" />
    </>
  );
}
