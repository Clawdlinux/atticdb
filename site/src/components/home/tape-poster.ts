// The still-frame poster's parts in public/tape-poster.svg (written by scripts/build-tape-poster.mjs).
// Sizes are checked against the generator in its test.
export const POSTER_SRC = "/tape-poster.svg";
// The writers part starts this far left of the gutter, so its tracks reach the band's edge.
export const POSTER_PAD = 400;

// y: where the group sits in the stacked file, for the viewBox that shows it.
export type PosterView = {
  id: string; mode: "wide" | "narrow"; layer: "left" | "tape"; w: number; h: number; y: number; tx: number;
};

export const POSTER_VIEWS: PosterView[] = [
  { id: "wide-left", mode: "wide", layer: "left", w: 946, h: 240, y: 0, tx: 546 },
  { id: "wide-tape", mode: "wide", layer: "tape", w: 1800, h: 240, y: 240, tx: 546 },
  { id: "narrow-left", mode: "narrow", layer: "left", w: 756, h: 262, y: 480, tx: 0 },
  { id: "narrow-tape", mode: "narrow", layer: "tape", w: 800, h: 262, y: 742, tx: 0 },
];
