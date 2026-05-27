#!/usr/bin/env bash
# Build the dx-playground cockpit (wasm) and stage it into the dioxus-mcp crate's
# `ui-dist/` so `include_dir!` bakes the real UI into the binary at compile time.
#
# Why this exists: `ui-dist/index.html` is committed as a tiny PLACEHOLDER (so a
# fresh clone compiles — `include_dir!` needs the file to exist) and
# `ui-dist/assets/` is gitignored (hashed wasm would bloat the repo). This script
# replaces the placeholder with the real release bundle. After running it,
# `cargo build -p dioxus-mcp` embeds the real cockpit, so the bare-registered
# stdio MCP serves it at :8731 with nothing extra to run.
#
# The working tree's `ui-dist/index.html` is left "modified" (the real, hashed
# bundle) on purpose — that's the input to the binary build. Don't commit it;
# `git checkout -- crates/dioxus-mcp/ui-dist/index.html` restores the placeholder.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLAYGROUND="$ROOT/playground"
OUT="$PLAYGROUND/target/dx/dx-playground/release/web/public"
DEST="$ROOT/crates/dioxus-mcp/ui-dist"

echo ">> building cockpit (dx build --release --platform web)"
# Clean the dx web output so `public/assets/` holds only this build's hashed
# files (dx appends across builds otherwise, bloating the embed).
rm -rf "$PLAYGROUND/target/dx/dx-playground/release/web"
( cd "$PLAYGROUND" && dx build --release --platform web )

if [[ ! -f "$OUT/index.html" ]]; then
  echo "!! expected build output missing: $OUT/index.html" >&2
  exit 1
fi
if grep -q "DIOXUS_MCP_UI_PLACEHOLDER" "$OUT/index.html"; then
  echo "!! build output is the placeholder, not a real bundle — aborting" >&2
  exit 1
fi

echo ">> staging into $DEST"
rm -rf "$DEST/assets"
cp -r "$OUT/assets" "$DEST/assets"
cp "$OUT/index.html" "$DEST/index.html"

echo ">> done. ui-dist now holds the real cockpit:"
echo "   index.html  $(wc -c < "$DEST/index.html") bytes"
echo "   assets/     $(find "$DEST/assets" -type f | wc -l) files"
echo ">> next: cargo build -p dioxus-mcp  (bakes it into the binary)"
