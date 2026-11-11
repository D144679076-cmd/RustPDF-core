# Shading Types 6/7 (Coons / Tensor Patches) — Implementation Report

**Date:** 2026-11-11
**Scope:** `render/shading/patch.rs` (new) + mesh `/Function` override wiring (`shading.rs`, `mesh.rs`, `ps.rs`)

## What Was Implemented

- **`src/render/shading/patch.rs`** (new module) — ShadingType 6 (Coons) and
  ShadingType 7 (tensor) patch meshes, ISO 32000-1 §8.7.4.6/§8.7.4.7:
  - `parse(dict, data, shading_type, function)` — flag machine (flags 0–3)
    ported exactly from pdf.js v3.11.174 `src/core/pattern.js`
    (`_decodeType6Shading`/`_decodeType7Shading`): ps[]/cs[] index maps for
    flag 0 (12/16 coords + 4 colours) and chained flags 1–3 (8/12 coords +
    2 colours, shared edge + colours inherited from the previous patch).
  - Coons interior points p11/p12/p21/p22 (ps[5]/ps[6]/ps[9]/ps[10])
    derived from the boundary via the reference /9 formulas.
  - `build_lattice` — reference `_buildFigureFromPatch` + `_updateBounds`:
    bicubic Bernstein surface evaluation (`bernstein()` = `getB` LUT),
    bilinear corner-colour blend, per-figure triangle density
    `clamp(ceil(figSize×20/boundsSize), 3, 20)` per axis, Type-5-style
    `[a,b,c],[c,b,d]` triangulation. Output is the same `MeshShading`
    lattice as Types 4/5, so the shared rasteriser handles all mesh types.
  - Errors: unknown flag, truncated stream, chained patch without flag-0,
    empty patch/colour sets (R1, all with offsets).
- **Mesh `/Function` override** (§8.7.4.4), Types 4–7: when `/Function` is
  present each colour record carries **1 component** (a t value) expanded
  through the function (`numComps = fn ? 1 : cs.numComps`), ported from
  pdf.js `MeshStreamReader` (`colorFn` path):
  - `mesh.rs`: new `MeshDecode` struct (`from_dict`, `comps_per_record`,
    `read_comps` with /Function expansion, `read_coord`) shared by Types
    4–7; `BitReader`/`decode_int`/`map_unit` made `pub(crate)`, added
    `len_bits()`.
  - `shading.rs`: `/Function` is now **optional** for mesh types (was
    required before the type dispatch — this broke function-less meshes);
    still required for Types 1/2/3. Dispatch 6/7 → `patch::parse`.
  - `ps.rs`: `eval1(src, t, domain)` — 1-input PostScript evaluation;
    `ShadingFunction::eval1(t)` dispatches PostScript → `ps::eval1`,
    other types → `eval(t)`.
- **`mesh.rs`** module header + `Shading::Mesh` doc comments updated
  (6/7 no longer "deferred"); `shading.rs` unsupported-type error message
  updated.
- **`page_renderer.rs`**: `apply_mask_to_image` takes `&mut [u8]` (clippy
  `ptr_arg`, surfaced when clippy was run with `--features render`).
- **Fixture**: `tests/fixtures/mesh_patches.pdf` — one page, one Coons
  (type 6) and one tensor (type 7) shading; MuPDF reference
  `tests/fixtures/ref/mesh_patches-1.ppm` (generated, gitignored like the
  other refs).
- **Reference**: `tests/mesh-reference.js` — committed copy of pdf.js
  v3.11.174 `pattern.js` mesh code (port authority, same playbook as
  CCITT/JBIG2).

## Design Decisions

- **Byte-aligned patch records** (divergence from pdf.js): pdf.js does not
  `align()` between patch records; ISO 32000-1 §8.7.4.6 requires each patch
  to be byte-aligned, and MuPDF (our golden master) follows the spec. We
  align per patch — documented in the module header.
- **Colours blended in f64 component space**; pdf.js quantises to Uint8
  RGB through the colour space. Within snapshot tolerance; keeps the
  component-space pipeline (`components_to_rgba`) intact.
- **Density bounds over control points only** (pdf.js computes mesh bounds
  before expanding figures) — matches `_updateBounds` ordering.
- **Corner vertices evaluated, not patched in**: pdf.js skips corner pixels
  and copies control points into the lattice; at t=0/1 the Bernstein basis
  is degenerate ([1,0,0,0]/[0,0,0,1]) so the surface evaluation yields the
  same control points exactly — the `continue` in pdf.js is an optimisation.
- **Interior-point formulas computed before pushing**: all four Coons
  interior points depend only on boundary points, so coordinates are
  computed first, then pushed — avoids borrow conflicts (no closures over
  `coords` across mutation).
- **`Patch::ps` convention**: `q = 4·i + j`, `i` the first Bernstein
  parameter, `j` the second; label `pRC` means `j = R, i = C` (matches the
  pdf.js comment ordering `p00, p10, …, p30, p01, …, p33`).

## Test Coverage

- `coons_square_patch_corner_colors` — flag-0 Coons: 21×21 lattice, corner
  positions and corner colours (happy path).
- `coons_flag1_chain_shares_edge` — flag-1 chaining: shared edge/colours,
  new stream order p13,p23,p33,p32,p31,p30,p20,p10, chained c00 = prev c03,
  density from global bounds (20×10 lattice per figure).
- `tensor_square_patch_corners` — flag-0 tensor: 16-point stream, interior
  p11/p12/p22/p21, corner colours/positions.
- `patch_bad_flag_is_error` / `patch_truncated_is_error` /
  `chained_patch_without_flag0_is_error` — error paths (R5).
- `rasterize_is_bounded_by_canvas` — inflated /Decode, no panic.
- `function_override_expands_t_to_components` (mesh.rs) — type 4 with
  exponential /Function: records carry 1 t value, comps = f(t).
- Snapshot harness: `mesh_patches.pdf` parity with MuPDF at 72 dpi.

## Verification

- `cargo fmt --check` ✓
- `cargo clippy --features render -- -D warnings` ✓ (also default features)
- `cargo test --features render --lib --tests` ✓ — 460 lib + all
  integration suites green (incl. snapshot parity)
- `cargo build --target wasm32-unknown-unknown` ✓

Note: plain `cargo test` currently fails to compile
`examples/generate_pdf2_fixtures.rs` (uses `pdf_core::writer`, gated behind
the `writer` feature) — **pre-existing at clean HEAD `b132ea2`** (verified
by stash), unrelated to this change; `--lib --tests` is the effective
verification set.

## Known Limitations / Follow-up

- ShadingType 0 (sampled) still unimplemented.
- `/Function` override always uses colour-space `n_components` output
  width; a function returning a different arity than the colour space is
  not rejected (pdf.js behaves the same).
- Patch density uses `usize` clamp after `ceil() as usize` — huge extents
  could saturate before the clamp; harmless (clamped to 20 either way).
- MuPDF reference for the fixture is gitignored like all other refs;
  regenerate with `tests/gen_snapshot_refs.sh` on fresh checkouts.
