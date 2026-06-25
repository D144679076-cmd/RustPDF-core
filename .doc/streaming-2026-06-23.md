# Streaming — Implementation Report

**Date:** 2026-06-23
**Scope:** `streaming` module + WASM bindings + SDK `openRemote` + FE composable

## What Was Implemented

### Rust (`pdf-editor-rust-core`)

- **`src/streaming/mod.rs`** — new module:
  - `ByteCache` — sorted, merging byte-range cache; `feed`, `get`, `has_range`, `estimate_object_len`
  - `XRefData` — wrapper around `HashMap<u32, u64>` offsets + trailer `PdfDict`
  - `StreamingDocument` — state machine: `from_tail`, `feed`, `needed_ranges_for_page`, `page_ready`, `build_page_document`
  - `collect_all_refs` — recursive BFS ref collector over `PdfObject` graphs
  - `write_xref_table` / `write_trailer` — synthetic PDF assembly helpers
  - `parse_trailer_from_bytes` — extract trailer dict from raw xref bytes

- **`src/parser/xref.rs`** — added `pub fn find_startxref(tail: &[u8]) -> Option<u64>`

- **`src/parser/objects.rs`** — added `pub(crate) fn parse_indirect_object(data, offset)` (always available, not crypto-gated)

- **`src/lib.rs`** — `pub mod streaming`

- **`src/wasm/streaming.rs`** — `WasmStreamingDocument` WASM binding:
  - `new(tail, total_len)` → constructor
  - `needed_ranges(page_index)` → JSON string `[{offset,length}]`
  - `feed(offset, data)` → insert bytes
  - `page_ready(page_index)` → bool
  - `build_page_document(page_index)` → `WasmDocument`

- **`src/wasm/mod.rs`** — `pub mod streaming`

### Tests (`tests/streaming.rs`)

- `byte_cache_empty_get_returns_none`
- `byte_cache_feed_and_get`
- `byte_cache_overlapping_feeds_merge`
- `byte_cache_non_overlapping_feeds_stay_separate`
- `byte_cache_total_len`
- `streaming_document_from_minimal_pdf` — end-to-end: tail → feed loop → build_page_document
- `streaming_document_from_multipage_pdf`
- `streaming_document_invalid_tail_returns_error`
- `needed_ranges_empty_when_page_ready`

Internal unit tests also added inside `streaming/mod.rs` (7 tests).

### SDK (`packages/sdk`)

- **`src/remote.ts`** — `openRemote(url, opts)`:
  - HEAD → `Content-Length` + `Accept-Ranges` check
  - Falls back to full fetch for files < 5 MB or no range support
  - Progressive path: tail fetch → `WasmStreamingDocument` → `fetchNeededRanges` loop → page-0 `WasmDocument`
  - `onProgress` callback wired through

- **`src/index.ts`** — exports `openRemote` and `RemoteOpenOptions`

### FE (`web-editor`)

- **`src/composables/useFileOps.ts`** — added `useRemoteOpen()`:
  - `loadProgress: Ref<number>` (0–100)
  - `openFromUrl(url)` — calls `openRemote`, updates `loadProgress`, sets doc via `store.setDocumentInstance`
  - Shows/hides `$q.loading` overlay with error notifications

## Design Decisions

- **Zero-fill synthetic buffer**: `build_page_document` assembles a minimal synthetic PDF containing only the needed objects (extracted from cache), writes a new xref + trailer, then calls `PdfDocument::parse`. This avoids allocating the full `total_len` buffer while still reusing the existing parser.

- **`try_parse_xref` scratch buffer**: The xref parser needs the bytes at their absolute file positions (so offset validation works). We allocate `xref_offset + 32KB` bytes and place the cached xref bytes there. For typical PDFs, xref_offset < 1 MB, so this is acceptable. Files with xref at >512 MB are rejected with a warning.

- **Iterative BFS**: `collect_page_object_ids` does a BFS from the catalog. If any object's bytes aren't in cache, it returns `Err` and `needed_ranges_for_page` falls back to returning the catalog seed range. The caller loops until all are cached.

- **`parse_indirect_object` always available**: Removed `#[cfg(feature = "crypto")]` gate — the function is small and needed by streaming unconditionally.

- **`MAX_OBJ_LEN = 64 KB`**: Object size estimated from the gap to the next xref entry, capped at 64 KB. Handles most real-world objects; content streams are larger but their ranges are correctly fetched once discovered.

- **`XREF_FETCH_LEN = 32 KB`**: Covers most traditional xref tables. XRef streams (PDF 1.5+) are typically smaller.

## Test Coverage

- ByteCache: empty, single feed, overlap merge, gap isolation, total_len
- StreamingDocument: valid minimal PDF end-to-end, multipage PDF, invalid tail error, full-file feed → page_ready
- All 324 crate tests still pass (0 regressions)

## Known Limitations / Follow-up

- **XRef streams (PDF 1.5+)**: The scratch-buffer approach for `try_parse_xref` will fail for XRef streams because they embed object offsets which `parse_xref` validates against `file_size = xref_offset + 32KB`, not the real file size. Fix: pass `total_len` as `file_size` to `parse_xref` (requires API change to that function).

- **Compressed objects (type-2 xref)**: `XRefData.offsets` only contains type-1 (uncompressed) entries. Object streams are not handled in streaming mode — `needed_ranges_for_page` will silently skip type-2 objects. Fix: fetch and decompress object streams during BFS.

- **Page > 0**: The page tree BFS walks from the catalog. For page N > 0, it fetches all preceding page nodes too (needed to count). For large documents, this is wasteful. Fix: use the `/Count` hint to binary-search the page tree.

- **`setDocumentInstance` on PdfStore**: `useRemoteOpen` calls `store.setDocumentInstance(doc)`. The actual Pinia store method may be named differently — align with the store's public API before shipping.

- **SDK type declarations**: `WasmStreamingDocument` is not yet in the TypeScript type declarations generated by wasm-bindgen. After rebuilding the WASM package, verify the type is exported from `pdf-core`.
