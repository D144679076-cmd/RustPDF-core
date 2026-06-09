# Bold Detection + On-Screen Repaint — Implementation Report

**Date:** 2026-06-07
**Scope:** edit-text bold detection (Rust), commit repaint (Vue/TS)

## What Was Implemented

### Bug 1 — Bold detection for embedded CID fonts

**`pdf-editor-rust-core/src/content/interpreter.rs`**
- Added `embedded_bold_from_font_file()` helper: navigates `FontDescriptor → FontFile2 → TrueTypeFont::parse → .is_bold()`. Called only when composite font + bold still false after cheaper checks.
- Extended `resolve_font_style` to read `/StemV` from the FontDescriptor. Added `const STEM_V_BOLD_THRESHOLD: f64 = 120.0`. Bold is now detected via any of: `/Flags` FORCE_BOLD, `/FontWeight >= 700`, `StemV >= 120`, name contains "bold", or embedded TrueType OS/2/macStyle.
- Added `build_pdf_with_font_stemv` test fixture builder (carries `/StemV`).
- Added tests: `resolve_font_style_high_stemv_is_bold` (StemV 140 → bold), `resolve_font_style_low_stemv_not_bold` (StemV 90 → not bold).

**`pdf-editor-rust-core/src/fonts/truetype.rs`**
- Added fields to `TrueTypeFont`: `mac_style_bold: bool`, `mac_style_italic: bool`, `os2_weight_class: Option<u16>`.
- Added `parse_head_mac_style()`: reads `head.macStyle` u16 at table offset 44.
- Added `parse_os2_weight_class()`: reads `OS/2.usWeightClass` u16 at table offset 4. OS/2 table is optional (mirrors the optional-`cmap` pattern).
- Added `TrueTypeFont::is_bold()` (`os2_weight_class.is_some_and(|w| w >= 600) || mac_style_bold`) and `is_italic()` (macStyle bit 1).
- Added tests: `parse_reads_macstyle_bold`, `parse_reads_macstyle_italic`, `parse_os2_weight_class_bold`, `parse_missing_os2_is_none` (with crafted-byte font builder `build_ttf_with_style`).

### Bug 2 — Committed text not repainting on-screen

**`web-editor/src/stores/usePdfStore.ts`** — `commitRenderPage`
- After the tile-composite fast path, added an authoritative follow-up: `renderQueue.invalidatePageDirect(pageIndex)` then `editorRenderPage(pageIndex, cssScale * dpr)`. This guarantees committed pixels reach the canvas regardless of tile success or scale-key cache mismatches. Updated doc comment to match.

**`web-editor/src/components/AnnotationOverlay.vue`**
- Added a `watch(() => store.commitPaintTick, …)` alongside the existing `renderTick` watch. Clears the preview overlay canvas immediately when the commit paint lands, so the 8 s fallback is never needed.

**`web-editor/src/composables/useLazyRender.ts`**
- `commitPaintTick` watch cache-miss path: when `getCached()` returns nothing, enqueues `editorRenderPage` instead of silently returning, ensuring the canvas still updates.

## Design Decisions

- **No change to `run_needs_substitute`**: once `orig_bold=true` (from correct detection), leaving B ON gives `style.bold == orig_bold` + `FontChoice::Original` → no substitution. The fix flows naturally from detection alone.
- **StemV threshold 120.0**: safe separation between normal (70–90) and bold (120–160) Latin fonts, consistent with PDF spec guidance and common writer output (this repo's writer uses StemV=80 for normal).
- **OS/2 checked before macStyle in `is_bold()`**: OS/2 `usWeightClass` is the authoritative numeric weight; macStyle bit 0 is the fallback when OS/2 is absent.
- **Tile composite kept as instant preview**: the authoritative `editorRenderPage` runs after the tile paint, not instead of it. Users see immediate feedback, then a corrected raster.

## Test Coverage

302 tests pass (6 new, 2 ignored). New tests:
- `resolve_font_style_high_stemv_is_bold` — StemV heuristic, happy path
- `resolve_font_style_low_stemv_not_bold` — StemV heuristic, below-threshold
- `parse_reads_macstyle_bold` — macStyle bit 0
- `parse_reads_macstyle_italic` — macStyle bit 1
- `parse_os2_weight_class_bold` — OS/2 usWeightClass 700
- `parse_missing_os2_is_none` — no OS/2 table → field is None

## Known Limitations / Follow-up

- **CFF / FontFile3**: no cheap weight parse — StemV is the only signal for CFF-embedded CID fonts whose descriptor also lacks weight metadata. A CFF Top DICT parser could be added later if needed.
- **Italic via OS/2**: `is_italic()` reads only `head.macStyle` bit 1. `OS/2.fsSelection` bit 0 would be more precise but requires parsing an additional field; deferred.
- **`render_metrics` dead-code warning**: pre-existing, only present under `--features wasm` without `render`; clean under `wasm-render`.
