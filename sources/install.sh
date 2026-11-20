#!/usr/bin/env bash
# Build a content plugin and install it into Zym Media Player's sources folder.
# Usage: sources/install.sh sources/<plugin-dir>
set -euo pipefail
cd "${1:?usage: install.sh <plugin-dir>}"
cargo build --release --target wasm32-unknown-unknown
crate=$(sed -n 's/^name = "\(.*\)"/\1/p' Cargo.toml | head -1 | tr - _)
id=$(python3 -c 'import json; print(json.load(open("manifest.json"))["id"])')
dest="${XDG_CONFIG_HOME:-$HOME/.config}/zym-player/sources/$id"
mkdir -p "$dest"
cp manifest.json "$dest/"
cp "target/wasm32-unknown-unknown/release/$crate.wasm" "$dest/plugin.wasm"
echo "installed $id -> $dest"
