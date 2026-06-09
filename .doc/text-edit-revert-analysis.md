# Text-Edit Write-Back — Revert-on-Click-Out Bug Analysis

**Date:** 2026-06-07  
**Symptom:** User edits text in a PDF block, clicks outside, text silently reverts to original.

---

## Complete Write-Back Pipeline

```
User types
  → onEditorInput / onEditorKeydown          (AnnotationOverlay.vue)
  → store.textEditInsert(ch)                 → WASM active_text_edit.engine updated
  → afterEngineEdit(block)
      → syncEngineState()                    → WASM text_edit_state() → editorText.value = st.text
      → scheduleGlyphRender()

User clicks SVG background / presses Enter
  → onOverlayClick → void commitBlockEdit()  ← NOT awaited (fire-and-forget)
  → commitBlockEdit():
      1. const newText = editorText.value     ← reads Vue ref synchronously
      2. const group   = selectedBlock.group  ← group.text = rustBlocks[i].text
      3. if (newText === group.text)           ← NO-OP GUARD
           → cancelBlockEdit(); return
      4. store.textEditCommit(rustId, pageIndex, svgBounds)
           a. history.pushSnapshot(editor.save(), ...)  ← undo snapshot (pre-edit bytes)
           b. WASM text_edit_commit(blockId):
               • active_text_edit is None           → committed:false  ← BUG PATH A
               • active_text_edit.block_id ≠ blockId → committed:false  ← BUG PATH B
               • encode_in_font(text, font)
                 → complete  → commit_block (marks dirty)
                             → flush_and_cache → commit_edit_session (writes writer pool)
                             → keep_model_current (model_gen = writer.generation())
                             → committed:true  ← SUCCESS
                 → incomplete → Tier-3 embed fallback
                               → fails → committed:false, missing=chars  ← BUG PATH C
      5. if committed:
           → commitRenderPage (canvas tile composite or full re-render)
           → cancelBlockEdit(keepCanvas=true)
           → rustBlocks[i].text = newText  (in-place, stable IDs)
      6. if NOT committed:
           → cancelBlockEdit()             ← VISUAL REVERT
           → if (missing) $q.notify(...)   ← only shown when missing is non-empty!
           → if (missing && text) replaceText fallback
```

---

## Root Causes

### Bug A — FULL-REBUILD after deletions desyncs block IDs (PRIMARY)

**Log evidence:**
```
[text_edit_commit] Tier-1 OK block_id=8 text="" enc_bytes=0 (model kept, gen=40)
[text_edit_enter]  FULL-REBUILD page=0 gen=46 prev_page=0 prev_gen=40
```

After blocks 6 and 8 are committed as empty, the WASM writer pool generation jumps from **40 → 46**
(+6, not +1). This fails the FAST-PATH guard in `text_edit.rs:94–96`:

```rust
let generation = self.editor.writer.generation();
if page_index == self.text_edit_page
    && self.text_edit_model_generation == generation  // 40 ≠ 46 → FULL-REBUILD
    && self.text_edit_model.is_some()
```

FULL-REBUILD renumbers all blocks (25 → 23). The WASM comment at `text_edit.rs:649–654` explicitly
names this as a known past bug: *"which is what desynced the FE's `selectedRustId` and silently
dropped subsequent commits."* `keep_model_current` was the fix for single-block Tier-1 commits,
but the gen is jumping by 6 — bypassing it.

**Likely gen-jump source:** `commitRenderPage` in `usePdfStore.ts` calls
`render_committed_block_tile` or `editorRenderPage`. One of these WASM paths may be writing to
the writer pool (triggering gen increments) even though it appears to be read-only.

**Impact:** After the FULL-REBUILD the FE correctly updates `rustBlocks` via `reenterForPage()`.
Block IDs stabilise. But the generation mismatch causes unnecessary model rebuilds and opens the
window for Bug B below.

---

### Bug B — Race between `void commitBlockEdit()` and a concurrent `reenterForPage()`

**Code location:** `AnnotationOverlay.vue:1201`

```javascript
// onOverlayClick — fire and forget
void commitBlockEdit()   // ← async, not awaited
```

`text_edit_enter` unconditionally resets `active_text_edit = None` on **every** call, including
the FAST-PATH (`text_edit.rs:99`):

```rust
// FAST-PATH
self.active_text_edit = None;   // ← always cleared
return Ok(blocks_to_json(&self.text_edit_blocks));
```

If the user clicks the SVG background at the same moment a block click event fires (double-click
generating two events, or touch-driven misfire), two code paths run concurrently in the async
event loop:

```
onOverlayClick  → void commitBlockEdit()   [async, not yet at WASM call]
onBlockClick    → openEditor
                    → reenterForPage()
                        → text_edit_enter  → active_text_edit = None  ← clears session!
                    → openEngineForBlock
                        → text_edit_open   → active_text_edit = Some(new block)
```

When the fire-and-forget `commitBlockEdit` finally reaches `textEditCommit`, the active session
belongs to the NEW block, not the one being committed → `active_text_edit.block_id ≠ rustId`
→ `committed:false` → silent revert.

---

### Bug C — Silent revert when Tier-3 fails with `missing: ""`

**Code location:** `AnnotationOverlay.vue:1088` and `text_edit.rs:688–699`

When Tier-1 encoding is incomplete AND Tier-3 bundled-font embed also fails, WASM returns:
```json
{"committed": false, "missing": ""}
```

The JS side only shows a warning `if (missing)` — an empty string is falsy, so **no warning**:

```javascript
cancelBlockEdit()
if (missing) {   // ← "" is falsy → notification suppressed
  $q.notify({ type: 'warning', message: "Can't save these characters..." })
}
```

The text reverts with zero feedback.

All blocks in this PDF use composite CID fonts (F1: 92 CID entries, F2: 76 CID entries). Any
character the user types that is not in the font's reverse CMap will fail Tier-1. Tier-3 attempts
a bundled-font embed; if the font resolver can't find a matching face (or the `render` feature
is not enabled in the WASM build), it returns `Ok(false)` → the `missing` field carries the
unencodable chars from Tier-1, which may be non-empty. But if Tier-3 fails for a different reason
the field can be `""`.

---

### Bug D — In-place `rustBlocks` update silently skipped when `liveWidthPts = 0`

**Code location:** `AnnotationOverlay.vue:~1083`

```javascript
} else if (newWidthPts > 0) {   // ← 0 width skips the update silently
  updated[i] = { ...updated[i], text: newText, width: newWidthPts }
}
```

If `syncEngineState` returned `st.width = 0` (e.g., composite font block where the advance-width
calculation returns 0), the commit succeeds in WASM and the PDF is correct, but `rustBlocks[i].text`
stays stale until the next `reenterForPage()`. Not a text-revert bug, but causes a stale hit-rect.

---

## Fixes

### Fix 1 — Don't clear `active_text_edit` on FAST-PATH (Rust)

**File:** `pdf-editor-rust-core/src/wasm/text_edit.rs` ~line 99

```rust
// BEFORE (clears active session even when reusing the model)
self.active_text_edit = None;
return Ok(blocks_to_json(&self.text_edit_blocks));

// AFTER (preserve in-flight session on FAST-PATH — the JS will re-open it anyway)
// active_text_edit stays as-is; text_edit_open resets it when the host opens a block.
return Ok(blocks_to_json(&self.text_edit_blocks));
```

This makes a re-entry on the same page with the same gen non-destructive to the active session.

### Fix 2 — Await `commitBlockEdit` in `onOverlayClick` (Vue)

**File:** `web-editor/src/components/AnnotationOverlay.vue` line 1201

```javascript
// BEFORE
void commitBlockEdit()

// AFTER
await commitBlockEdit()
```

`onOverlayClick` is already called in an async context (Vue event handler). Awaiting the commit
ensures no other async operations can interleave until the commit completes.

### Fix 3 — Show diagnostic on any `committed:false` (Vue)

**File:** `web-editor/src/components/AnnotationOverlay.vue` ~line 1088

```javascript
// BEFORE
cancelBlockEdit()
if (missing) {
  $q.notify({ type: 'warning', message: `Can't save these characters: ${missing}` })
}

// AFTER
cancelBlockEdit()
if (missing) {
  $q.notify({ type: 'warning', message: `Can't save these characters: ${missing}` })
} else {
  // committed:false but no missing chars — encoding or session failure
  $q.notify({ type: 'warning', message: 'Could not save edit — please try again' })
}
```

### Fix 4 — Diagnose the gen jump (investigation)

**File:** `pdf-editor-rust-core/src/wasm/text_edit.rs` `flush_and_cache` and/or `usePdfStore.ts`
`textEditCommit` / `commitRenderPage`

Add before/after gen logging around every writer pool write to identify what increments the gen
from 40 → 46 after block 8's commit. Likely candidate: the WASM `render_committed_block_tile`
or `render_page` path modifying a pool entry as a side effect of the preload/cache mechanism.

---

## Verification Steps

1. Delete two blocks (reproduce FULL-REBUILD scenario from logs)
2. After FULL-REBUILD, edit a remaining block's text and click outside
3. Confirm `[text_edit_commit] Tier-1 OK` appears (not `committed:false`)
4. Confirm the canvas shows the new text after the page re-renders
5. Save the PDF and reopen — new text must persist
6. Run `cargo test && cargo build --target wasm32-unknown-unknown` after Rust changes
