# SDK Phase 0 — npm Package Publication

**Status:** Complete — 2026-06-10
**Effort:** ~1–2 weeks
**Priority:** P0 — required before any external developer can install the SDK
**Depends on:** `sdk-phase0-js-ts-wrapper.md` (SDK wrapper must exist)

## Context

The WASM output currently lives in `pdf-editor-rust-core/pkg/` — a local directory only usable within this monorepo. No external developer can install it. All competitors publish to npm: `@pspdfkit/web`, `pdfjs-dist`, `@pdftron/webviewer`. This phase establishes the publishing pipeline and sets the package name/shape.

## Package Structure

Two published packages:

| Package | Features | WASM binary | Target use |
|---|---|---|---|
| `@pdf-core/viewer` | parse, render, text extract, search | ~1.8 MB (wasm without writer/crypto) | Read-only embedding |
| `@pdf-core/sdk` | all features (viewer + edit + forms + crypto) | ~3.7 MB | Full editor use |

Both are ES modules with TypeScript declarations. `@pdf-core/sdk` re-exports everything from `@pdf-core/viewer` plus editor/writer APIs.

## Step 1 — Build Variants in `Cargo.toml`

The `wasm-pack` builds produce the WASM binary. Add a viewer-only feature profile:

```toml
[features]
# existing
wasm        = ["dep:wasm-bindgen", ..., "writer", "forms"]
wasm-render = ["wasm", "render"]

# new: viewer build — no writer, no forms, no crypto; ~half the binary
wasm-viewer = ["dep:wasm-bindgen", "dep:js-sys", "dep:console_log",
               "dep:console_error_panic_hook", "render"]
```

In `src/wasm/mod.rs` guard editor/writer WASM exports behind `#[cfg(feature = "wasm")]` vs `#[cfg(feature = "wasm-viewer")]`.

## Step 2 — `Makefile` (or `justfile`) Build Targets

```makefile
.PHONY: wasm-viewer wasm-sdk

# Read-only viewer build → packages/viewer/pkg/
wasm-viewer:
	wasm-pack build pdf-editor-rust-core \
	  --target web \
	  --out-dir ../packages/viewer/pkg \
	  --out-name pdf_core_viewer \
	  -- --features wasm-viewer,render \
	     --no-default-features

# Full SDK build → packages/sdk/pkg/
wasm-sdk:
	wasm-pack build pdf-editor-rust-core \
	  --target web \
	  --out-dir ../packages/sdk/pkg \
	  --out-name pdf_core \
	  -- --features wasm-render,crypto,forms \
	     --no-default-features

# Verify WASM sizes
check-size:
	@echo "viewer:" && ls -lh packages/viewer/pkg/*.wasm
	@echo "sdk:" && ls -lh packages/sdk/pkg/*.wasm
	@# Brotli targets: viewer < 500 KB, sdk < 1.2 MB
	@brotli --quality=11 packages/viewer/pkg/pdf_core_viewer_bg.wasm -c | wc -c
	@brotli --quality=11 packages/sdk/pkg/pdf_core_bg.wasm -c | wc -c
```

## Step 3 — `packages/viewer/package.json`

```json
{
  "name": "@pdf-core/viewer",
  "version": "0.1.0",
  "description": "Fast PDF viewer — parse, render, search. Read-only. WASM-powered.",
  "type": "module",
  "exports": {
    ".": {
      "import": "./pkg/pdf_core_viewer.js",
      "types":  "./pkg/pdf_core_viewer.d.ts"
    }
  },
  "files": ["pkg"],
  "sideEffects": false,
  "keywords": ["pdf", "wasm", "viewer", "rust"],
  "license": "UNLICENSED"
}
```

## Step 4 — `packages/sdk/package.json`

```json
{
  "name": "@pdf-core/sdk",
  "version": "0.1.0",
  "description": "Full-featured PDF SDK — view, edit, annotate, forms, crypto. WASM-powered.",
  "type": "module",
  "exports": {
    ".": {
      "import": "./src/index.js",
      "types":  "./dist/index.d.ts"
    },
    "./wasm": {
      "import": "./pkg/pdf_core.js",
      "types":  "./pkg/pdf_core.d.ts"
    }
  },
  "files": ["src", "dist", "pkg"],
  "sideEffects": false,
  "keywords": ["pdf", "wasm", "editor", "annotations", "forms", "rust"],
  "license": "UNLICENSED"
}
```

## Step 5 — GitHub Actions Publish Workflow

File: `.github/workflows/publish.yml`

```yaml
name: Publish npm packages

on:
  push:
    tags:
      - 'v*'   # Trigger on v0.1.0, v0.2.0, etc.

jobs:
  build-and-publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust + wasm-pack
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: wasm32-unknown-unknown
      - run: cargo install wasm-pack

      - name: Install Brotli (size check)
        run: sudo apt-get install -y brotli

      - uses: actions/setup-node@v4
        with:
          node-version: 20
          registry-url: 'https://registry.npmjs.org'

      - name: Build WASM variants
        run: |
          make wasm-viewer
          make wasm-sdk
          make check-size

      - name: Publish @pdf-core/viewer
        run: npm publish packages/viewer --access public
        env:
          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}

      - name: Publish @pdf-core/sdk
        run: npm publish packages/sdk --access public
        env:
          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}
```

## Step 6 — CDN Snippet (jsDelivr, no install)

Add to README.md:

```html
<!-- Viewer only (smallest, read-only) -->
<script type="module">
  import init, { WasmDocument, WasmRenderer }
    from 'https://cdn.jsdelivr.net/npm/@pdf-core/viewer@0.1.0/pkg/pdf_core_viewer.js';
  await init();
  // use WasmDocument.parse(bytes), WasmRenderer...
</script>

<!-- Full SDK -->
<script type="module">
  import init, { WasmEditor }
    from 'https://cdn.jsdelivr.net/npm/@pdf-core/sdk@0.1.0/pkg/pdf_core.js';
  await init();
</script>
```

## Step 7 — CHANGELOG Automation

Use [conventional-changelog](https://github.com/conventional-changelog/conventional-changelog-cli):

```bash
npx conventional-changelog-cli -p angular -i CHANGELOG.md -s
```

Add to publish workflow after the build step:
```yaml
- name: Generate changelog entry
  run: npx conventional-changelog-cli -p angular -i CHANGELOG.md -s -r 1
- name: Commit changelog (if not a tag push)
  if: github.ref_type != 'tag'
  run: |
    git config user.email "ci@github.com"
    git config user.name "CI"
    git add CHANGELOG.md && git commit -m "docs: update changelog" || true
```

## Verification

```bash
# Local smoke test — build and check size
make wasm-sdk
ls -lh pdf-editor-rust-core/pkg/*.wasm

# Check package.json exports are valid
node --input-type=module <<'EOF'
import * as mod from './packages/sdk/pkg/pdf_core.js';
console.log(Object.keys(mod));
EOF

# Dry-run publish (no actual upload)
npm publish packages/sdk --dry-run
npm publish packages/viewer --dry-run
```
