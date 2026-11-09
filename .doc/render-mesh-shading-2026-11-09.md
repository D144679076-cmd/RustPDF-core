# Render Mesh Shading — Implementation Report

**Date:** 2026-11-09
**Scope:** Shading types 1, 4, 5 — `src/render/shading/` (ps.rs, mesh.rs, function_shaded.rs) + integration in shading.rs, interpreter.rs, page_renderer.rs

## What Was Implemented

- **`shading/ps.rs`** — FunctionType 4 PostScript-calculator evaluator (ISO 32000-1 §7.10.4):
  - Tokenizer: numeric literals (incl. `-x`, `+x`, `eE` exponents), `%` comments, `{`/`}` blocks, whitespace-insensitive operators. Unknown bytes → error.
  - `eval2(src, t0, t1, domain: [f64;4])` — 2-input entry point; inputs normalized/clamped into `[x_lo x_hi]` / `[y_lo y_hi]`.
  - Operators: arithmetic (`add sub mul div mod`), stack (`dup pop exch index copy`), math (`min max neg abs sqrt sin cos atan exp ln log floor ceil round truncate`), logical/comparison (`and or xor gt lt ge le eq ne`), control flow (`if ifelse` with block values), no-op casts (`cvx cvr cvi cvs`).
  - Correct `if`/`ifelse` pop order per PDF spec: proc on top (`cond proc if`, `cond {t} {f} ifelse`) — blocks popped before the boolean.
  - Unknown operator → error (loud failure rather than silent mis-coloring).
- **`shading/mesh.rs`** — ShadingType 4 (free-form Gouraud) + ShadingType 5 (lattice Gouraud):
  - MSB-first `BitReader` with per-vertex byte alignment.
  - `/BitsPerCoordinate` (1–32), `/BitsPerComponent` (1–16), `/BitsPerFlag` (2–8, type 4 only), `/Decode` mapping (`raw/(2^bits−1)` → `[lo,hi]`, unit-box default).
  - Type-4 flag machine mirrored from pdf.js `pattern.js` (`_decodeType4Shading`): flag `0` = fresh 3-vertex patch, `1` = shares P1–P2 edge (`ps.push(at(-2), at(-1))`), `2` = shares P0–P2 edge (`ps.push(at(-3), at(-1))`); every 3 consecutive `ps` indices form a triangle. Flag > 2 → error.
  - Type-5: `/VerticesPerRow` (required, ≥ 2), 2 triangles per grid cell.
  - Barycentric Gouraud rasterizer: per-triangle bounding box, sign-inside test, per-component interpolation through shared `components_to_rgba` (gray/RGB/CMYK), clipped blit.
- **`shading/function_shaded.rs`** — ShadingType 1 function-based shading:
  - `/Domain` (4 values, default unit box), `/Matrix` (6 values, default identity).
  - Requires a 2-input function (FunctionType 4); `n_output` from first eval output count (`n_components` passed by caller).
  - Rasterizer: inverts `ctm ∘ /Matrix`, normalizes to `n_components` color per pixel, paints the device-space bbox of the domain.
- **`shading.rs`** (modified):
  - New `ShadingFunction::PostScript { body, ps_domain }` variant + `eval2` dispatcher.
  - `ShadingFunction::parse` now accepts `Reference`/`Stream`/`Dictionary`; FunctionType 4 body comes from the decoded stream contents.
  - `Shading::parse(obj, doc)` handles Reference/Stream/Dictionary; Stream payload feeds mesh decode.
  - New `Shading::FunctionBased`, `Shading::Mesh` variants + `rasterize` dispatch; unsupported types 0/6/7 still error with explicit message.
  - `inverse_matrix`, `components_to_rgba`, `cs_n_components` promoted to `pub(crate)`.
- **`content/interpreter.rs`** — `OutputDevice::paint_shading` signature changed `&PdfDict` → `&PdfObject` (mesh types need stream data); `sh` operator passes the fully resolved object.
- **`render/page_renderer.rs`** — both call sites updated; pattern shading resolve arm properly matches `Result` and rejects non-dict/stream values.

## Design Decisions

- **pdf.js as behavioral reference** for type-4 flag semantics (user-approved): pdf.js `src/core/pattern.js` implements the `ps` index-stack exactly as decoded above; pdf.js's old `mesh.js` no longer exists in master. This avoids the classic flag-order bug (flag 1 ≠ "shares P2/P1 of previous triangle" ambiguity).
- **FunctionType 4 evaluator is a separate module**, not inlined into `ShadingFunction`: keeps `shading.rs` lean, mirrors pdf.js's `function.js`/`pattern.js` split.
- **1-input `eval` on a PostScript function** returns flat grey with a `log::warn!` (axial/radial can technically carry one, though nonsensical) instead of a hard error, so damaged files still render something.
- **Function types 0 (sampled) and 6/7 shading (Coons/tensor) deferred** — explicit errors naming the missing type rather than silent grey.
- **`paint_shading` signature break is internal-only** — `OutputDevice` is `pub(crate)`; TextExtractor/DisplayListBuilder use default no-op impls, unchanged.
- Guard rails honored: R1 (all error paths, no panics/unwrap outside tests), R2 (no new deps, `cargo build --target wasm32-unknown-unknown` green), R4 (all new files < 450 lines), R5 (happy + error tests per parser), R7-style offsets in `PdfError` (uses existing `invalid_token`/`eof` constructors with offset 0 for dict-context errors).

## Test Coverage

- `ps.rs` (12 tests): arithmetic, stack ops, comparisons, `if`/`ifelse` branch execution (both branches incl. nested blocks), input normalization + clamping, `dup` semantics, unsupported-operator error, unmatched-brace errors, stack-underflow error.
- `mesh.rs` (7 tests): lattice triangle count (2×3 grid → 4 tris), free-form flag machine (fresh + flag1 + flag2 chains → `[0,1,2],[1,2,3],[1,3,4]`), truncated stream error, bad-flag error, missing `/VerticesPerRow` error, off-canvas rasterize safety, inner-pixel painting.
- `function_shaded.rs` (3 tests): non-PostScript function rejection, default Domain/Matrix, explicit Domain/Matrix parse.
- Existing 811+ tests unchanged and green (838 total). `cargo fmt --check`, `cargo clippy` (only 2 pre-existing HEAD errors: uniffi fn-pointer, `&mut Vec`), wasm32 build all clean.

## Known Limitations / Follow-up

- ShadingTypes **6 (Coons patch) and 7 (tensor-product patch)** not implemented — parse errors explicitly state this.
- Shading **FunctionType 0 (sampled)** still unsupported in `ShadingFunction::parse`.
- Mesh functions: `ps.rs::eval2` exists but mesh `/Function` override not wired into `mesh.rs` rasterizer (vertices always carry raw components).
- Bad-type flag tests use `BitsPerFlag=8`; low-width (2-bit) flag decoding untested.
- No integration fixture yet (no mesh-shading `tests/fixtures/*.pdf`) — recommend adding when the snapshot harness question (MuPDF AGPL renders vs hand-crafted expected images) is settled.
- Open question for user: snapshot-harness reference for render-completeness verification.
