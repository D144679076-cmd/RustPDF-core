# optimizer — Implementation Report

**Date:** 2026-06-16
**Scope:** writer/optimizer — Phase 2 PDF optimization/compression

## What Was Implemented

- `src/writer/optimizer.rs` — new module (gated on `writer` + `crypto` features)
  - `OptimizationOptions` struct with `Default` impl
  - `pub fn optimize(doc_bytes: &[u8], options: &OptimizationOptions) -> Result<Vec<u8>>`
  - `collect_reachable_ids` — BFS from `/Root` and `/Info` to GC unreachable objects
  - `build_dedup_map` — SHA-256 stream deduplication (cfg-gated on `crypto`)
  - `assign_new_ids` — sequential ID remapping with deterministic sort
  - `transform_object` / `transform_stream` — per-object copy, remap, recompress
  - `remap_object` / `remap_dict` — deep reference remapping
  - `is_image_stream` / `downsample_if_needed` — nearest-neighbour DeviceRGB downscaling
  - `sha256` helper (cfg-gated on `crypto`, uses `sha2` crate)
- `src/writer/mod.rs` — added `pub mod optimizer` under `#[cfg(feature = "crypto")]`
- `src/wasm/editor.rs` — added `WasmEditor::optimize` binding + `parse_optimization_options`, `json_opt_bool`, `json_opt_u32` helpers (all gated on `crypto`)
- `src/editor/annotation.rs` — fixed pre-existing `unused variable: rect` clippy warning (gated declaration on `#[cfg(feature = "forms")]`)

## Design Decisions

- **Gated on `crypto` feature**: optimizer calls `crate::license::require` (requires `crypto`) and uses `sha2` for dedup. Without `crypto`, the module is excluded. The `wasm` feature already includes `crypto`, so WASM builds always have it.
- **Dedup skipped without `crypto`**: `build_dedup_map` returns empty map when `crypto` is absent, so the optimizer still compiles and works (minus dedup) in that configuration.
- **Deterministic ID assignment**: IDs are sorted before assignment so the same input always produces the same output, making tests reproducible.
- **No `reserve_id` pre-allocation**: the spec draft called `reserve_id` in a loop, but `set_object` already advances `next_id` automatically. The loop was redundant and removed.
- **`shift_remove` over `remove`**: used `IndexMap::shift_remove` to remove `/DecodeParms` and `/Filter` to avoid the deprecated `remove` alias.
- **DeviceRGB-only downsampling**: nearest-neighbour downscale guards on `decoded.len() == width * height * 3` to avoid corrupting grayscale/CMYK images where channel count differs.
- **Pro license gate**: `optimize` calls `crate::license::require(Tier::Pro, "optimize")` at entry; in test builds this is a no-op.

## Test Coverage

All tests in `src/writer/optimizer.rs` under `#[cfg(test)]`:

| Test | What it covers |
|------|----------------|
| `optimize_output_is_valid_pdf` | Output starts with `%PDF-` and ends with `%%EOF` |
| `optimize_preserves_page_count` | Multipage PDF still has 3 pages after optimization |
| `optimize_removes_unused_objects` | Minimal PDF parses cleanly after GC pass |
| `optimize_reduces_file_size_or_equal` | Optimized bytes ≤ original + 512 (xref overhead allowed) |
| `optimize_no_recompress_still_valid` | With `recompress_streams: false`, output still parses correctly |

Run with: `cargo test --features writer,crypto -- optimize`

## Known Limitations / Follow-up

- **Downsampling is DeviceRGB-only**: CMYK (4-channel) and grayscale images are returned unchanged. A follow-up could detect channel count from `/ColorSpace` and `/BitsPerComponent`.
- **No JPEG re-encoding**: downsampling re-compresses via FlateDecode (lossless). For further size reduction, lossy JPEG re-encoding would be needed (`zune-jpeg` encode, which the crate doesn't yet bundle).
- **DPI estimate is heuristic**: without the page's MediaBox we use `max_dpi * 11` pixels as threshold (11 inch max page). For images placed on small pages this may under-downsample.
- **Dedup doesn't merge XObject references in content streams**: only object-level dedup (same stream bytes → same object ID). If two pages reference the same image under different resource names, the XObject objects are merged but the resource dicts still contain both names pointing to the same new ID.
- **Phase spec test command was `--features crypto`** (missing `writer`). Corrected to `--features writer,crypto`. The `wasm` build already pulls in both.
