# fix-register-page-font-indirect-resources — Implementation Report

**Date:** 2026-06-05
**Scope:** `editor/text_commit.rs` — `register_page_font`

## What Was Implemented

- Fixed `register_page_font` to resolve indirect `/Resources` and `/Font` PDF object
  references through the CoW editor chain before modifying them.
- Added `indirect_resources_pdf()` test helper that builds a minimal PDF where the page's
  `/Resources` entry is an indirect object reference (object 6).
- Added `register_page_font_preserves_existing_fonts_with_indirect_resources` test (behind
  `#[cfg(feature = "render")]`) verifying that the original F1 font is preserved alongside
  the newly registered Ed0 after the fix.

## Design Decisions

- **Resolve through `editor.get_object(id)`** — uses the CoW writer pool first, then falls
  back to the original doc. This is the correct read path for any object that may have been
  previously overridden by the writer.
- **Inline the resolved dict back into the page** — we write the (now-mutated) Resources dict
  inline in the page dict override. This is intentional: the writer pool stores the new
  version without touching the original PDF's indirect Resources object. Semantically
  equivalent to the PDF spec requirement.
- **Two levels fixed** — both `/Resources` and `/Font` within it may be indirect references
  in real PDFs; both match arms were extended with the `Reference` case.

## Test Coverage

- `register_page_font_preserves_existing_fonts_with_indirect_resources` (happy-path):
  opens a PDF with indirect Resources, registers a bundled CID font, asserts that the
  original F1 key is still present and Ed0 was added.

## Known Limitations / Follow-up

- If `/Resources` is **inherited** from a parent Pages node (not present in the page dict at
  all) and a font needs to be registered, the current code still creates an empty dict. This
  is an uncommon PDF structure and was out of scope for this fix.
- The bold/italic **visual preview** (using the embedded Liberation Sans variant) still
  depends on the bundled font resolver finding a matching face for the original CID font's
  PostScript name. When no BoldItalic face is found, the preview falls back to the original
  font rendering (correct text, no visual bold/italic), which is acceptable. A follow-up
  could try the Bold-only cached font as a BoldItalic fallback.
