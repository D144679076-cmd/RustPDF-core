# ONLYOFFICE PDF Editor — How It Edits a PDF File

> Reference study only. No code or design from this system may be copied into
> the commercial Rust rebuild. See REBUILD_PLAN.md §L1 and §L2.

---

## The Core Idea: Two Systems Working Together

ONLYOFFICE never truly "edits" the PDF in place. It runs two completely
separate systems simultaneously:

| System | Role |
|--------|------|
| **CPdfReader** (backed by xpdf) | Reads and understands the existing PDF. Knows where every object lives in the file, what every page contains, what every font looks like. |
| **CPdfWriter** (ONLYOFFICE's own writer) | Builds a fresh layer of new or modified PDF objects on top. Has its own object model, font engine, and serializer. |

These two systems are coordinated by a middle layer called **CPdfEditor**,
which understands editing operations and decides what to read from the old
file and what to write into the new layer.

---

## Why This Works: Incremental Updates

This whole approach is possible because of a fundamental feature of the PDF
format called **incremental updates**. A PDF file is append-only by design:

1. The original file contains objects (pages, fonts, images) followed by a
   cross-reference table that records the byte offset of each object.
2. When you make a change, you **append the new versions of changed objects
   to the end of the file**, then append a new cross-reference table that
   points to the updated versions.
3. Any PDF reader always uses the **last** cross-reference table in the file,
   so it sees the updated objects automatically.
4. The old objects remain in the file but are no longer referenced — they are
   effectively invisible to readers.

```
Original file:
┌─────────────────────────────────────────────────────┐
│  %PDF-1.7 header                                    │
│  ...objects (pages, fonts, images)...               │
│  xref table #1  (points to original objects)        │
│  trailer  →  Root: catalog obj 1                    │
└─────────────────────────────────────────────────────┘

After incremental edit (appended):
┌─────────────────────────────────────────────────────┐
│  %PDF-1.7 header                                    │
│  ...original objects (unchanged)...                 │
│  xref table #1                                      │
│  trailer  →  Root: catalog obj 1                    │
├─────────────────────────────────────────────────────┤  ← append starts here
│  new/modified objects                               │
│  xref table #2  (points to new objects + Prev→#1)  │
│  trailer  →  Root: updated catalog                  │
└─────────────────────────────────────────────────────┘
```

Editing a PDF is really just **appending a patch to the end of the original**.

---

## The Four Edit Modes

`CPdfEditor` operates in one of four modes depending on the operation:

| Mode | Description | Used for |
|------|-------------|----------|
| **WriteAppend** | Reader and writer work on the same file. Changes are appended as an incremental update. | Annotations, form fills, text additions, redactions, signatures |
| **WriteNew** | Full rewrite. A brand new PDF is built from scratch. | When the resulting structure is too different from the original |
| **Split** | Reader and writer work on different files. | Pulling pages out into a separate file |
| **ReadOnly** | No writing. | Viewing, text extraction, rendering |

---

## Step 1 — Entering Edit Mode

When the user starts making changes, `EditPdf()` is called. It does two things:

**First**, it reads the existing PDF's internal structure through xpdf:
- The cross-reference table (byte offset of every object in the file)
- The catalog (the root of the document tree)
- The page tree (how pages are organized hierarchically)
- The encryption dictionary (if the file is password protected)
- The AcroForm dictionary (all form fields)

**Second**, it hands all of this to `CPdfWriter` as the starting state. The
writer now knows the full shape of the document and can assign new object
numbers that do not conflict with existing ones.

After this, `EditPage(N)` is called for the specific page being modified.
The writer loads that page's existing content streams, resources (fonts,
images, color spaces), and geometry as **editable objects** — meaning new
content can be appended to them.

---

## Step 2 — Content Edits (Adding Text or Shapes)

When the user draws something or types text on a page, the JavaScript layer
sends an **XML description** of the change to C++. The C++ side converts this
into standard PDF drawing operators — the same `m`, `l`, `Tf`, `Tj`, `rg`
instructions that the original content stream uses.

These operators are written into a **new content stream object**. They are
never merged into the existing content stream. Instead, the page's `/Contents`
entry — the list of content streams for that page — gets a new version that
includes the original streams **plus the new one appended at the end**.
Anything drawn last paints on top of everything drawn before.

```
Before edit:
  Page /Contents → [stream A, stream B]    ← original content

After adding a text box:
  Page /Contents → [stream A, stream B, stream C]   ← C is new
                                              ↑
                              new text/shape operators live here
```

Fonts used for new text are embedded fresh using ONLYOFFICE's own font
subsystem (FreeType-based). Only the glyphs actually used are embedded
(font subsetting), keeping the file size small.

---

## Step 3 — Annotations (Highlights, Comments, Links)

Annotations are handled **completely separately** from page content. They are
never written into the content stream. Instead they live in the `/Annots`
array attached to the page dictionary.

**Adding an annotation:**

1. `CPdfEditor::EditAnnot()` creates a new annotation dictionary object.
2. This object describes: type, rectangle, color, text content, author, and
   an **appearance stream** (the actual visual representation of the annotation).
3. A new version of the page dictionary is written with the updated `/Annots`
   array that includes this new entry.

**Modifying an existing annotation:**

1. The system finds the original annotation object by its ID.
2. A new version of that object is written with the changes applied.
3. The old version is shadowed — still in the file bytes, no longer referenced.

**Annotation types supported:**
- Text (sticky note)
- Highlight, underline, strikeout
- Link (GoTo page, URI, Named action, JavaScript)
- Ink (freehand drawing)
- Form fields (text input, checkbox, radio button, combo box, signature)

---

## Step 4 — Redaction (Permanently Removing Content)

Redaction is more destructive than annotation. It **permanently removes**
content from the page — it cannot be undone once saved.

The flow is:

1. User marks rectangular areas to redact on page N.
2. `CPdfEditor::Redact()` is called with the rectangle coordinates.
3. A special xpdf `OutputDev` called **`RedactOutputDev`** is used to
   re-render the page. When xpdf interprets the content stream and calls
   the output device for any drawing operation that falls inside a redaction
   rectangle, that operation is simply **suppressed** — not drawn.
4. The result is a freshly rendered version of the page with those areas blank.
5. This rendered content is written as a **brand new content stream** that
   replaces the old one entirely.

```
Before redaction:
  Page /Contents → [original stream with sensitive text]

After redaction:
  Page /Contents → [new clean stream, sensitive area is blank]
                   ↑ old stream is orphaned (no longer referenced)
```

This is why redaction cannot be an incremental overlay — the sensitive data
must physically not exist in the new content stream. The old stream is still
in the file bytes but the page no longer points to it.

---

## Step 5 — Page Structure Operations

### Adding a Page
A blank new page object is created with an empty content stream. It is
inserted into the page tree at the correct position. The parent node in the
tree gets a new version with the updated `/Kids` array.

### Deleting a Page
The page reference is removed from its parent's `/Kids` array. A new version
of that parent node is written without the deleted entry. The page object
itself remains in the file bytes but is unreachable — no cross-reference
points to it any more.

### Moving a Page
The page reference is removed from its current position in the tree and
inserted at the new position. All affected parent nodes get new versions.

### Merging Two PDFs
This is the most complex operation:

1. The second PDF is opened through a separate `CPdfReader` instance.
2. Every object from the second document — pages, fonts, images, form fields
   — is copied into the writer under **new object IDs**, offset by the maximum
   ID of the first document so there are no numbering conflicts.
3. Font and image resources are deduplicated where possible.
4. The page trees of both documents are merged into one unified structure.
5. Form field names are prefixed to avoid name collisions between the two
   documents' AcroForm fields.

---

## Step 6 — Digital Signatures

Signing happens in **two passes** because the signature must cover the bytes
of the file itself, but those bytes only exist after the file is written.

**Pass 1 — `PrepareSignature()`:**
- The writer produces the complete final file.
- Where the signature bytes will go, it writes a placeholder filled with zeros
  of the correct size.
- The byte positions of this placeholder are recorded as the `/ByteRange`.

**Pass 2 — `FinalizeSignature()`:**
- The file bytes excluding the placeholder area are fed to the cryptographic
  library (PKCS#7 / CMS).
- The resulting signature is written directly into the placeholder area.

```
Final signed file structure:

[ file bytes before placeholder ][ placeholder / sig bytes ][ file bytes after ]
 ↑_________________________________↑                         ↑__________________↑
        covered by signature                                   covered by signature

/ByteRange = [0, offset_before, offset_after, length_after]
```

The signature field in the PDF's `/ByteRange` always shows two ranges with
a gap — that gap is the signature container itself, which is intentionally
excluded from what is signed (you cannot sign something whose value depends
on the signature itself).

---

## Step 7 — Saving the File

`SaveToFile()` serializes all new and modified objects to disk:

1. Each object is written in standard PDF syntax:
   `N G obj ... endobj`
2. After all objects, the new **cross-reference table** is written — either
   a classic flat table or a compressed XRef stream for PDF 1.5+ files.
3. The new **trailer** is written, pointing to the updated catalog and
   recording the position of the previous cross-reference table (`/Prev`).

**For incremental updates (WriteAppend mode):**
The entire new section is appended to the original file. The original file
bytes are preserved unchanged before the appended section.

**For full rewrites (WriteNew, Merge, Split, Redaction):**
A completely new file is produced. No bytes from the original are carried
forward verbatim.

---

## Complete Edit Flow — End to End

```
User makes a change in the browser
        │
        ▼
JavaScript (sdkjs)
  Encodes the change as XML or structured data
  Calls the appropriate WASM function
        │
        ▼
CPdfFile (C++ / WASM)
  Receives the command
  Delegates to CPdfEditor
        │
        ▼
CPdfEditor
  Reads affected objects from CPdfReader (xpdf)
  Builds new/modified objects via CPdfWriter
  Appends to the object graph
        │
    ┌───┴────────────────────────────────────┐
    │  Content edit   │  Annotation  │  Redact│
    ▼                 ▼              ▼        │
  New content    New annotation   RedactOutputDev
  stream         dictionary       re-renders page
  appended to    added to         → new clean stream
  page /Contents page /Annots     replaces old one
    └───────────────────────────────────────┘
        │
        ▼
User clicks Save
        │
        ▼
CPdfFile::SaveToFile()
  → CPdfWriter serializes all new objects
  → Writes new XRef table
  → Writes new trailer (with /Prev pointing to old XRef)
  → Appends everything to end of original file
        │
        ▼
Result: valid PDF with incremental update appended
Original bytes untouched
```

---

## Key Architectural Decisions

**1. Never modifying original bytes**
All changes are appended. The original file is forensically preserved. This
is required for PDF compliance and for digital signatures (which cover the
original bytes).

**2. Separate reader and writer systems**
xpdf is the reader — battle-tested for 30 years, handles every real-world
PDF quirk. ONLYOFFICE's PdfWriter is the writer — clean, modern, produces
standards-compliant output. Neither system needs to understand the other's
internals.

**3. Annotation vs content separation**
Annotations never touch the content stream. This means a highlight or comment
can be added, moved, or deleted without re-rendering the page at all.

**4. RedactOutputDev for redaction**
Rather than parsing and modifying the content stream (which is complex and
error-prone), redaction works by re-rendering the page through a suppressing
output device. The result is always correct regardless of how complex the
original content stream was.

**5. Two-pass signing**
Writing the signature placeholder first, then filling it in, is the only
correct way to sign a PDF. It ensures the signature covers exactly the bytes
that will appear in the final file.

**6. Font subsetting on write**
When embedding a font for new text content, only the glyphs actually used
are included. A paragraph using 12 characters from a 10,000-glyph font will
only add those 12 glyphs to the file.
