.PHONY: wasm wasm-no-render wasm-viewer wasm-sdk check-size

WASM_PACK := $(shell which wasm-pack 2>/dev/null || echo ~/.cargo/bin/wasm-pack)

# ── Development builds (web-editor) ──────────────────────────────────────────

wasm:
	$(WASM_PACK) build --target web -d ../web-editor/src/pkg \
	  -- --features wasm-render --no-default-features

wasm-no-render:
	$(WASM_PACK) build --target web -d ../web-editor/src/pkg \
	  -- --features wasm --no-default-features

# ── Publishable package builds ────────────────────────────────────────────────

# Read-only viewer build → packages/viewer/pkg/
wasm-viewer:
	$(WASM_PACK) build --target web \
	  --out-dir ../packages/viewer/pkg \
	  --out-name pdf_core_viewer \
	  -- --features wasm-viewer --no-default-features

# Full SDK build → packages/sdk/pkg/
wasm-sdk:
	$(WASM_PACK) build --target web \
	  --out-dir ../packages/sdk/pkg \
	  --out-name pdf_core \
	  -- --features wasm-render --no-default-features

# ── Size verification ─────────────────────────────────────────────────────────
# Brotli targets: viewer < 500 KB compressed, sdk < 1.2 MB compressed.

check-size:
	@echo "=== viewer ==="
	@ls -lh ../packages/viewer/pkg/*.wasm
	@echo "=== sdk ==="
	@ls -lh ../packages/sdk/pkg/*.wasm
	@command -v brotli >/dev/null 2>&1 && { \
	  echo "--- brotli compressed sizes ---"; \
	  brotli --quality=11 ../packages/viewer/pkg/pdf_core_viewer_bg.wasm -c | wc -c; \
	  brotli --quality=11 ../packages/sdk/pkg/pdf_core_bg.wasm -c | wc -c; \
	} || echo "(brotli not installed — skipping compressed size check)"
