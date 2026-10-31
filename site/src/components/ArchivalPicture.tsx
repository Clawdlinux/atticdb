// The widths scripts/make-images.sh writes for every archival picture. The last one is the
// picture's own width, capped at 1200.
const STEPS = [480, 800];
const CAP = 1200;

export function archivalWidths(width: number): number[] {
  const top = Math.min(width, CAP);
  return [...STEPS.filter((w) => w < top), top];
}

function srcSet(base: string, width: number, ext: "avif" | "jpg"): string {
  return archivalWidths(width)
    .map((w) => `${base}-${w}.${ext} ${w}w`)
    .join(", ");
}

type Props = {
  // The original, e.g. /images/ibm-350-ramac-disk.jpg. Only its resized copies are served.
  src: string;
  width: number;
  height: number;
  alt: string;
  // Must match the rendered width of the picture in its layout.
  sizes: string;
  className?: string;
};

// One archival picture: AVIF first, a JPEG fallback, both sized for the viewport.
export default function ArchivalPicture({ src, width, height, alt, sizes, className }: Props) {
  const base = src.replace(/\.jpg$/, "");
  const widths = archivalWidths(width);
  return (
    <picture>
      <source type="image/avif" srcSet={srcSet(base, width, "avif")} sizes={sizes} />
      <img
        className={className}
        src={`${base}-${widths[widths.length - 1]}.jpg`}
        srcSet={srcSet(base, width, "jpg")}
        sizes={sizes}
        width={width}
        height={height}
        alt={alt}
        loading="lazy"
        decoding="async"
      />
    </picture>
  );
}
