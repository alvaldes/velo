#!/bin/sh
# Re-record the README's screenshots from a real velo in a real terminal.
#
#     docs/media/render.sh
#
# Needs `vhs` and `ffmpeg`. vhs 0.12 records a GIF and cannot write a still
# frame, so each tape is recorded to GIF and only its last frame is kept as the
# PNG the README links to.
#
# Two things are pinned so the images do not describe this machine:
#   - the binary built from this checkout, when there is one, over the one on
#     PATH, so a screenshot never shows a version that has not shipped;
#   - a scratch config with the cover art pane off, because the recording
#     terminal speaks no image protocol and velo would print that it hid it.
set -eu
cd "$(dirname "$0")"

root=$(cd ../.. && pwd)
for kind in release debug; do
  if [ -x "$root/target/$kind/velo" ]; then
    PATH="$root/target/$kind:$PATH"
    break
  fi
done
command -v velo >/dev/null || {
  echo "render.sh: no velo binary — build one, or put the released one on PATH" >&2
  exit 1
}
export PATH

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/velo"
printf '[ui]\ncover_art = false\n' >"$scratch/velo/config.toml"
XDG_CONFIG_HOME=$scratch
export XDG_CONFIG_HOME

for name in shot-library shot-visualizer shot-radio; do
  echo "recording $name"
  rm -f "$name.gif"
  vhs "$name.tape"
  ffmpeg -y -loglevel error -sseof -0.2 -i "$name.gif" -frames:v 1 "$name.png"
  rm -f "$name.gif"
  printf '%s.png\n' "$name"
done
