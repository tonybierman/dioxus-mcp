#!/usr/bin/env bash
# Build the dx-playground UI and stage it for embedding into the dioxus-mcp
# server. Run this, then rebuild the server (`cargo build -p dioxus-mcp`) so
# include_dir! picks up the new bundle. For UI iteration without rebuilding the
# server, skip this and run the server with DIOXUS_MCP_UI_DIR pointing at the
# `dx build`/`dx serve` output dir instead.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
crate_dir="$(dirname "$here")"               # crates/dioxus-mcp
playground="${1:-$crate_dir/../../playground}"  # in-repo by default
dest="$crate_dir/ui-dist"

echo "Building UI in: $playground"
( cd "$playground" && dx build --release --platform web )

src="$playground/target/dx/dx-playground/release/web/public"
if [[ ! -f "$src/index.html" ]]; then
  echo "error: expected build output at $src (dx output layout may have changed)" >&2
  exit 1
fi

echo "Staging bundle into: $dest"
rm -rf "$dest/assets" "$dest/index.html"
mkdir -p "$dest"
cp "$src/index.html" "$dest/index.html"
cp -r "$src/assets" "$dest/assets"

echo "Done. Now rebuild the server: cargo build -p dioxus-mcp"
