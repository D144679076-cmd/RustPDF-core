# Tech-Debt Remediation — Implementation Report

**Date:** 2026-06-05
**Scope:** TD-1 … TD-10 from `.doc/tech-debt-fixes-2026-06-05.md`

All ten items addressed. Native test suite: **626 passed, 5 ignored** (was 599 at
baseline → **+27 new tests**). Builds clean on `wasm32-unknown-unknown`
(`wasm` and `wasm-render`). `cargo fmt --check` clean; the only `clippy`
warnings remaining are pre-existing and outside the changed code (see
[Known Limitations](#known-limitations--follow-up)).

---

## What Was Implemented

### TD-5 — Writer generation counter
- `PdfWriter.generation: u64`, bumped in `add_object` / `set_object` / `restore`; accessor `PdfWriter::generation()`.
- WASM `WasmEditor` cache keys (`edit_model_doc`, `text_edit_model_generation`) switched from pool **length** to **generation**.
- **Why:** pool length is wrong when `set_object` replaces in place (length unchanged) or after undo (length repeats). Generation is a correct invalidation signal.

### TD-6 — Undo / redo
- `PoolSnapshot` (opaque) + `PdfWriter::snapshot()/restore()`.
- `PdfEditor`: `undo_stack`/`redo_stack`, `checkpoint()`, `undo()`, `redo()`, `can_undo()`, `can_redo()`; history capped at `MAX_UNDO_DEPTH = 50`.
- WASM: `WasmEditor::undo/redo/can_undo/can_redo`; `text_edit_commit` calls `checkpoint()` once before any write-back path; undo/redo invalidate the derived edit caches.
- **Why:** snapshotting the (small) writer pool is simple and robust; `restore` bumps `generation` so caches rebuild. Chose full-snapshot (Approach A) over a command/delta log because the pool is small and snapshots can't drift out of sync with the real state.

### TD-7 — Inline-image boundary
- `inline_image_data_len` resolves length from explicit `/L`/`/Length`, JPEG EOI (`FF D9`) for `DCTDecode`, or `ceil(W·components·bpc/8)·H` for unfiltered rasters; helpers `inline_cs_components`, `find_jpeg_eoi`, `consume_trailing_ei`, `scan_for_ei`.
- The whitespace-`EI` scan is now a last resort, used only when length can't be computed.
- **Why:** raw image bytes can contain a whitespace-`EI` sequence; deterministic length is spec-correct (§8.9) and stops mid-image truncation.

### TD-1 — `PdfDict` = `IndexMap`
- `pub type PdfDict = IndexMap<String, PdfObject>` (`indexmap = "2"`).
- `serialize_dict` emits insertion order (was alphabetical sort).
- ~12 files migrated from explicit `HashMap<String, PdfObject>` dict construction to `PdfDict`; two dict removals switched to `shift_remove` (order-preserving). The decrypt path collects into `IndexMap`. Non-dict `HashMap`s (xref, caches, id-maps) were left untouched.
- **Why:** preserves `/Filter`↔`/DecodeParms` pairing and signed-dict byte order; O(1) lookups retained.

### TD-9 — Font-metric substitution
- `StandardFont::from_name` strips a 6-uppercase-letter `+` subset prefix before matching (ISO 32000-1 §9.6.4).
- **Why:** subsetted standard fonts (`ABCDEF+Arial-BoldMT`) were missing the metric table and falling back to estimates, mispositioning text. (The `/Widths`/`/W` array path already existed; broader `/MissingWidth` work is deferred — see follow-up.)

### TD-2 — `PdfDocument: Send + Sync`
- Four caches `RefCell` → `parking_lot::RwLock`; manual `Clone` (RwLock isn't `Clone`).
- `get_stream_data` uses double-checked locking (`entry(id).or_insert`) so concurrent render threads don't stampede the decoder.
- **Why:** `RefCell` made `PdfDocument` `!Sync`, so parallel tile rendering would not *compile*. `parking_lot` needs no `cfg` split (single-threaded shim on WASM). This unblocks the parallel-render plan.

### TD-4 — Signed-PDF detection + lossless-ish round-trip
- `PdfDocument::is_signed()` (AcroForm `/SigFlags` bit 1) + `WasmEditor::is_signed()`; `text_edit_enter` logs a warning on signed docs.
- `OpStream` snapshots its parsed ops (`orig_ops`); `commit_edit_session` skips streams whose ops are unchanged, so untouched page/XObject streams keep their original bytes.
- **Why:** prevents silent signature invalidation surprises, and shrinks the edit delta (untouched streams aren't reformatted/recompressed). Snapshot-compare is robust: a missed dirty-mark can't drop an edit.

### TD-3 — Typed operator dispatch
- New `content::operator::Operator` enum (71 variants) with `from_token`/`as_str`; the interpreter classifies once and `match`es the enum (unknown tokens log + skip).
- **Why:** the compiler now enforces exhaustive operator handling; operator-name typos in dispatch are impossible. Arm **bodies were left byte-identical** (only patterns changed), so behaviour is preserved — confirmed by the full render/interpreter test suite.

### TD-8 — Stream-decode allocation
- `apply_pipeline` no longer pre-copies the input before the first filter (saves one full copy of the compressed bytes per filtered stream).
- `apply_pipeline_cow` returns `Cow::Borrowed` for unfiltered streams (zero-copy) for future read-only callers.
- **Why:** the contained, safe win that fits the on-demand-parse model. (Full `mmap`/`Cow`-everywhere is deferred — see follow-up.)

### TD-10 — Web Worker offload (Rust side verified)
- The WASM API is already worker-ready: `save()`/`build()` and `RenderResult::rgba_bytes()` return `js_sys::Uint8Array` (transferable); metadata, `undo/redo`, `is_signed` are synchronous and fast.
- The Worker wrapper itself is **host JS** (lives in `web-editor/`), not in this crate. The concrete integration is documented below.

---

## Design Decisions

- **`parking_lot` over `std::sync::RwLock` + cfg:** one code path for native and WASM; `std::sync` isn't available on `wasm32-unknown-unknown`.
- **`IndexMap` over `Vec<(K,V)>`:** keeps O(1) lookups on the hot `get`/`contains_key` paths while preserving order.
- **Snapshot-compare for dirty streams (TD-4) and full-pool snapshots for undo (TD-6):** both favour "can't silently corrupt" over minimal memory; per-page sessions and the small writer pool make the memory cost negligible.
- **Operator enum keeps bodies untouched (TD-3):** the conversion changed only `match` patterns, minimising regression risk for a maintainability-only change; exhaustiveness is now compiler-checked.
- **`from_token` (not `from_str`):** avoids `clippy::should_implement_trait` shadowing of `std::str::FromStr`.

---

## Test Coverage (new tests)

| Area | Tests |
|------|-------|
| TD-5 | `generation_bumps_on_add_and_set`, `snapshot_and_restore_roundtrip` |
| TD-6 | `undo_reverts_a_checkpointed_change`, `redo_replays_an_undone_change`, `checkpoint_clears_redo_history`, `undo_with_empty_history_is_noop` |
| TD-7 | `inline_image_unfiltered_data_may_contain_ei_bytes`, `inline_image_explicit_length_is_respected`, `inline_image_filtered_without_length_falls_back_to_ei_scan`, `find_jpeg_eoi_locates_marker`, strengthened `test_parse_inline_image` |
| TD-1 | `dict_preserves_insertion_order_not_alphabetical` |
| TD-9 | `test_from_name_strips_subset_prefix`, `test_subset_prefix_only_six_uppercase` |
| TD-2 | `pdf_document_is_send_and_sync`, `concurrent_reads_populate_caches_safely` |
| TD-4 | `is_signed_detects_sigflags`, `is_signed_false_without_sigflags` |
| TD-3 | `operator::tests::{unknown_token_is_none, known_tokens_map, as_str_round_trips_through_from_token}` |
| TD-8 | `pipeline_no_filters_returns_input_copy`, `pipeline_cow_borrows_when_unfiltered`, `pipeline_chains_filters_same_as_before` |

---

## TD-10 — Web Worker integration (host JS)

The Rust side is done; drop these into `web-editor/` to move parse/render/save
off the main thread. Render buffers transfer zero-copy via `Transferable`.

```js
// pdf-worker.js
import init, { WasmDocument, WasmEditor, WasmRenderer } from "./pkg/pdf_core.js";
let doc = null, editor = null;
self.onmessage = async ({ data: { id, method, args } }) => {
  try {
    await init();
    let result, transfer = [];
    switch (method) {
      case "parse":       doc = WasmDocument.parse(new Uint8Array(args.bytes)); break;
      case "open":        editor = WasmEditor.open(new Uint8Array(args.bytes)); break;
      case "renderPage": {
        const r = WasmRenderer.render_page(doc, args.page, args.scale);
        const buf = r.rgba_bytes();                 // Uint8Array (transferable)
        result = { width: r.width, height: r.height, data: buf.buffer };
        transfer = [buf.buffer];
        break;
      }
      case "save":        { const a = editor.save(); result = a.buffer; transfer = [a.buffer]; break; }
      case "isSigned":    result = editor.is_signed(); break;
      case "undo":        result = editor.undo(); break;
      case "redo":        result = editor.redo(); break;
    }
    self.postMessage({ id, result }, transfer);
  } catch (e) { self.postMessage({ id, error: String(e) }); }
};
```

```js
// pdf-client.js — async proxy on the main thread
export class PdfClient {
  #w = new Worker(new URL("./pdf-worker.js", import.meta.url), { type: "module" });
  #pending = new Map(); #n = 0;
  constructor() {
    this.#w.onmessage = ({ data: { id, result, error } }) => {
      const p = this.#pending.get(id); this.#pending.delete(id);
      error ? p.reject(new Error(error)) : p.resolve(result);
    };
  }
  #call(method, args, transfer = []) {
    return new Promise((res, rej) => {
      const id = ++this.#n; this.#pending.set(id, { resolve: res, reject: rej });
      this.#w.postMessage({ id, method, args }, transfer);
    });
  }
  renderPage(page, scale) { return this.#call("renderPage", { page, scale }); }
  save() { return this.#call("save"); }
  isSigned() { return this.#call("isSigned"); }
}
```

Only `parse`, `renderPage`, and `save` need offloading; the rest are fast.
For WASM **threads** (parallel tile rendering), TD-2 already makes
`PdfDocument: Sync`; the remaining work is `wasm-bindgen-rayon` +
`+atomics,+bulk-memory` + COOP/COEP headers — a separate spike.

---

## Known Limitations / Follow-up

- **TD-8 Tier 2 (mmap / `Cow` everywhere):** deferred. The on-demand-parse model returns owned objects, so borrowing decoded bytes for the whole API is a larger refactor. `apply_pipeline_cow` is in place for read-only callers; native `memmap2` behind `#[cfg(not(target_arch = "wasm32"))]` is the next step.
- **TD-9 broader metrics:** subset-prefix matching is done; using `/MissingWidth` as the default and a warn-on-fallback in the render-path font loader remain. The render path doesn't go through `PdfFontBuilder`, so that loader is a separate change.
- **TD-4 signatures:** we detect and warn; we do not implement incremental signature preservation (out of scope).
- **Pre-existing `clippy` warnings** (not introduced here): `edit_render_content_ops` unused without `wasm`/tests; `render_metrics` field unused without `render`; PI-approx in lexer/serializer tests; unused test imports in `write_edit.rs`; a borrow lint in `crypto/aes256`. Left untouched to keep this change scoped.
- **Parallel tile rendering** is now *unblocked* (TD-2) but not *implemented* — see the plan in the parallel-rendering discussion.
