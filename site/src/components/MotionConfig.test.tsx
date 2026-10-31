import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

// Records the reducedMotion setting of every MotionConfig that renders.
const seen = vi.hoisted(() => [] as (string | undefined)[]);

vi.mock("motion/react", async (importOriginal) => {
  const real = await importOriginal<typeof import("motion/react")>();
  function MotionConfig(props: Parameters<typeof real.MotionConfig>[0]) {
    seen.push(props.reducedMotion);
    return real.MotionConfig(props);
  }
  return { ...real, MotionConfig };
});

const { default: CopyCommand } = await import("./CopyCommand");
const { default: Installer } = await import("./Installer");
const { default: Hero } = await import("./home/Hero");
const { default: Showcase } = await import("./home/Showcase");

afterEach(() => {
  cleanup();
  seen.length = 0;
});

// The root layout carries no MotionConfig, so each motion tree must bring its own.
describe("motion trees honour reduced motion on their own", () => {
  it.each([
    ["Hero", () => <Hero />],
    ["Showcase", () => <Showcase />],
    ["Installer", () => <Installer />],
    ["CopyCommand", () => <CopyCommand />],
  ])("%s sets reducedMotion to user at its root", (_, view) => {
    render(view());
    expect(seen[0]).toBe("user");
  });
});
