#!/usr/bin/env bash
# SBOMs and CBOM for a Gitta CLI release. Reads Cargo.lock files only: nothing is built.
#   sbom/generate.sh VERSION OUT_DIR
# Writes, per release binary (gitta, gitta-fs, gitta-sandbox):
#   <bin>-VERSION.cdx.json (CycloneDX 1.6) and <bin>-VERSION.spdx.json (SPDX 2.3)
# and gitta-VERSION.cbom.cdx.json (CycloneDX 1.6 cryptography bill of materials).
# Needs syft on PATH (SYFT=path to override). Validates with cyclonedx-cli when it is on PATH (CYCLONEDX=path).
set -euo pipefail
v="${1:?version}"; out="${2:?out dir}"; here="$(cd "$(dirname "$0")/.." && pwd)"
syft="${SYFT:-syft}"; cdx="${CYCLONEDX:-cyclonedx}"
export SYFT_CHECK_FOR_APP_UPDATE=false
mkdir -p "$out"
python3 "$here/sbom/cbom.py" check "$here/sbom/crypto-inventory.json"
for bin in gitta gitta-fs gitta-sandbox; do
  dir=$bin; [ $bin = gitta ] && dir=gitta-cli
  "$syft" scan "file:$here/$dir/Cargo.lock" -q --source-name "$bin" --source-version "$v" \
    -o "cyclonedx-json@1.6=$out/$bin-$v.cdx.json" -o "spdx-json=$out/$bin-$v.spdx.json"
done
python3 "$here/sbom/cbom.py" cbom "$here/sbom/crypto-inventory.json" --version "$v" -o "$out/gitta-$v.cbom.cdx.json"
if command -v "$cdx" >/dev/null; then
  for f in "$out"/*-"$v".cdx.json "$out/gitta-$v.cbom.cdx.json"; do "$cdx" validate --input-file "$f" --input-version v1_6 --fail-on-errors >/dev/null && echo "valid CycloneDX 1.6: $(basename "$f")"; done
fi
