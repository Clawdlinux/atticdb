import "@testing-library/jest-dom/vitest";

// jsdom has no 2D context and logs a warning when asked. The hero's canvas then keeps its poster.
if (typeof HTMLCanvasElement !== "undefined") {
  HTMLCanvasElement.prototype.getContext = (() => null) as typeof HTMLCanvasElement.prototype.getContext;
}
