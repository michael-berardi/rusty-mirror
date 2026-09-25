#!/bin/sh
# Build Tally's frontend into dist/. There is no bundler, so the mirror gate is
# a pair of comment fences: RUSTY_MIRROR=1 keeps them, anything else strips them.
# (Bundled apps do the same with import.meta.env; see docs/integrate.md.)
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${1:-"$here/dist"}
rm -rf "$out"
mkdir -p "$out"
for file in index.html style.css app.js; do
  if [ "${RUSTY_MIRROR:-0}" = 1 ]; then
    cp "$here/src/$file" "$out/$file"
  else
    sed '/rusty-mirror:begin/,/rusty-mirror:end/d' "$here/src/$file" > "$out/$file"
  fi
done
echo "built $out (mirror gate: ${RUSTY_MIRROR:-0})"
