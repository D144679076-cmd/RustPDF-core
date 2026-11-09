#!/usr/bin/env bash
# Generate reference renders for the render-snapshot harness using MuPDF.
#
# Usage:    tests/gen_snapshot_refs.sh [fixture.pdf ...]
# Default:  every .pdf in tests/fixtures/ (bun script requires `mutool` on PATH).
#
# Output:   tests/fixtures/ref/<stem>-<page#>.ppm   (one per page, 72 DPI)
#           MuPDF names pages 1-based; tests/snapshot.rs matches that.
#
# Rationale: references are MuPDF (AGPL) *renders* — image data only, no MuPDF
# code linked or distributed, per the license decision recorded in
# .doc/render-snapshot-harness-2026-11-09.md.
set -euo pipefail

CD=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
FIX="$CD/fixtures"
REF="$FIX/ref"
command -v mutool >/dev/null || { echo "error: mutool not on PATH (sudo apt install mupdf-tools)" >&2; exit 1; }
mkdir -p "$REF"

pdfs=("$@")
if [ ${#pdfs[@]} -eq 0 ]; then
    while IFS= read -r f; do pdfs+=("$f"); done < <(find "$FIX" -maxdepth 1 -name '*.pdf' | sort)
fi
[ ${#pdfs[@]} -gt 0 ] || { echo "no fixtures found" >&2; exit 1; }

for pdf in "${pdfs[@]}"; do
    stem=$(basename -- "${pdf%.*}")
    echo "→ $stem"
    rm -f "$REF/$stem"-[0-9]*.ppm
    mutool draw -o "$REF/$stem-%d.ppm" -r 72 -- "$pdf" 2>/dev/null || {
        echo "  (convert failed for $pdf — skipping; encrypted/broken?)"; continue; }
done
echo "References written to $REF/"
