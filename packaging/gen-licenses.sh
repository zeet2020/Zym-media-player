#!/usr/bin/env bash
# Regenerate THIRD-PARTY-LICENSES.md for the player and every source plugin.
# Doubles as a license gate: cargo-about fails if any dependency's license isn't in about.toml.
# Needs: cargo install cargo-about --locked --features cli
set -euo pipefail
cd "$(dirname "$0")/.."
out=THIRD-PARTY-LICENSES.md
cargo about generate -c about.toml about.hbs -o "$out"
for dir in sources/*/; do
  [ -f "$dir/Cargo.toml" ] || continue
  printf '\n\n# Plugin: %s\n\n' "$(basename "$dir")" >> "$out"
  cargo about generate -c about.toml --manifest-path "$dir/Cargo.toml" about.hbs >> "$out"
done
echo "wrote $out ($(wc -l < "$out") lines)"
