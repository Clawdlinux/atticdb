// The widths scripts/make-images.sh writes for every archival picture: 480, 800 and the
// picture's own width, capped at 1200 for JPEG and 1024 for AVIF.
// Chrome draws an AVIF from sips wider than 1024 px blank (checked 2026-10-31).
const STEPS = [480, 800];
const CAP = { jpg: 1200, avif: 1024 } as const;

type Ext = keyof typeof CAP;

export function archivalWidths(width: number, ext: Ext): number[] {
  const top = Math.min(width, CAP[ext]);
  return [...STEPS.filter((w) => w < top), top];
}

function srcSet(base: string, width: number, ext: Ext): string {
  return archivalWidths(width, ext)
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
  const widths = archivalWidths(width, "jpg");
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
