# Blend Modes (all 17) — Implementation Report

**Date:** 2026-11-11
**Scope:** `content/graphics_state.rs` (BlendMode enum) + `render/canvas.rs` (composite_over blend functions)

## What Was Implemented

Plan §1.7: the missing blend modes, per ISO 32000-1 §11.3.5.

- **Enum + parsing** (`graphics_state.rs`): added the four non-separable
  variants `Hue`, `Saturation`, `Color`, `Luminosity` and their
  `from_name` arms. All 15 non-Normal PDF blend modes now parse.
- **Separable blend functions** (`canvas.rs`, module-level helpers):
  `blend_hard_light` (multiply on the dark half, screen on the light half),
  `blend_color_dodge`, `blend_color_burn`, `blend_soft_light` (spec's
  polynomial/sqrt `d` branches). These close the silent Normal-fallback
  gap: `ColorDodge`, `ColorBurn`, `HardLight`, `SoftLight`, `Difference`
  (inline `|cb − cs|`), `Exclusion` (inline) previously fell through the
  `_ =>` arm and rendered as Normal.
- **Non-separable HSL functions** (`canvas.rs`): `lum` (0.3/0.59/0.11
  weights), `sat`, `clip_color` (gamut clip by scaling chroma toward the
  luminance axis, min side then max side), `set_lum`, `set_sat`
  (index-tracking sort so rescaled components land back in their original
  positions). Wired as: Hue = SetLum(SetSat(Cs, Sat(Cb)), Lum(Cb)),
  Saturation = SetLum(SetSat(Cb, Sat(Cs)), Lum(Cb)),
  Color = SetLum(Cs, Lum(Cb)), Luminosity = SetLum(Cb, Lum(Cs)).
- **`Overlay` rewritten as `blend_hard_light(cs, cb)`** — mathematically
  identical to the old inline expression (verified at the Cb = 0.5
  boundary), removing the duplicated formula.
- The per-pixel composite path in `composite_over` is unchanged otherwise:
  blends operate on straight (unpremultiplied) colours, results pass through
  the existing Porter-Duff source-over with group alpha.

## Design Decisions

- **Kept the per-pixel fallback path** per plan guidance: `tile.rs` has no
  band/group compositing of its own — all blending funnels through
  `PixmapBuffer::composite_over`, so no structural change was needed; the
  tile structure question resolved itself.
- **Blend helpers at module level in `canvas.rs`** (not associated fns):
  pure math, no state; keeps `composite_over`'s match arms one-liners.
- **`clip_color` guards division by zero** (`l − n` / `x − l`) explicitly —
  R1 no-panic; degenerate inputs (already-clipped colours) pass through.
- **Soft-light uses `cs <= 0.5` for the shade branch** matching the spec's
  convention (and CSS); the boundary is continuous there.

## Test Coverage

- **13 new tests** (`render/canvas.rs` + `graphics_state` via canvas test,
  521 lib total with `render`):
  - A `blend_1x1` helper composites two opaque 1×1 pixels per mode.
  - Separable happy paths: Difference (red/blue → magenta, exact),
    Exclusion (mid-gray ≈ 0.5), ColorDodge (0.251/0.5 → ≈ 0.502; zero
    backdrop → 0), ColorBurn (cs = 1 → cb; full backdrop → white),
    HardLight (screen half ≈ 0.624), SoftLight (dark source darkens ≈ 0.378).
  - Non-separable: Luminosity (gray over red → clipped (1, ≈0.286, ≈0.286)),
    Color (gray over red → gray at red's luminance ≈ 0.3), Hue (blue over
    red → (≈0.21, ≈0.21, 1)), Saturation (saturated source over gray →
    gray preserved).
  - `from_name` parses all 10 previously-fallback names + rejects unknown.
- Existing suites: 521 lib + all integration binaries (incl. MuPDF snapshot
  and jpx parity) pass; fmt/clippy (`-D warnings`, ± `render`) and wasm32
  build clean.

## Known Limitations / Follow-up

- Blending uses straight-space colours with the composite after blend;
  backdrop is assumed opaque where it is not, the existing source-over
  formula applies (unchanged behaviour, matches spec's `αb`-weighted result
  only approximately for partially transparent backdrops — pre-existing).
- Soft-light on out-of-gamut backdrop components (cannot occur here; pixels
  are u8-clamped) is undefined by the spec — not applicable.
- Blend mode is only honoured where `composite_over` is called
  (transparency groups, pattern/shading fills). Text and path draws bypass
  group compositing when no soft mask is active (pre-existing structure);
  per-op blending for those remains a larger refactor if ever needed.
