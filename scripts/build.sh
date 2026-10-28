#!/usr/bin/env bash
# build.sh <provider> — builds providers/<provider> to WASM and packs it into
# dist/<name>-<version>.teletapext with `teletap ext pack`.
# TELETAP_SIGN_KEY=<key file> signs it (teletap/ extensions load only signed by teletap).
# TELETAP=<path> picks the teletap binary (default: teletap on PATH).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
name=${1:?usage: build.sh <provider>}
src=$root/providers/$name
[ -f "$src/manifest.toml" ] || { echo "build.sh: no $name" >&2; exit 2; }
crate=$(sed -n 's/^name = "\(.*\)"/\1/p' "$src/Cargo.toml" | head -1)
(cd "$root" && cargo build -q --release --target wasm32-unknown-unknown -p "$crate")
stage=$root/dist/$name
rm -rf "$stage"; mkdir -p "$stage"
cp "$src/manifest.toml" "$stage/"
cp "$root/target/wasm32-unknown-unknown/release/${crate//-/_}.wasm" "$stage/extension.wasm"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$src/manifest.toml" | head -1)
sign=()
[ -n "${TELETAP_SIGN_KEY:-}" ] && sign=(--sign-key "$TELETAP_SIGN_KEY")
"${TELETAP:-teletap}" ext pack "$stage" -o "$root/dist/$name-$version.teletapext" ${sign[@]+"${sign[@]}"}
