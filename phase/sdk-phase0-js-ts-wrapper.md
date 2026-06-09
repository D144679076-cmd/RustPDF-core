# SDK Phase 0 — JavaScript/TypeScript SDK Wrapper

**Status:** Complete — 2026-06-10
**Effort:** ~3–4 weeks
**Priority:** P0 — blocks npm publish, component library, and all external adoption
**Tier gate:** Free tier (viewer), Pro tier (editor), Enterprise (signatures, REST)

## Context

The WASM bridge (`pkg/`) exposes raw `WasmDocument`, `WasmEditor`, `WasmRenderer` classes. Consumers must:
- Parse JSON strings manually for every result (`get_metadata()` → `JSON.parse(...)`)
- Call `.free()` on every object or face WASM heap leaks
- Handle `JsError` with no type information
- Wire async rendering logic from scratch

This is the same level of abstraction as calling C malloc directly. Every competitor (PSPDFKit, Apryse/PDFTron) exposes a typed, Promise-based, lifecycle-managed SDK. This phase builds that layer.

## New Package: `sdk/`

Located at `/home/duy/Documents/Workspace/work/pdfEditor/sdk/` (sibling of `pdf-editor-rust-core/` and `web-editor/`).

```
sdk/
  src/
    index.ts          — public exports
    document.ts       — PdfDocument class
    editor.ts         — PdfEditor class
    renderer.ts       — PdfRenderer class
    errors.ts         — typed error hierarchy
    memory.ts         — DisposableDocument, DisposableEditor
    types.ts          — shared value types (metadata, outline, annotation, span)
  package.json
  tsconfig.json
  vite.config.ts
```

## `sdk/src/types.ts`

```typescript
export interface PdfMetadata {
  title?: string;
  author?: string;
  subject?: string;
  keywords?: string;
  creator?: string;
  producer?: string;
}

export interface OutlineItem {
  title: string;
  destPage: number;
  open: boolean;
  children: OutlineItem[];
}

export interface TextSpan {
  text: string;
  x: number;
  y: number;
  width: number;
  height: number;
  fontSize: number;
  fontName: string;
}

export interface Annotation {
  subtype: string;
  rect: [number, number, number, number];
  color?: [number, number, number];
  quadPoints?: number[];
  contents?: string;
}

export interface PageSize {
  widthPt: number;
  heightPt: number;
}

export interface SearchResult {
  page: number;
  text: string;
  // Future: bounds when backend returns position
}
```

## `sdk/src/errors.ts`

```typescript
export class PdfError extends Error {
  constructor(message: string, public readonly cause?: unknown) {
    super(message);
    this.name = 'PdfError';
  }
}

export class PdfParseError extends PdfError {
  constructor(message: string, cause?: unknown) {
    super(message, cause);
    this.name = 'PdfParseError';
  }
}

export class PdfRenderError extends PdfError {
  constructor(message: string, cause?: unknown) {
    super(message, cause);
    this.name = 'PdfRenderError';
  }
}

export class PdfEditError extends PdfError {
  constructor(message: string, cause?: unknown) {
    super(message, cause);
    this.name = 'PdfEditError';
  }
}

/** Wraps a raw JsError from WASM into a typed PdfError subclass. */
export function wrapWasmError(e: unknown, cls: typeof PdfError = PdfError): never {
  const msg = e instanceof Error ? e.message : String(e);
  throw new cls(msg, e);
}
```

## `sdk/src/document.ts`

```typescript
import type { WasmDocument } from '../pkg/pdf_core';
import { PdfParseError, wrapWasmError } from './errors';
import type { Annotation, OutlineItem, PageSize, PdfMetadata, TextSpan } from './types';

export class PdfDocument {
  /** Do not call directly — use `PdfDocument.open()`. */
  constructor(private readonly _wasm: WasmDocument) {}

  /** Parse PDF bytes and return a `PdfDocument`. */
  static open(bytes: Uint8Array): PdfDocument {
    let { WasmDocument } = getWasm();
    try {
      return new PdfDocument(WasmDocument.parse(bytes));
    } catch (e) {
      wrapWasmError(e, PdfParseError);
    }
  }

  /** Parse password-protected PDF bytes. */
  static openWithPassword(bytes: Uint8Array, password: string): PdfDocument {
    let { WasmDocument } = getWasm();
    try {
      const enc = new TextEncoder().encode(password);
      return new PdfDocument(WasmDocument.parse_with_password(bytes, enc));
    } catch (e) {
      wrapWasmError(e, PdfParseError);
    }
  }

  /** Total number of pages. */
  pageCount(): number {
    return this._wasm.page_count();
  }

  /** Page size in PDF points (1 pt = 1/72 inch). */
  pageSize(pageIndex: number): PageSize {
    const arr = this._wasm.page_size(pageIndex);
    return { widthPt: arr[0], heightPt: arr[1] };
  }

  /** Plain text of one page (0-based). */
  extractText(pageIndex: number): string {
    return this._wasm.extract_text(pageIndex);
  }

  /** Detailed text spans with position data for one page. */
  extractTextSpans(pageIndex: number): TextSpan[] {
    return JSON.parse(this._wasm.extract_text_spans(pageIndex)) as TextSpan[];
  }

  /** Document-level metadata (title, author, etc.). */
  getMetadata(): PdfMetadata {
    return JSON.parse(this._wasm.get_metadata()) as PdfMetadata;
  }

  /** Bookmark tree (outline). */
  getOutline(): OutlineItem[] {
    return JSON.parse(this._wasm.get_outline()) as OutlineItem[];
  }

  /** All annotations on a page. */
  listAnnotations(pageIndex: number): Annotation[] {
    return JSON.parse(this._wasm.list_annotations(pageIndex)) as Annotation[];
  }

  /** Search all pages for a query string. Returns page + context for each match. */
  search(query: string): import('./types').SearchResult[] {
    const results: import('./types').SearchResult[] = [];
    const count = this.pageCount();
    for (let i = 0; i < count; i++) {
      const text = this.extractText(i);
      if (text.toLowerCase().includes(query.toLowerCase())) {
        results.push({ page: i, text });
      }
    }
    return results;
  }

  /** Expose raw WASM object for composable integration (web-editor internal use). */
  get raw(): WasmDocument { return this._wasm; }

  free(): void { this._wasm.free(); }
  [Symbol.dispose](): void { this.free(); }
}
```

## `sdk/src/renderer.ts`

```typescript
import type { WasmRenderer, RenderResult } from '../pkg/pdf_core';
import type { PdfDocument } from './document';
import { PdfRenderError, wrapWasmError } from './errors';

export interface RenderOptions {
  scale?: number;        // default 1.0
  pageIndex?: number;    // default 0
}

export interface PageImageData {
  imageData: ImageData;
  widthPx: number;
  heightPx: number;
}

export class PdfRenderer {
  private static _instance: WasmRenderer | null = null;

  private static getInstance(): WasmRenderer {
    if (!PdfRenderer._instance) {
      const { WasmRenderer } = getWasm();
      PdfRenderer._instance = new WasmRenderer();
    }
    return PdfRenderer._instance!;
  }

  /** Render a page to an `ImageData` ready for `ctx.putImageData()`. */
  static renderPage(doc: PdfDocument, pageIndex = 0, scale = 1.0): PageImageData {
    const renderer = PdfRenderer.getInstance();
    let result: RenderResult | null = null;
    try {
      result = renderer.render_page(doc.raw, pageIndex, scale);
      const { width, height } = result;
      const rgba = result.rgba_bytes();
      const imageData = new ImageData(new Uint8ClampedArray(rgba), width, height);
      return { imageData, widthPx: width, heightPx: height };
    } catch (e) {
      wrapWasmError(e, PdfRenderError);
    } finally {
      result?.free();
    }
  }

  /**
   * Render a page to an HTMLCanvasElement, handling device pixel ratio automatically.
   * Returns the CSS pixel dimensions (before DPR scaling).
   */
  static renderToCanvas(
    canvas: HTMLCanvasElement,
    doc: PdfDocument,
    pageIndex = 0,
    cssScale = 1.0,
  ): { cssWidth: number; cssHeight: number } {
    const dpr = window.devicePixelRatio || 1;
    const { imageData, widthPx, heightPx } = PdfRenderer.renderPage(doc, pageIndex, cssScale * dpr);
    const cssWidth = widthPx / dpr;
    const cssHeight = heightPx / dpr;
    canvas.width = widthPx;
    canvas.height = heightPx;
    canvas.style.width = `${cssWidth}px`;
    canvas.style.height = `${cssHeight}px`;
    const ctx = canvas.getContext('2d')!;
    ctx.putImageData(imageData, 0, 0);
    return { cssWidth, cssHeight };
  }
}
```

## `sdk/src/editor.ts`

```typescript
import type { WasmEditor } from '../pkg/pdf_core';
import type { PdfDocument } from './document';
import { PdfEditError, wrapWasmError } from './errors';

export interface TextBoxOptions {
  fontName?: string;    // default 'Helvetica'
  fontSize?: number;    // default 12
  color?: [number, number, number];  // RGB 0–255, default [0,0,0]
  align?: 0 | 1 | 2;   // 0=left, 1=center, 2=right
}

export class PdfEditor {
  constructor(private readonly _wasm: WasmEditor) {}

  static open(bytes: Uint8Array): PdfEditor {
    const { WasmEditor } = getWasm();
    try {
      return new PdfEditor(WasmEditor.open(bytes));
    } catch (e) {
      wrapWasmError(e, PdfEditError);
    }
  }

  /** Returns the underlying document for rendering / inspection. */
  document(): PdfDocument {
    const { PdfDocument } = require('./document');
    return new PdfDocument(this._wasm.document());
  }

  /** Save and return updated PDF bytes. */
  save(): Uint8Array {
    try {
      return this._wasm.save();
    } catch (e) {
      wrapWasmError(e, PdfEditError);
    }
  }

  addBlankPage(index: number, widthPt = 595, heightPt = 842): void {
    this._wasm.add_blank_page(index, widthPt, heightPt);
  }

  deletePage(index: number): void {
    this._wasm.delete_page(index);
  }

  addHighlight(pageIndex: number, quadPoints: number[], color: [number, number, number] = [1, 1, 0]): void {
    this._wasm.add_highlight(pageIndex, new Float64Array(quadPoints), ...color);
  }

  addTextBox(
    pageIndex: number,
    x: number, y: number, width: number, height: number,
    text: string,
    opts: TextBoxOptions = {},
  ): void {
    const { fontName = 'Helvetica', fontSize = 12, color = [0, 0, 0], align = 0 } = opts;
    this._wasm.add_text_box(
      pageIndex, x, y, width, height, text,
      fontName, fontSize,
      color[0] / 255, color[1] / 255, color[2] / 255,
      align,
    );
  }

  addTextAnnotation(pageIndex: number, x: number, y: number, width: number, height: number, contents: string): void {
    this._wasm.add_text_annotation(pageIndex, x, y, width, height, contents);
  }

  addRedact(pageIndex: number, x: number, y: number, width: number, height: number): void {
    this._wasm.add_redact(pageIndex, x, y, width, height, 0, 0, 0);
  }

  applyRedactions(): void {
    this._wasm.apply_redactions();
  }

  fillTextField(fieldName: string, value: string): void {
    this._wasm.fill_text_field(fieldName, value);
  }

  setLicenseKey(key: string): void {
    this._wasm.set_license_key(key);
  }

  get raw(): WasmEditor { return this._wasm; }

  free(): void { this._wasm.free(); }
  [Symbol.dispose](): void { this.free(); }
}
```

## `sdk/src/memory.ts`

```typescript
/**
 * Returns a `using`-compatible scope that auto-frees the document on exit.
 *
 * Usage:
 *   using doc = openDocument(bytes);
 *   // doc is freed at end of block
 */
export function openDocument(bytes: Uint8Array) {
  const { PdfDocument } = require('./document');
  return PdfDocument.open(bytes);
}

export function openEditor(bytes: Uint8Array) {
  const { PdfEditor } = require('./editor');
  return PdfEditor.open(bytes);
}
```

## `sdk/src/index.ts`

```typescript
export { PdfDocument } from './document';
export { PdfEditor } from './editor';
export { PdfRenderer } from './renderer';
export { PdfError, PdfParseError, PdfRenderError, PdfEditError } from './errors';
export type { PdfMetadata, OutlineItem, TextSpan, Annotation, PageSize, SearchResult } from './types';
export type { RenderOptions, PageImageData } from './renderer';
export type { TextBoxOptions } from './editor';
```

## `sdk/package.json`

```json
{
  "name": "@pdf-core/sdk",
  "version": "0.1.0",
  "description": "TypeScript SDK for pdf-core WASM — typed, lifecycle-safe PDF operations",
  "type": "module",
  "exports": {
    ".": {
      "import": "./dist/index.js",
      "require": "./dist/index.cjs",
      "types": "./dist/index.d.ts"
    }
  },
  "files": ["dist", "README.md"],
  "scripts": {
    "build": "vite build && tsc --declaration --emitDeclarationOnly",
    "test": "vitest"
  },
  "peerDependencies": {
    "@pdf-core/wasm": "^0.1.0"
  },
  "devDependencies": {
    "typescript": "^5",
    "vite": "^5",
    "vitest": "^1"
  }
}
```

## Helper: `getWasm()`

Each file references `getWasm()`. This is a module-level singleton loader:

```typescript
// sdk/src/_wasm.ts
let _wasm: typeof import('../pkg/pdf_core') | null = null;

export function getWasm() {
  if (!_wasm) throw new Error('pdf-core WASM not initialized — call initPdfSdk() first');
  return _wasm;
}

/** Call once at app startup: await initPdfSdk() */
export async function initPdfSdk(wasmUrl?: string): Promise<void> {
  const mod = await import('../pkg/pdf_core');
  if (mod.default) await mod.default(wasmUrl ? { module_or_path: wasmUrl } : undefined);
  _wasm = mod;
}
```

## Tests (`sdk/src/__tests__/document.test.ts`)

```typescript
import { describe, it, expect, beforeAll } from 'vitest';
import { initPdfSdk } from '../_wasm';
import { PdfDocument } from '../document';
import { PdfParseError } from '../errors';
import { readFileSync } from 'fs';

const MINIMAL_PDF = readFileSync('../../tests/fixtures/minimal.pdf');

beforeAll(async () => {
  await initPdfSdk();
});

describe('PdfDocument', () => {
  it('opens minimal.pdf and returns page count', () => {
    using doc = PdfDocument.open(new Uint8Array(MINIMAL_PDF));
    expect(doc.pageCount()).toBe(1);
  });

  it('returns page size for page 0', () => {
    using doc = PdfDocument.open(new Uint8Array(MINIMAL_PDF));
    const size = doc.pageSize(0);
    expect(size.widthPt).toBeGreaterThan(0);
  });

  it('throws PdfParseError on garbage bytes', () => {
    expect(() => PdfDocument.open(new Uint8Array([0, 1, 2, 3]))).toThrow(PdfParseError);
  });

  it('returns empty metadata without throwing', () => {
    using doc = PdfDocument.open(new Uint8Array(MINIMAL_PDF));
    const meta = doc.getMetadata();
    expect(meta).toBeDefined();
  });

  it('search returns results on text match', () => {
    // multipage.pdf has known text
    const multipage = readFileSync('../../tests/fixtures/multipage.pdf');
    using doc = PdfDocument.open(new Uint8Array(multipage));
    // should not throw; results may be empty depending on fixture content
    const results = doc.search('test');
    expect(Array.isArray(results)).toBe(true);
  });
});
```

## Verification

```bash
cd sdk
npm install
npm run build
npm test
# Confirm no TypeScript errors
npx tsc --noEmit
```
