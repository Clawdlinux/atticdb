import { existsSync } from "node:fs";
import { join } from "node:path";
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import ArchivalPicture, { archivalWidths } from "./ArchivalPicture";

// Vitest runs from site/.
const PUBLIC = join(process.cwd(), "public");

afterEach(cleanup);

// The originals the site renders, with their own widths.
const ORIGINALS = [
  ["/images/ibm-350-ramac-disk.jpg", 900],
  ["/images/titan-core-memory-board.jpg", 1280],
  ["/images/ncr-304-tape-reel.jpg", 932],
  ["/images/punched-paper-tape.jpg", 1600],
] as const;

function files(srcset: string | null) {
  return (srcset ?? "").split(", ").map((c) => c.split(" ")[0]);
}

describe("ArchivalPicture", () => {
  it("caps AVIF at 1024 px, which Chrome can draw, and JPEG at 1200 px", () => {
    expect(archivalWidths(1600, "avif")).toEqual([480, 800, 1024]);
    expect(archivalWidths(1600, "jpg")).toEqual([480, 800, 1200]);
    expect(archivalWidths(900, "avif")).toEqual([480, 800, 900]);
  });

  it.each(ORIGINALS)("serves only resized copies of %s that exist", (src, width) => {
    const { container } = render(
      <ArchivalPicture src={src} width={width} height={100} alt="x" sizes="100vw" />,
    );
    const source = container.querySelector("source")!;
    const img = container.querySelector("img")!;
    expect(source.type).toBe("image/avif");
    expect(img.getAttribute("loading")).toBe("lazy");
    expect(img.getAttribute("decoding")).toBe("async");
    const all = [...files(source.getAttribute("srcset")), ...files(img.getAttribute("srcset")), img.getAttribute("src")!];
    expect(all).not.toContain(src);
    for (const file of all) expect(existsSync(PUBLIC + file), file).toBe(true);
  });
});
