#!/bin/sh
# Regenerates the responsive copies of the archival pictures with macOS sips.
# The originals in public/images/<name>.jpg stay the source of truth. Never edit the copies.
# Each original gets <name>-<w>.avif and <name>-<w>.jpg at 480, 800 and its own width (cap 1200).
# Run from site/: sh scripts/make-images.sh
set -eu

AVIF_Q=55
JPEG_Q=55
DIR=public/images

for src in "$DIR"/*.jpg; do
  if echo "$src" | grep -Eq -- '-[0-9]+\.jpg$'; then continue; fi
  name=$(basename "$src" .jpg)
  full=$(sips -g pixelWidth "$src" | awk '/pixelWidth/ { print $2 }')
  top=$full
  [ "$top" -gt 1200 ] && top=1200
  for w in 480 800 "$top"; do
    [ "$w" -gt "$full" ] && continue
    sips --resampleWidth "$w" -s format avif -s formatOptions "$AVIF_Q" "$src" --out "$DIR/$name-$w.avif" >/dev/null
    sips --resampleWidth "$w" -s format jpeg -s formatOptions "$JPEG_Q" "$src" --out "$DIR/$name-$w.jpg" >/dev/null
  done
done

ls -l "$DIR"
