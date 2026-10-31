#!/bin/sh
# Regenerates the responsive copies of the archival pictures with macOS sips.
# The originals in public/images/<name>.jpg stay the source of truth. Never edit the copies.
# Each original gets <name>-<w>.jpg at 480, 800 and its own width (cap 1200),
# and <name>-<w>.avif at 480, 800 and its own width (cap 1024).
# Chrome draws an AVIF from sips wider than 1024 px blank (checked 2026-10-31). Keep that cap.
# Widths here must match src/components/ArchivalPicture.tsx.
# Run from site/: sh scripts/make-images.sh
set -eu

AVIF_Q=55
JPEG_Q=55
DIR=public/images

# widths <full> <cap>: 480, 800 and min(full, cap), skipping any wider than the original.
widths() {
  top=$1
  [ "$top" -gt "$2" ] && top=$2
  for w in 480 800; do [ "$w" -lt "$top" ] && echo "$w"; done
  echo "$top"
}

for src in "$DIR"/*.jpg; do
  if echo "$src" | grep -Eq -- '-[0-9]+\.jpg$'; then continue; fi
  name=$(basename "$src" .jpg)
  full=$(sips -g pixelWidth "$src" | awk '/pixelWidth/ { print $2 }')
  for w in $(widths "$full" 1200); do
    sips --resampleWidth "$w" -s format jpeg -s formatOptions "$JPEG_Q" "$src" --out "$DIR/$name-$w.jpg" >/dev/null
  done
  for w in $(widths "$full" 1024); do
    sips --resampleWidth "$w" -s format avif -s formatOptions "$AVIF_Q" "$src" --out "$DIR/$name-$w.avif" >/dev/null
  done
done

ls -l "$DIR"
