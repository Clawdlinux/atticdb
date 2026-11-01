import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { POSTER_PAD, POSTER_VIEWS } from "../src/components/home/tape-poster.ts";
import { POSTER_PAD as GEN_PAD, posterParts, posterSvg, tokens } from "./build-tape-poster.mjs";

// Vitest runs from site/.
const css = readFileSync("src/app/globals.css", "utf8");

describe("tape poster", () => {
  it("matches the views the hero places", () => {
    expect(GEN_PAD).toBe(POSTER_PAD);
    const { parts } = posterParts();
    expect(
      parts.map((q) => ({
        id: q.id, mode: q.mode, layer: q.layer, w: Math.round(q.w), h: q.h, y: q.y, tx: Math.round(q.G.tx - GEN_PAD),
      })),
    ).toEqual(POSTER_VIEWS);
  });

  it("is one SVG with four named groups, coloured from the tokens", () => {
    const svg = posterSvg(css);
    for (const v of POSTER_VIEWS) expect(svg).toContain(`<g id="${v.id}"`);
    const t = tokens(css);
    expect(svg).toContain(t.ink);
    expect(svg).toContain(t.accent);
    expect(svg).not.toContain("var(");
    expect(svg).toContain("Claude Code");
    expect(svg.length).toBeLessThan(20000);
  });
});
