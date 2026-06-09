# ONLYOFFICE PDF Reader — Complete Internal Flow

> Reference study only. No code or design from this system may be copied into the commercial Rust rebuild.
> See REBUILD_PLAN.md §L1 and §L2.

---

## 1. System Architecture — All Layers

```
┌─────────────────────────────────────────────────────────────┐
│                    WEB APP UI LAYER                         │
│  HTML + CSS + Backbone.js/React                            │
│  PDFEditorApi (api.js) — public API for the UI             │
│  APIBuilder (apiBuilder.js) — command builder              │
│                                                             │
│  User actions: open file, zoom, next page, select text     │
└──────────────────────┬──────────────────────────────────────┘
                       │ openDocument(file.data)
                       │ getDocumentRenderer()
                       ▼
┌─────────────────────────────────────────────────────────────┐
│            JAVASCRIPT SDK LAYER  (sdkjs/pdf/)               │
│                                                             │
│  file.js       (CFile)     — PDF file wrapper              │
│  viewer.js     (CViewer)   — canvas + page rendering       │
│  document.js   (CPDFDoc)   — document logic                │
│  apiPDF.js                 — PDF-specific API methods      │
│  thumbnails.js             — thumbnail strip               │
│  annotations/              — annotation overlay            │
│  forms/                    — form field handling           │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  WASM BRIDGE  (CDrawingFile)                        │   │
│  │  nativeFile["loadFromData"](bytes)                  │   │
│  │  nativeFile["getPages"]() → [{W,H,Dpi,Rotate,...}] │   │
│  │  nativeFile["getPagePixmap"](idx,w,h,bg) → RGBA    │   │
│  │  nativeFile["getStructure"]() → XML                │   │
│  │  nativeFile["getLinks"](pageIdx)                   │   │
│  │  nativeFile["getAnnots"](pageIdx)                  │   │
│  │  nativeFile["getWidgets"]()                        │   │
│  │  nativeFile["getGIDByUnicode"](fontName)           │   │
│  └─────────────────────────────────────────────────────┘   │
└──────────────────────┬──────────────────────────────────────┘
                       │ Emscripten WASM function calls
                       │ Memory shared via WASM heap
                       ▼
┌─────────────────────────────────────────────────────────────┐
│          C++ CORE LAYER   (core/PdfFile/)                   │
│          Compiled to drawingfile.wasm via Emscripten        │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  CPdfFile  (PdfFile.h/cpp)                           │  │
│  │  • Main entry point                                  │  │
│  │  • Implements IOfficeDrawingFile + IRenderer         │  │
│  │  • Delegates to CPdfReader (read) / CPdfWriter (write│  │
│  └──────────────────────────┬─────────────────────────┘  │
│                             │                              │
│           ┌─────────────────┴───────────────────┐         │
│           ▼                                     ▼         │
│  ┌──────────────────────┐        ┌────────────────────┐   │
│  │  CPdfReader          │        │  CPdfWriter        │   │
│  │  • LoadFromFile/Mem  │        │  • CreatePdf       │   │
│  │  • GetPageInfo       │        │  • SaveToFile      │   │
│  │  • DrawPageOnRenderer│        │  • EditPage ops    │   │
│  │  • Font management   │        │  • Sign/Redact     │   │
│  │  • Merge/Unmerge     │        └────────────────────┘   │
│  └──────────┬───────────┘                                  │
│             │                                              │
│             ▼                                              │
│  ┌────────────────────────────────────────────────────┐   │
│  │  SrcReader/  (Custom xpdf OutputDev)               │   │
│  │                                                    │   │
│  │  RendererOutputDev  (extends xpdf OutputDev)       │   │
│  │  • stroke(), fill(), eoFill()                      │   │
│  │  • drawChar() — renders each glyph                 │   │
│  │  • drawImage(), drawMaskedImage()                  │   │
│  │  • shadedFill() — gradients                        │   │
│  │  • tilingPatternFill() — patterns                  │   │
│  │  • beginTransparencyGroup() / setSoftMask()        │   │
│  │  • All calls go to IRenderer (not bitmap)          │   │
│  │                                                    │   │
│  │  PdfFont.h/cpp — font glyph/unicode cache          │   │
│  │  PdfAnnot.h/cpp — annotation/action parsing        │   │
│  │  Adaptors.h/cpp — xpdf GlobalParams glue           │   │
│  └──────────────────────┬─────────────────────────────┘   │
│                         │                                  │
│                         ▼                                  │
│  ┌────────────────────────────────────────────────────┐   │
│  │  xpdf Library  (core/PdfFile/lib/xpdf/)            │   │
│  │  • PDFDoc — top-level document object              │   │
│  │  • XRef — cross-reference / object table           │   │
│  │  • Catalog, Page — document structure              │   │
│  │  • Gfx — content stream interpreter                │   │
│  │  • GfxState — graphics state (CTM, color, font)    │   │
│  │  • Stream — filter chain (Flate, DCT, LZW, ...)    │   │
│  │  • GfxFont — font type resolution                  │   │
│  │  • OutputDev — abstract rendering interface        │   │
│  └────────────────────────────────────────────────────┘   │
│                         │                                  │
│                         ▼                                  │
│  ┌────────────────────────────────────────────────────┐   │
│  │  DesktopEditor Framework  (IRenderer / Fonts)      │   │
│  │  • IRenderer interface — abstract drawing backend  │   │
│  │  • IFontManager — system + embedded font access    │   │
│  │  • Image decoders (JBIG2, JPEG, PNG, TIFF)        │   │
│  │  • Color profiles & transformation matrices        │   │
│  └────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

---

## 2. C++ Core — Complete Public API

### CPdfFile (PdfFile.h) — Main Entry Point

```cpp
class CPdfFile : public IOfficeDrawingFile, public IRenderer
{
    // ── LOADING ────────────────────────────────────────────
    CPdfFile(NSFonts::IApplicationFonts* pAppFonts);

    bool LoadFromFile(const std::wstring& file,
                      const std::wstring& options = L"",
                      const wchar_t* owner_password = NULL,
                      const wchar_t* user_password  = NULL);

    bool LoadFromMemory(BYTE* data, DWORD length,
                        const std::wstring& options = L"",
                        const wchar_t* owner_password = NULL,
                        const wchar_t* user_password  = NULL);
    void Close();

    // ── DOCUMENT INFO ──────────────────────────────────────
    int  GetPagesCount();
    int  GetError();        // see EError enum below
    void GetPageInfo(int nPage, double* pdW, double* pdH,
                     double* pdDpiX, double* pdDpiY);
    int  GetRotate(int nPage);  // 0 | 90 | 180 | 270
    std::wstring GetInfo();
    BYTE* GetStructure();       // XML page/object tree
    BYTE* GetLinks(int nPage);  // hyperlink rectangles
    BYTE* GetAnnots(int nPage); // annotation data
    BYTE* GetWidgets();         // form field data

    // ── RENDERING ─────────────────────────────────────────
    void DrawPageOnRenderer(IRenderer* pRenderer,
                            int nPageIndex,
                            bool* pBreak,
                            COfficeDrawingPageParams* pParams = NULL);

    // ── FONTS & GLYPHS ────────────────────────────────────
    NSFonts::IFontManager* GetFontManager();
    BYTE*        GetGIDByUnicode(const std::wstring& wsFontName);
    std::wstring GetFontPath(const std::wstring& wsFontName);
    std::wstring GetEmbeddedFontPath(const std::wstring& wsFontName);

    // ── CMAPS (CJK encoding support) ──────────────────────
    bool IsNeedCMap();
    void SetCMapMemory(BYTE* pData, DWORD nSizeData);
    void SetCMapFolder(const std::wstring& sFolder);
    void SetCMapFile(const std::wstring& sFile);

    // ── EDIT MODE ─────────────────────────────────────────
    bool EditPdf(const std::wstring& wsDstFile = L"");
    void EditClose();
    bool EditPage(int nPageIndex);
    bool DeletePage(int nPageIndex);
    bool AddPage(int nPageIndex);
    bool MovePage(int nPageIndex, int nPos);
    bool MergePages(const std::wstring& wsPath, ...);
    bool UnmergePages();
    bool RedactPage(int nPage, double* arrRedactBox, ...);
    bool UndoRedact();

    // ── WRITE & EXPORT ────────────────────────────────────
    void CreatePdf(bool isPDFA = false);
    int  SaveToFile(const std::wstring& wsPath);
    void SetPassword(const std::wstring& wsPassword);
    void SetDocumentInfo(const std::wstring& wsTitle, ...);
    void Sign(...);

    // ── IRENDERER METHODS (painting commands) ─────────────
    HRESULT put_PenColor(const LONG& lColor);
    HRESULT put_PenSize(const double& dSize);
    HRESULT put_BrushColor1(const LONG& lColor);
    HRESULT put_FontName(const std::wstring& wsName);

    HRESULT CommandDrawText(const std::wstring& wsText,
                            const double& dX, const double& dY,
                            const double& dW, const double& dH);

    HRESULT PathCommandMoveTo(const double& dX, const double& dY);
    HRESULT PathCommandLineTo(const double& dX, const double& dY);
    HRESULT PathCommandCurveTo(double dX1, double dY1,
                               double dX2, double dY2,
                               double dXe, double dYe);
    HRESULT DrawPath(const LONG& lType);
    HRESULT DrawImage(IGrObject* pImage,
                      const double& dX, const double& dY,
                      const double& dW, const double& dH);
    HRESULT SetTransform(double dM11, double dM12,
                         double dM21, double dM22,
                         double dX,   double dY);
    HRESULT put_ClipMode(const LONG& lMode);
};
```

### EError Enum
```cpp
enum EError {
    errorNone         = 0,   // Success
    errorOpenFile     = 1,   // Cannot open/read file
    errorBadCatalog   = 2,   // Invalid page catalog
    errorDamaged      = 3,   // Corrupted / malformed PDF
    errorEncrypted    = 4,   // Password required
    errorHighlightFile= 5,   // Annotation file error
    errorBadPrinter   = 6,   // Printer configuration error
    errorPrinting     = 7,   // Print operation failed
    errorPermission   = 8,   // Restricted by PDF permissions
    errorBadPageNum   = 9,   // Invalid page number
    errorFileIO       = 10,  // File I/O error
    errorMemory       = 11,  // Memory allocation failed
};
```

### CPdfReader (PdfReader.h) — xpdf Wrapper

```cpp
class CPdfReader {
    bool LoadFromFile(NSFonts::IApplicationFonts*,
                      const std::wstring& file,
                      const wchar_t* owner_pw = NULL,
                      const wchar_t* user_pw  = NULL);

    bool LoadFromMemory(NSFonts::IApplicationFonts*,
                        BYTE* data, DWORD length,
                        const wchar_t* owner_pw = NULL,
                        const wchar_t* user_pw  = NULL);

    int  GetError();
    int  GetNumPages();

    void DrawPageOnRenderer(IRenderer* pRenderer,
                            int nPageIndex,
                            bool* pBreak);

    void GetPageInfo(int nPage, double* pdW, double* pdH,
                     double* pdDpiX, double* pdDpiY);
    int  GetRotate(int nPage);

    // Font management
    NSFonts::IFontManager*    GetFontManager();
    void SetFonts(PdfReader::CPdfFontList* pFontList);

    // Multi-document state (for merge)
    PDFDoc* GetPDFDocument(int PDFIndex);
    PDFDoc* GetLastPDFDocument();
    int     GetNumPagesBefore(PDFDoc* pDoc);
    PdfReader::CPdfFontList* GetFontList(PDFDoc* pDoc);

    // Document metadata
    std::wstring GetInfo();
    bool ValidMetaData();

    // Merge / split
    bool MergePages(BYTE* pData, DWORD nLength,
                    int nMaxID = 0, const std::string& sPrefix = "");
    bool UnmergePages();

    // Redaction
    bool RedactPage(int nPage, double* arrRedactBox,
                    int nLengthX8, BYTE* pChanges, int nLength);
    bool UndoRedact();

    // Data extraction
    BYTE* GetStructure();
    BYTE* GetLinks(int nPage);
    BYTE* GetWidgets();
    BYTE* GetAnnots(int nPage = -1);
    BYTE* GetAPAnnots(...);    // Appearance streams

private:
    std::vector<CPdfReaderContext*> m_vPDFContext;
    NSFonts::IFontManager*          m_pFontManager;
};

// Internal per-document state
struct CPdfReaderContext {
    PDFDoc*                  m_pDocument;    // xpdf document
    PdfReader::CPdfFontList* m_pFontList;    // font cache
    unsigned int             m_nStartID;     // base ref ID (for merge)
    std::string              m_sPrefixForm;  // form field prefix (for merge)
};
```

---

## 3. SrcReader — Custom OutputDev

ONLYOFFICE does **not** use xpdf's SplashOutputDev.
Instead it implements `RendererOutputDev` which extends xpdf's `OutputDev`
and translates all PDF drawing commands to `IRenderer` calls (a vector-aware
abstract interface), not to a bitmap.

```
xpdf Gfx (operator interpreter)
    │ calls virtual methods on OutputDev
    ▼
RendererOutputDev  (ONLYOFFICE custom)
    │ translates to IRenderer calls
    ▼
IRenderer  (abstract, platform-independent)
    │ writes RGBA pixels (browser) or SVG/print output (desktop)
    ▼
Canvas / Pixel buffer
```

### RendererOutputDev Key Methods

```cpp
class RendererOutputDev : public OutputDev {

    // Page lifecycle
    virtual void startPage(int nPageIndex, GfxState* pGState);
    virtual void endPage();

    // Graphics state
    virtual void saveState(GfxState* pGState);
    virtual void restoreState(GfxState* pGState);
    virtual void updateCTM(GfxState*, double m11, double m12,
                                      double m21, double m22,
                                      double m31, double m32);
    virtual void updateFillColor(GfxState*);
    virtual void updateStrokeColor(GfxState*);
    virtual void updateLineWidth(GfxState*);
    virtual void updateFont(GfxState*);
    virtual void updateBlendMode(GfxState*);
    virtual void updateFillOpacity(GfxState*);
    virtual void updateStrokeOpacity(GfxState*);

    // Path painting
    virtual void stroke(GfxState*);   // outline path
    virtual void fill(GfxState*);     // fill nonzero
    virtual void eoFill(GfxState*);   // fill even-odd
    virtual void clip(GfxState*);
    virtual void eoClip(GfxState*);
    virtual void clipToStrokePath(GfxState*);

    // Text
    virtual void drawChar(GfxState*, double x, double y,
                          double dx, double dy,
                          double originX, double originY,
                          CharCode code, int bytesCount,
                          Unicode* u, int uLen);
    virtual void beginStringOp(GfxState*);
    virtual void endStringOp(GfxState*);
    virtual void endTextObject(GfxState*);
    virtual void beginMarkedContent(GfxState*, GString*);

    // Shading & patterns
    virtual GBool shadedFill(GfxState*, GfxShading*);
    virtual void  tilingPatternFill(GfxState*, Gfx*, Object* stream,
                                    int paintType, int tilingType,
                                    Dict* resources, double* matrix,
                                    double* bbox, int x0, int y0,
                                    int x1, int y1,
                                    double xStep, double yStep);

    // Images
    virtual void drawImageMask(GfxState*, Gfx*, Object* ref,
                               Stream*, int w, int h,
                               GBool invert, GBool inlineImg,
                               GBool interpolate);
    virtual void drawImage(GfxState*, Gfx*, Object* ref,
                           Stream*, int w, int h,
                           GfxImageColorMap*, int* maskColors,
                           GBool inlineImg, GBool interpolate);
    virtual void drawMaskedImage(GfxState*, Gfx*, Object* ref,
                                 Stream*, int w, int h,
                                 GfxImageColorMap*,
                                 Object* maskRef, Stream* maskStream,
                                 int maskW, int maskH,
                                 GBool maskInvert, GBool interpolate);
    virtual void drawSoftMaskedImage(GfxState*, Gfx*, Object* ref,
                                     Stream*, int w, int h,
                                     GfxImageColorMap*,
                                     Object* maskRef, Stream* maskStream,
                                     int maskW, int maskH,
                                     GfxImageColorMap* maskColorMap,
                                     double* matte, GBool interpolate);

    // Transparency
    virtual void beginTransparencyGroup(GfxState*, double* bbox,
                                        GfxColorSpace* blendCS,
                                        GBool isolated,
                                        GBool knockout,
                                        GBool forSoftMask);
    virtual void endTransparencyGroup(GfxState*);
    virtual void paintTransparencyGroup(GfxState*, double* bbox);
    virtual void setSoftMask(GfxState*, double* bbox, GBool alpha,
                             Function* transferFunc,
                             GfxColor* backdropColor);
    virtual void clearSoftMask(GfxState*);

    // Feature flags
    virtual GBool useDrawChar()              { return gTrue; }
    virtual GBool useTilingPatternFill()     { return gTrue; }
    virtual GBool interpretType3Chars()      { return gTrue; }
    virtual GBool useSimpleTransparentGroup(){ return gTrue; }

private:
    IRenderer*                  m_pRenderer;
    NSFonts::IFontManager*      m_pFontManager;
    XRef*                       m_pXref;
    PdfReader::CPdfFontList*    m_pFontList;
    std::deque<GfxOutputState>  m_sStates;   // graphics state stack
    std::deque<GfxOutputCS>     m_sCS;       // color space stack
};
```

### Why IRenderer Instead of Bitmap?

| | SplashOutputDev (xpdf) | RendererOutputDev (ONLYOFFICE) |
|--|------------------------|-------------------------------|
| Output | `SplashBitmap` (raster) | `IRenderer` (abstract) |
| Rendering | Own Splash rasterizer | Platform's drawing backend |
| Text | Rasterize glyph bitmaps | Vector glyph outlines |
| Portability | Single platform | Web, desktop, print |
| Edit support | Not applicable | Can emit editable commands |

---

## 4. WASM Bridge — JavaScript ↔ C++

### Functions Exported from drawingfile.wasm

```javascript
// Instance creation
const pdfEngine = new window["AscViewer"]["CDrawingFile"]();

// ── LOADING ───────────────────────────────────────────────
pdfEngine["loadFromData"](data: Uint8Array)
    → error_code: number  (0 = success, 4 = encrypted)

pdfEngine["loadFromDataWithPassword"](password: string)
    → error_code: number

// ── DOCUMENT STRUCTURE ────────────────────────────────────
pdfEngine["getPages"]()
    → Array<{W: number, H: number, Dpi: number,
             Rotate: number, originIndex: number}>

pdfEngine["getType"]()
    → document_type_code: number

pdfEngine["getStructure"]()
    → xml_string: string  (page/object tree as XML)

pdfEngine["getDocumentInfo"]()
    → {Title, Subject, Author, Keywords,
       Creator, Producer, CreationDate, ModificationDate}

// ── RENDERING ─────────────────────────────────────────────
pdfEngine["getPagePixmap"](pageIdx: number,
                            width: number,
                            height: number,
                            backgroundColor: number)
    → pixel_pointer: number  (WASM heap pointer to RGBA bytes)
    // width * height * 4 bytes, RGBA 32-bit

// ── ANNOTATIONS & FORMS ───────────────────────────────────
pdfEngine["getLinks"](pageIdx: number)
    → links_json: string

pdfEngine["getAnnots"](pageIdx: number)
    → annots_json: string

pdfEngine["getWidgets"]()
    → widgets_json: string

// ── FONTS ─────────────────────────────────────────────────
pdfEngine["getGIDByUnicode"](fontName: string)
    → gid_map: Uint8Array

pdfEngine["isNeedCMap"]()
    → bool

pdfEngine["setCMap"](cmapBinary: Uint8Array)
    → void

// ── TEXT ──────────────────────────────────────────────────
pdfEngine["getGlyphs"](pageIdx: number)
    → glyph_positions: Uint8Array

pdfEngine["destroyTextInfo"]()
    → void

// ── EDIT ──────────────────────────────────────────────────
pdfEngine["addPage"](pageIdx: number, pageObj: object) → bool
pdfEngine["removePage"](pageIdx: number) → bool

// ── MEMORY ────────────────────────────────────────────────
pdfEngine["free"](pointer: number) → void  // release WASM heap
pdfEngine["close"]()

// ── CALLBACKS (JS → WASM) ─────────────────────────────────
pdfEngine["onRepaintPages"]      = (pages) => { ... }
pdfEngine["onRepaintAnnotations"]= (pages) => { ... }
pdfEngine["onRepaintForms"]      = (pages) => { ... }
pdfEngine["onUpdateStatistics"]  = (par, word, sym, sp) => { ... }
pdfEngine["isPunctuation"]       = (unicode) => boolean
```

### Memory Management Across the WASM Boundary

```
JavaScript                    WASM heap
────────────────────────────────────────────
Uint8Array pdfBytes          copied into WASM by loadFromData()
                             C++ allocates RGBA buffer
pixel_ptr ◄──────────────── getPagePixmap() returns pointer
Uint8ClampedArray(           view WASM memory — zero copy
  WASM.buffer,
  pixel_ptr,
  w * h * 4)
ctx.putImageData(...)        write to canvas
pdfEngine["free"](pixel_ptr) ──────────────► C++ free()
```

---

## 5. Complete End-to-End Flows

### Flow A — Open PDF

```
① User selects PDF file in browser
   ▼
② Web App: PDFEditorApi.openDocument(fileData: Uint8Array)
   ├─ initDocumentRenderer()
   └─ DocumentRenderer.open(file.data)
   ▼
③ JavaScript (sdkjs/pdf/src/file.js)
   file.nativeFile = new CDrawingFile()
   error = file.nativeFile["loadFromData"](data)
   ▼
④ C++: CPdfFile::LoadFromMemory()
   ├─ m_pInternal->pReader = new CPdfReader()
   └─ pReader->LoadFromMemory(appFonts, data, len, ownerPw, userPw)
   ▼
⑤ C++: CPdfReader::LoadFromMemory()
   ├─ Creates xpdf MemStream from bytes
   ├─ new PDFDoc(stream, ownerPw, userPw)
   └─ PDFDoc parses: header → XRef → Catalog → Pages
   ▼
⑥ xpdf: PDFDoc constructor
   ├─ Scan file for "%PDF-" header
   ├─ Find "startxref" at end of file
   ├─ Parse XRef table (or rebuild via constructXRef)
   ├─ Load /Root → Catalog
   ├─ Walk /Pages tree → build page array
   └─ Detect encryption → set up Decrypt keys
   ▼
⑦ C++ returns: error = 0 (success) or EError code
   ▼
⑧ JavaScript:
   file.pages = file.nativeFile["getPages"]()
   // → [{W:612, H:792, Dpi:72, Rotate:0, originIndex:0}, ...]
   file.totalPages = file.pages.length
   ▼
⑨ Trigger render of first page (see Flow B)
```

If `error == 4` (encrypted):
```
User prompted for password
    ▼
file.nativeFile["loadFromDataWithPassword"](password)
    ▼
CPdfFile::LoadFromMemory() retried with password
```

---

### Flow B — Render Page to Canvas

```
① Page N becomes visible (scroll / navigation)
   ▼
② JavaScript: viewer.renderPage(N)
   renderWidth  = page.W * zoom
   renderHeight = page.H * zoom
   ▼
③ JavaScript: pixelPtr = file.nativeFile["getPagePixmap"](
       N, renderWidth, renderHeight, 0xFFFFFFFF)
   ▼
④ C++: CPdfFile::GetPagePixmap()
   ├─ Create IRenderer wrapping a RGBA pixel buffer
   │   (renderWidth × renderHeight × 4 bytes)
   └─ DrawPageOnRenderer(pRenderer, N, &bBreak)
   ▼
⑤ C++: CPdfReader::DrawPageOnRenderer()
   ├─ Get CPdfReaderContext for this document
   ├─ Create RendererOutputDev(pRenderer, fontManager, fontList)
   └─ xpdf: page->display(outputDev, hDPI, vDPI, rotate,
                           useMediaBox, crop, printing)
   ▼
⑥ xpdf: Page::display()
   ├─ Concatenate content streams (if multiple)
   ├─ Create Gfx interpreter
   └─ Gfx::go(RendererOutputDev)
   ▼
⑦ xpdf: Gfx::go() — for each operator in content stream:

   ── Graphics state ──
   q/Q    → saveState/restoreState → RendererOutputDev::saveState
   cm     → updateCTM             → pRenderer->SetTransform
   gs     → load ExtGState dict   → updateBlendMode, updateFillOpacity, ...

   ── Color ──
   rg/RG  → updateFillColor       → pRenderer->put_BrushColor1/PenColor
   k/K    → CMYK → RGB conversion  → pRenderer->put_BrushColor1/PenColor
   cs/sc  → arbitrary colorspace  → color space conversion → put_BrushColor1

   ── Path ──
   m/l/c  → accumulate path points
   re     → add rectangle to path
   S/s    → stroke(GfxState*)    → RendererOutputDev::stroke()
   f/F    → fill(GfxState*)      → RendererOutputDev::fill()
   B/b    → fill + stroke        → both

   ── Text ──
   BT     → begin text object
   Tf     → updateFont()         → load font via CPdfFontList
   Tm/Td  → update text matrix
   Tj/TJ  → for each character:
              font->getNextChar() → code, Unicode, dx
              outputDev->drawChar(x, y, dx, dy, code, unicode)
              text_x += dx * fontSize * hScale
   ET     → endTextObject()

   ── Images ──
   Do(image XObject) → drawImage() or drawMaskedImage()
   BI...EI          → inline image → drawImageMask()

   ── XObject (Form) ──
   Do(form XObject) → recursive Gfx::display(form stream)
   ▼
⑧ RendererOutputDev methods call IRenderer:
   stroke()   → pRenderer->PathCommandMoveTo/LineTo/CurveTo
                pRenderer->DrawPath(DRAW_PATH_STROKE)
   fill()     → same path building
                pRenderer->DrawPath(DRAW_PATH_FILL)
   drawChar() → lookup glyph in CPdfFontList
                pRenderer->CommandDrawText(unicode, x, y, w, h)
   drawImage()→ decode image stream → pRenderer->DrawImage(img, x, y, w, h)
   ▼
⑨ IRenderer writes RGBA pixels to buffer
   ▼
⑩ C++ returns pixel_ptr to JavaScript
    ▼
⑪ JavaScript:
    const pixels = new Uint8ClampedArray(WASM.buffer, pixelPtr, w*h*4)
    const imageData = new ImageData(pixels, w, h)
    ctx.putImageData(imageData, 0, 0)
    file.nativeFile["free"](pixelPtr)
    ▼
⑫ Canvas displays rendered page
```

---

### Flow C — Text Selection

```
① User click-drags on page N
   ▼
② JavaScript: viewer.onMouseDown(pageIdx, x_pdf, y_pdf)
   file.nativeFile["onMouseDown"](pageIdx, x_pdf, y_pdf)
   ▼
③ C++ tracks selection start point in PDF coordinate space
   ▼
④ User moves mouse
   file.nativeFile["onMouseMove"](pageIdx, x_pdf, y_pdf)
   C++ updates selection rectangle
   ▼
⑤ User releases mouse
   file.nativeFile["onMouseUp"](pageIdx, x_pdf, y_pdf)
   ▼
⑥ C++:
   ├─ Walk glyph positions on page N (from last render)
   ├─ Find all glyphs whose bounding box intersects selection rect
   ├─ Collect Unicode values in reading order
   └─ Return: quad boxes + Unicode text
   ▼
⑦ JavaScript:
   ├─ Draw highlight overlay on canvas (RGBA semi-transparent)
   └─ Copy Unicode text to clipboard
```

---

### Flow D — Page Navigation

```
User clicks "Next Page" (or scrolls)
    ▼
JavaScript: viewer.goToPage(currentPage + 1)
    ├─ Validate: 0 ≤ newPage < totalPages
    ├─ Update currentPage index
    ├─ Scroll canvas to page position
    └─ Call renderPage(newPage)  →  Flow B
```

---

### Flow E — Zoom Change

```
User changes zoom to 150%
    ▼
JavaScript: viewer.setZoom(1.5)
    ├─ viewer.zoom = 1.5
    ├─ For each visible page:
    │   newW = page.W * 1.5
    │   newH = page.H * 1.5
    │   renderPage(idx, newW, newH)  →  Flow B
    └─ Update scroll position
    ▼
C++ re-renders at new pixel dimensions
(No pixel cache — always re-render from PDF data)
```

---

### Flow F — Open Encrypted PDF

```
User opens password-protected PDF
    ▼
Flow A runs → error = 4 (errorEncrypted)
    ▼
JavaScript: promptForPassword()
    ▼
User types password → "mysecret"
    ▼
file.nativeFile["loadFromDataWithPassword"]("mysecret")
    ▼
C++: CPdfFile::LoadFromMemory(data, len, "mysecret", "mysecret")
    ▼
xpdf: Decrypt::makeFileKey(version, keyLen, "mysecret", ownerKey, ...)
    ├─ Try "mysecret" as user password
    ├─ Try "mysecret" as owner password
    └─ Derive file key if either matches
    ▼
Success → Flow A continues from step ⑦
```

---

## 6. Pixel Format & Canvas Update

```
C++ pixel buffer layout (RGBA 32-bit):
┌──────┬──────┬──────┬──────┬──────┬──────┬──────┬──────┬─...─┐
│  R   │  G   │  B   │  A   │  R   │  G   │  B   │  A   │     │
│ px0  │ px0  │ px0  │ px0  │ px1  │ px1  │ px1  │ px1  │     │
└──────┴──────┴──────┴──────┴──────┴──────┴──────┴──────┴─...─┘
 byte0  byte1  byte2  byte3  byte4  byte5  byte6  byte7

Total bytes = width × height × 4
```

```javascript
// Zero-copy path (WASM memory → canvas)
const ptr    = engine["getPagePixmap"](idx, w, h, 0xFFFFFFFF);
const pixels = new Uint8ClampedArray(wasmModule.buffer, ptr, w * h * 4);
const idata  = new ImageData(pixels, w, h);
ctx.putImageData(idata, 0, 0);
engine["free"](ptr);

// For large pages: WebGL texture upload is faster
// ctx.texImage2D(TEXTURE_2D, 0, RGBA, w, h, 0, RGBA, UNSIGNED_BYTE, pixels)
```

---

## 7. Font Handling Pipeline

```
PDF content stream: "Tf /F1 12" then "Tj (Hello)"
    ▼
xpdf Gfx: opSetFont → GfxFont::makeFont(xref, "/F1", fontDict)
    ▼
RendererOutputDev::updateFont(GfxState*)
    ├─ Get GfxFont from state
    ├─ Get font object reference (Ref: {num, gen})
    └─ CPdfFontList::Find(Ref) → TFontEntry or nullptr
        ▼
    If not cached: CPdfFontList::Add()
        ├─ Check /FontFile2 (TrueType) in FontDescriptor
        ├─ Check /FontFile (Type1)
        ├─ Check /FontFile3 (CFF / OpenType)
        ├─ If embedded: extract stream → save to temp file
        └─ If not embedded: find via IFontManager (system fonts)
        ▼
    TFontEntry {
        wsFilePath:      "/tmp/font_abc.ttf"
        wsFontName:      "Arial"
        pCodeToGID:      int[256] or int[65536]
        pCodeToUnicode:  int[256] or int[65536]
        bFontSubstitution: false
        bIsIdentity:     false
    }
    ▼
xpdf Gfx: opShowText → for each byte in string:
    GfxFont::getNextChar(s, len, &code, &u, ...)
        ├─ code = char code from string bytes
        ├─ u[0] = ToUnicode CMap lookup OR
        │         Encoding → Adobe Glyph List lookup
        └─ dx = advance width from font metrics
    ▼
RendererOutputDev::drawChar(x, y, dx, dy, code, u, uLen)
    ├─ fontEntry->pCodeToGID[code] → GID (glyph index)
    ├─ fontEntry->pCodeToUnicode[code] → Unicode
    └─ pRenderer->CommandDrawText(unicode, x_final, y_final, ...)
    ▼
IRenderer: look up glyph outline by GID in font file
    → render glyph path or bitmap at (x, y) with CTM
```

### CMap (CJK Support)

```
CJK PDFs use 2-byte character codes: e.g., 0x82A0 → Unicode 0x3041 (あ)

Process:
    ├─ isNeedCMap() → true (PDF uses predefined CMap names)
    ├─ JavaScript loads cmap.bin (9.4MB bundled in sdkjs)
    │   OR custom CMap via setCMap(data)
    └─ C++ uses CMap to resolve: code bytes → CID → Unicode
```

---

## 8. Annotation & Form Data Formats

### GetAnnots(pageIdx) → JSON
```json
[
  {
    "id": "annot_0",
    "type": "Link",
    "rect": [72.0, 720.0, 144.0, 738.0],
    "content": "Click here",
    "author": "Author Name",
    "created": "2024-01-01T00:00:00",
    "modified": "2024-01-01T00:00:00",
    "action": {
      "type": "URI",
      "uri": "https://example.com"
    }
  },
  {
    "id": "annot_1",
    "type": "Highlight",
    "rect": [72.0, 700.0, 300.0, 715.0],
    "quads": [[72,700, 300,700, 72,715, 300,715]],
    "color": "#FFFF00"
  }
]
```

### Action Types in PdfAnnot.h
| Type | Description |
|------|-------------|
| `GoTo` | Jump to page + rectangle within the document |
| `GoToR` | Jump to page in a different PDF file |
| `URI` | Open a URL |
| `Named` | Execute named action (Print, Save, NextPage, etc.) |
| `JavaScript` | Execute embedded JavaScript |
| `Hide` | Show/hide form fields |
| `ResetForm` | Reset all form fields |
| `SubmitForm` | Submit form data to URL |

### GetWidgets() → JSON
```json
[
  {
    "id": "field_name",
    "type": "Text",
    "rect": [72.0, 600.0, 300.0, 620.0],
    "page": 0,
    "value": "Current value",
    "defaultValue": "",
    "maxLength": 100,
    "flags": 0
  },
  {
    "id": "checkbox_1",
    "type": "Check",
    "rect": [72.0, 580.0, 90.0, 598.0],
    "page": 0,
    "value": "Off",
    "defaultValue": "Off"
  }
]
```

---

## 9. Document Structure XML (GetStructure)

```xml
<?xml version="1.0"?>
<Pages Count="10">
  <Page Index="0" Width="612" Height="792" Dpi="72" Rotate="0">
    <Objects>
      <Text X="72" Y="720" W="200" H="14" FontSize="12">Page title</Text>
      <Image X="0" Y="0" W="612" H="792"/>
      <Link Rect="72 700 200 715" URI="https://example.com"/>
    </Objects>
  </Page>
  ...
</Pages>
```

---

## 10. Layer Dependencies

```
Layer                  | Depends on                      | Provides
───────────────────────┼─────────────────────────────────┼──────────────────
Web UI (HTML/CSS)      | PDFEditorApi, DOM canvas         | User interaction
JS SDK (file.js)       | drawingfile.wasm (WASM bridge)   | Rendered canvas
WASM module            | CPdfFile, IRenderer               | Pixel buffers
CPdfFile               | CPdfReader, CPdfWriter            | JS API
CPdfReader             | RendererOutputDev, xpdf           | DrawPageOnRenderer
RendererOutputDev      | xpdf OutputDev, IRenderer         | Drawing calls
xpdf (PDFDoc/Gfx)      | PDF binary format, Stream filters | Object graph
IRenderer              | Font engine, image decoders       | RGBA pixels
```

---

## 11. Performance Characteristics

| Operation | Typical time | Notes |
|-----------|-------------|-------|
| PDF parse & XRef load | 50–500ms | Scales with file size |
| Page render (1x zoom) | 50–500ms | Scales with page complexity |
| Pixel → canvas transfer | 5–50ms | JavaScript; depends on resolution |
| Font extraction (first use) | 10–100ms | Cached after first load |
| Page navigation | <10ms | No re-render needed if cached |
| Zoom re-render | 50–500ms | Always re-renders from PDF |

**Optimization patterns in ONLYOFFICE:**
- Page renders are on-demand (no pre-render)
- Font data cached in `CPdfFontList` across all page renders
- No pixel cache — every zoom level triggers a full re-render
- Canvas size clamped on mobile (max ~4096px side)
- Viewport-based lazy loading: only visible pages are rendered

---

## 12. Key Architectural Decisions

**1. RendererOutputDev instead of SplashOutputDev**
- xpdf's SplashOutputDev renders to an internal bitmap
- ONLYOFFICE's RendererOutputDev translates to the `IRenderer` interface
- Reason: `IRenderer` works across browser (canvas), desktop (GDI/Quartz), and print
- Benefit: one PDF interpreter drives all output targets

**2. WASM Pixel Output (not PNG/JPEG)**
- Raw RGBA buffer is returned from C++ to JavaScript
- JavaScript accesses it via a typed array view of WASM heap memory (zero-copy)
- Reason: Avoids encode/decode overhead; canvas.putImageData accepts RGBA directly

**3. Font Cache (CPdfFontList)**
- Fonts are extracted from PDF streams and cached by object reference
- Reason: Font extraction is expensive (decompress stream, parse CFF/TrueType)
- Result: First page render for a font is slow; subsequent pages use cache

**4. Separation of Content Rendering and Annotation Rendering**
- PDF page content (text, images, paths) rendered via RendererOutputDev
- Annotations rendered separately via JS overlay on canvas
- Reason: Annotations can change (user adds highlight) without re-rendering page

**5. Incremental Update Support (CPdfWriter)**
- Modified PDFs are saved as incremental updates (new XRef + appended objects)
- Reason: Faster save; preserves original objects; required for digital signatures
