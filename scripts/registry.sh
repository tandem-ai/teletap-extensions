#!/usr/bin/env bash
# registry.sh [extension…] — builds the extensions (default: every provider)
# with build.sh and publishes them into dist/registry (index.json and
# <id>/<version>.teletapext), the folder a lookup server serves at
# /dl/extensions/ (from <dist>/extensions). REGISTRY=<dir> publishes elsewhere.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  for d in "$root"/providers/*/; do names+=("$(basename "$d")"); done
fi
packages=()
for n in "${names[@]}"; do
  "$root/scripts/build.sh" "$n" >/dev/null
  src=$root/providers/$n; [ -d "$src" ] || src=$root/extensions/$n
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$src/manifest.toml" | head -1)
  packages+=("$root/dist/$n-$version.teletapext")
done
"${TELETAP:-teletap}" ext publish "${packages[@]}" --into "${REGISTRY:-$root/dist/registry}"
