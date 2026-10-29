#!/usr/bin/env bash
# Builds labeled before/after comparisons from design/before and design/after.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out="$here/compare"
mkdir -p "$out"
font=$(fc-match -f '%{file}' 'Adwaita Sans:semibold' 2>/dev/null || true)
label() { magick "$1" -gravity north -background '#eef0f5' -splice 0x44 -font "${font:-DejaVu-Sans}" -pointsize 22 -fill '#1d2030' -annotate +0+10 "$2" "$3"; }
pair() {
  local name=$1 before=$2 after=$3 crop=${4:-}
  local b="$out/.b.png" a="$out/.a.png"
  if [ -n "$crop" ]; then magick "$before" -crop "$crop" +repage "$b"; magick "$after" -crop "$crop" +repage "$a"; else cp "$before" "$b"; cp "$after" "$a"; fi
  label "$b" "Before" "$b"; label "$a" "After" "$a"
  magick "$b" "$a" -background '#c9ced6' -splice 0x0 +append -resize '2200x>' "$out/$name.png"
  rm -f "$b" "$a"
}
pair 1-receipts "$here/before/channel-light.png" "$here/after/channel-light.png" 1192x640+248+260
pair 2-needs-you "$here/before/needs-you-unanswered.png" "$here/after/unanswered-light.png" 1192x340+248+0
pair 3-hierarchy "$here/before/channel-light.png" "$here/after/channel-light.png"
pair 4-sidebar "$here/before/channel-light.png" "$here/after/channel-light.png" 260x480+0+0
pair 5-thread "$here/before/thread-light.png" "$here/after/thread-light.png"
pair 6-dark "$here/before/channel-dark.png" "$here/after/channel-dark.png"
ls -la "$out"
