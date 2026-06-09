# render-chart-phase4 — Implementation Report

**Date:** 2026-05-28
**Scope:** PDF stream PNG/TIFF predictor unfilter support

## What Was Implemented

### 1. `apply_png_predictor` — `src/parser/filters.rs`

New public function implementing the PNG predictor unfilter (ISO 32000-1 §7.4.4.4, Predictor 10–15). After zlib decompression, FlateDecode streams with a predictor embed a 1-byte filter-type header per scanline followed by predictor-encoded pixel deltas. This function strips the headers and reconstructs raw pixel values for all five PNG filter types: None (0), Sub (1), Up (2), Average (3), Paeth (4).

### 2. `apply_tiff_predictor` — `src/parser/filters.rs`

New public function implementing the TIFF horizontal differencing predictor (Predictor 2). Each sample is stored as the delta from the previous sample in the same row; this function reconstructs absolute values.

### 3. `PdfStream::decode()` — `src/parser/objects.rs`

Extended to read `/DecodeParms` after the filter pipeline and apply the appropriate predictor:
- `Predictor >= 10` → `apply_png_predictor`
- `Predictor == 2` → `apply_tiff_predictor`
- `Predictor <= 1` (default) → no-op

### 4. `PdfStream::decode_parms()` — `src/parser/objects.rs`

New private helper that extracts `/DecodeParms` as a `&PdfDict`. Handles both direct dictionary form and single-element array form (the common PDF convention for a single-filter stream). For multi-filter arrays, returns the last dict (innermost/final filter's parms).

### 5. `pdf_int_from_obj` — `src/parser/objects.rs`

New private helper that extracts `i64` from `Integer` or `Real` PdfObject variants. Scoped to `objects.rs` to avoid a public API change.

## Design Decisions

- **Apply predictor at `PdfStream::decode()` level, not in `image.rs`**: The predictor is a property of the stream encoding, not of image semantics. Fixing it at the stream level means every caller of `decode()` — including SMask streams, font streams, content streams, and XRef streams — automatically gets correct data without per-call changes.
- **`decode_parms` returns last dict for array form**: PDF spec says each entry in a `/DecodeParms` array corresponds to the same-position filter in the `/Filter` array. For predictor purposes we want the parms for the FlateDecode filter, which is always last (innermost). Iterating in reverse and returning the first dict found is correct for all common cases.
- **No panic on trailing data**: If `data.len()` is not a multiple of `stride`, the trailing partial row is silently ignored. This is consistent with how PDF viewers handle malformed streams.
- **Unknown filter type treated as None**: A filter type byte other than 0–4 falls through to `recon.copy_from_slice(row)`. This avoids hard failures on non-conforming PDFs.

## Root Cause of Blurry Rendering

`PdfStream::decode()` previously applied only zlib decompression. FlateDecode streams with `/DecodeParms << /Predictor 15 ... >>` are extremely common for PDF image data (both color images and SMask streams). Without the unfilter step, the predictor header bytes and delta-encoded values landed directly in the pixel buffer:

- **Color image streams**: wrong RGB values → distorted colors
- **SMask streams**: near-zero delta bytes → near-zero alpha → semi-transparent content → blurry, washed-out appearance

## Test Coverage

| Test | What it covers |
|------|---------------|
| `test_png_predictor_none` | filter_type=0 passes bytes through unchanged |
| `test_png_predictor_sub` | Sub reconstruction: recon[i] = raw[i] + recon[i-bpp] |
| `test_png_predictor_up` | Up reconstruction across two rows |
| `test_png_predictor_paeth_trivial` | Paeth with known context values |
| `test_tiff_predictor_basic` | 2-pixel row delta reconstruction |
| `test_tiff_predictor_multi_row` | Two-row TIFF predictor |

## Phase 4b Fix — Indirect `/DecodeParms` Reference (2026-05-28)

After Phase 4 deployed, `[predictor]` log never appeared. Root cause: in real PDF files
`/DecodeParms` is almost always stored as an **indirect reference** (`5 0 R`), parsed as
`PdfObject::Reference(5, 0)`. `decode_parms()` only matched `Dictionary` and `Array`,
so the reference silently fell through to `None` and the predictor was never applied.

`PdfStream` has no access to `PdfDocument`, so `decode()` cannot follow the reference.

### Fix

Three changes:

1. **`apply_predictor(data, parms)` private fn** — factored out of the inline block in
   `decode()` so both `decode()` and `decode_with_doc()` share the same predictor logic.

2. **`PdfStream::decode_with_doc(&self, doc: &PdfDocument)`** — new public method.
   Resolves `/DecodeParms` through the document if it is a `Reference`, then applies the
   predictor. This is the preferred call site when a document is available.

3. **Call-site updates** — `decode()` calls replaced with `decode_with_doc(doc)` at every
   location that has document access:
   - `PdfDocument::get_stream_data()` (line ~302, non-crypto path) → `s.decode_with_doc(self)`
   - `draw_image_xobject()` main image decode → `stream.decode_with_doc(self.doc)`
   - `draw_image_xobject()` SMask decode → `smask_stream.decode_with_doc(self.doc)`

`decode()` is kept for call sites that have no document access (XRef stream bootstrap
parsing, object stream parsing).

## Phase 4c Fix — Array Element Reference Resolution + Sub-byte Indexed (2026-05-28)

After Phase 4b, `[predictor]` log STILL never appeared. Root cause: the actual PDF stores
`/DecodeParms [5 0 R]` — the array's element is the indirect reference, not the top-level
entry. Phase 4b only resolved top-level refs. The array branch in `decode_with_doc` silently
skipped Reference elements: `_ => None`.

Three additional bugs were found and fixed:

**Bug 1 (primary — predictor still not firing):** `decode_with_doc` array branch resolves
each element via `doc.resolve()` before matching Dictionary. Handles all four storage forms.

**Bug 2 — sub-byte indexed image indices:** `apply_indexed_lookup` now accepts `bpc: u8`
and unpacks 1/2/4-bit indices before palette lookup (PDF §8.9.3 — multiple indices packed
per byte for `bpc < 8`). Tests: `test_apply_indexed_lookup_4bit`, `test_apply_indexed_lookup_1bit`.

**Bug 3 — wrong `bpc` after indexed expansion:** After `apply_indexed_lookup`, expanded data
is always 8-bit per channel regardless of original `bpc`. `draw_image_xobject` now uses
`effective_bpc = 8` for `decode_image` after indexed expansion.

**Diagnostic warning added:** `decode_with_doc` emits `log::warn!("[decode] DecodeParms present
but unresolved ...")` when `/DecodeParms` is present but resolves to `None`, making future
reference resolution failures visible in the browser console.

**New test:** `test_decode_with_doc_decode_parms_array_of_refs` in `objects.rs` — crafts a
PDF where `/DecodeParms [3 0 R]` with the array element being an indirect ref to the predictor
dict, verifies Average predictor is applied correctly.

## Known Limitations / Follow-up

- **16-bit samples**: `apply_png_predictor` treats all data as bytes. For `BitsPerComponent=16`, `bpp` is computed correctly but the byte-level delta arithmetic is applied to individual bytes rather than 2-byte samples. This is incorrect for 16-bit images but irrelevant for the 8-bit chart images addressed here.
- **Multi-filter predictor pairing**: For streams with multiple filters, `decode_parms()` returns the last dict. If a stream has `[ASCIIHexDecode, FlateDecode]` with parms `[Null, <<...>>]`, this works correctly. The edge case of a predictor on a non-final filter is not handled but does not occur in practice.
- **XRef stream predictor**: XRef streams also use FlateDecode + predictor. They are parsed via a separate code path that calls `decode()` (without doc). XRef predictor support would require a separate refactor of the bootstrap parser.
