// Draws the tape's still frame into public/tape-poster.svg, before every build.
// One file, four named groups (wide and narrow, writers part and tape part), stacked. The hero
// shows each through an inline <svg viewBox><use href="/tape-poster.svg#id"></svg>, so the page
// fetches it once, its HTML stays small, the text takes the page's own font, and the poster is
// never a Largest Contentful Paint candidate (an <img> of it was). Colours are the tokens' hex values.
// Run: node --import ./scripts/ts-hooks.mjs scripts/build-tape-poster.mjs
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { geometry, scene } from "../src/components/home/tape-scene.ts";
import { stillFrame, tapeParams } from "../src/components/home/tape-sim.ts";

const ROOT = new URL("../", import.meta.url);

// The writers part reaches this far left of the gutter, so tracks run to the band's edge.
export const POSTER_PAD = 400;
// The tape part: wide enough for a 2560 px screen, and for the stacked band under 768 px.
export const POSTER_SPAN = { wide: 1800, narrow: 800 };

export function tokens(css) {
  const out = {};
  for (const name of ["bg", "ink", "muted", "accent"]) {
    const m = css.match(new RegExp(`--${name}:\\s*(#[0-9a-fA-F]{3,8})`));
    if (!m) throw new Error(`globals.css has no hex value for --${name}`);
    out[name] = m[1];
  }
  return out;
}

const r1 = (n) => Math.round(n * 10) / 10;
const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;");

function shapes(prims, layer, colors) {
  return prims
    .filter((p) => p.layer === layer)
    .map((p) => {
      const fill = `fill="${colors[p.c]}"${p.a < 1 ? ` fill-opacity="${p.a.toFixed(2)}"` : ""}`;
      if (p.k === "rect") return `<rect x="${r1(p.x)}" y="${r1(p.y)}" width="${r1(p.w)}" height="${r1(p.h)}" ${fill}/>`;
      if (p.k === "dot") return `<circle cx="${r1(p.x)}" cy="${r1(p.y)}" r="${p.r}" ${fill}/>`;
      const anchor = p.align === "center" ? "middle" : p.align === "right" ? "end" : "start";
      const num = p.num ? ` style="font-variant-numeric:tabular-nums lining-nums"` : "";
      return `<text x="${r1(p.x)}" y="${r1(p.y)}" font-size="${p.size}" text-anchor="${anchor}" dominant-baseline="central"${num} ${fill}>${esc(p.s)}</text>`;
    })
    .join("");
}

// The layout of each view, also used by the hero to size and place the images.
export function posterParts() {
  const p = tapeParams("nvme", 1);
  const parts = [];
  let y = 0;
  for (const mode of ["wide", "narrow"]) {
    const tx0 = geometry(mode, 10000, p.writers, POSTER_PAD).tx;
    const span = POSTER_SPAN[mode];
    const G = geometry(mode, tx0 + span, p.writers, POSTER_PAD);
    // Wide: the writers part ends where the tape begins. Narrow: the tape runs underneath it.
    const leftW = Math.max(G.tx, G.qx + G.colW + 40);
    parts.push({ id: `${mode}-left`, mode, layer: "left", G, x: 0, w: leftW, h: G.h, y });
    y += G.h;
    parts.push({ id: `${mode}-tape`, mode, layer: "tape", G, x: G.tx, w: span, h: G.h, y });
    y += G.h;
  }
  return { parts, height: y, width: Math.max(...parts.map((q) => q.w)), params: p };
}

export function posterSvg(css) {
  const colors = tokens(css);
  const { parts, height, width, params } = posterParts();
  const still = stillFrame(params);
  const body = parts
    .map((q) => {
      // Segment labels stick where the tape starts on a 1440 px (wide) or 390 px (narrow) screen.
      const shown = q.mode === "wide" ? 1440 - 80 - (q.G.tx - POSTER_PAD) : 390 - 16;
      const prims = scene(still, params.writers, q.G, { still: true, stickyX: q.G.w - shown });
      return `<g id="${q.id}" transform="translate(${r1(-q.x)} ${q.y})">${shapes(prims, q.layer, colors)}</g>`;
    })
    .join("");
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="${r1(width)}" height="${height}" viewBox="0 0 ${r1(width)} ${height}">` +
    body +
    `</svg>\n`
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const css = readFileSync(new URL("src/app/globals.css", ROOT), "utf8");
  const svg = posterSvg(css);
  writeFileSync(new URL("public/tape-poster.svg", ROOT), svg);
  const { parts } = posterParts();
  console.log(`tape-poster.svg: ${svg.length} bytes; views ${parts.map((q) => `${q.id} ${r1(q.w)}x${q.h}`).join(", ")}`);
}
