//! Font fallback resolution for the render pipeline.
//!
//! Resolves font bytes in three tiers:
//!
//! 1. **`RuntimeFontRegistry`** — fonts registered at runtime (used by WASM JS host).
//! 2. **`EmbeddedFontResolver`** — Liberation / DejaVu fonts compiled in via
//!    `include_bytes!`; covers all 14 Standard PDF fonts, always available.
//! 3. **`DirectoryFontResolver`** — native-only; walks a `core-fonts` directory and
//!    serves any of the 186 available TTF/OTF files.

use std::collections::HashMap;

// ─── Public trait ─────────────────────────────────────────────────────────────

/// Resolves a font name to its raw TTF/OTF bytes.
///
/// Implementations must be `Send + Sync` so they can be stored in `PageRenderer`.
pub trait FontResolver: Send + Sync {
    /// Return raw TTF/OTF bytes for `name`, or `None` if the font is unknown.
    ///
    /// `bold` and `italic` are style hints derived from the font name; implementations
    /// may use or ignore them depending on their lookup strategy.
    fn resolve(&self, name: &str, bold: bool, italic: bool) -> Option<Vec<u8>>;
}

// ─── RuntimeFontRegistry ─────────────────────────────────────────────────────

thread_local! {
    static FONT_REGISTRY: std::cell::RefCell<HashMap<String, Vec<u8>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Register a font by name so it is available to any [`FontResolver`] in this thread.
///
/// Intended for WASM callers that inject fonts from the JS host at runtime.
pub fn register_font(name: String, data: Vec<u8>) {
    FONT_REGISTRY.with(|r| r.borrow_mut().insert(name.to_lowercase(), data));
}

fn lookup_registered(name: &str) -> Option<Vec<u8>> {
    FONT_REGISTRY.with(|r| r.borrow().get(&name.to_lowercase()).cloned())
}

/// WASM export: register a font from the JS host.
///
/// Called from JavaScript as:
/// ```js
/// const fontBytes = new Uint8Array(await fetch('/fonts/NotoSans.ttf').then(r => r.arrayBuffer()));
/// const nameBytes = new TextEncoder().encode('NotoSans');
/// const namePtr = module._malloc(nameBytes.length);
/// const dataPtr = module._malloc(fontBytes.length);
/// module.HEAPU8.set(nameBytes, namePtr);
/// module.HEAPU8.set(fontBytes, dataPtr);
/// module._pdf_register_font(namePtr, nameBytes.length, dataPtr, fontBytes.length);
/// module._free(namePtr); module._free(dataPtr);
/// ```
///
/// # Safety
///
/// `name_ptr` and `data_ptr` must each point to a readable region of at least
/// `name_len` / `data_len` bytes that stays valid for the duration of the call.
/// The bytes are copied immediately, so the caller may free both regions once
/// this function returns. Passing a null or dangling pointer, or a length
/// exceeding the actual allocation, is undefined behaviour.
#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn pdf_register_font(
    name_ptr: *const u8,
    name_len: usize,
    data_ptr: *const u8,
    data_len: usize,
) {
    // SAFETY: JS caller guarantees both pointers are valid and their respective
    // lengths are accurate. Both regions must remain valid for the duration of
    // this call; we copy immediately so no lifetime escapes.
    let name = match std::str::from_utf8(std::slice::from_raw_parts(name_ptr, name_len)) {
        Ok(s) => s.to_string(),
        Err(_) => return,
    };
    let data = std::slice::from_raw_parts(data_ptr, data_len).to_vec();
    register_font(name, data);
}

// ─── Embedded font data (Liberation + DejaVu) ─────────────────────────────────

static LIBERATION_SANS_REGULAR: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationSans-Regular.ttf");
static LIBERATION_SANS_BOLD: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationSans-Bold.ttf");
static LIBERATION_SANS_ITALIC: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationSans-Italic.ttf");
static LIBERATION_SANS_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationSans-BoldItalic.ttf");

static LIBERATION_SERIF_REGULAR: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationSerif-Regular.ttf");
static LIBERATION_SERIF_BOLD: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationSerif-Bold.ttf");
static LIBERATION_SERIF_ITALIC: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationSerif-Italic.ttf");
static LIBERATION_SERIF_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationSerif-BoldItalic.ttf");

static LIBERATION_MONO_REGULAR: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationMono-Regular.ttf");
static LIBERATION_MONO_BOLD: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationMono-Bold.ttf");
static LIBERATION_MONO_ITALIC: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationMono-Italic.ttf");
static LIBERATION_MONO_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../commercial-fonts/liberation/LiberationMono-BoldItalic.ttf");

// Best available fallback for Symbol and ZapfDingbats.
static DEJAVU_SANS: &[u8] = include_bytes!("../../../commercial-fonts/dejavu/DejaVuSans.ttf");

/// Display names of every font family the embedded resolver can render distinctly.
///
/// These are the only faces compiled into the WASM build (Liberation Sans/Serif/
/// Mono + DejaVu Sans), exposed so the editor's font picker lists exactly what it
/// can actually render. Aliases that map to the same face (e.g. Arial→Liberation
/// Sans) are listed because users search by familiar name; DejaVu Sans is included
/// for broad Unicode coverage (e.g. Vietnamese). Native builds additionally serve
/// ~186 directory fonts, but those aren't available in the browser.
pub const EMBEDDED_FONT_FAMILIES: &[&str] = &[
    "Helvetica",
    "Arial",
    "Times New Roman",
    "Times-Roman",
    "Courier New",
    "Courier",
    "DejaVu Sans",
];

fn embedded_font_bytes(family: &str, bold: bool, italic: bool) -> Option<&'static [u8]> {
    Some(match (family, bold, italic) {
        // Helvetica / Arial family → Liberation Sans
        ("helvetica" | "arial" | "liberationsans" | "helveticaneue" | "arialmt", false, false) => {
            LIBERATION_SANS_REGULAR
        }
        ("helvetica" | "arial" | "liberationsans" | "helveticaneue" | "arialmt", true, false) => {
            LIBERATION_SANS_BOLD
        }
        ("helvetica" | "arial" | "liberationsans" | "helveticaneue" | "arialmt", false, true) => {
            LIBERATION_SANS_ITALIC
        }
        ("helvetica" | "arial" | "liberationsans" | "helveticaneue" | "arialmt", true, true) => {
            LIBERATION_SANS_BOLD_ITALIC
        }

        // Times / Times New Roman family → Liberation Serif
        ("times" | "timesnewroman" | "timesnewromanps" | "liberationserif", false, false) => {
            LIBERATION_SERIF_REGULAR
        }
        ("times" | "timesnewroman" | "timesnewromanps" | "liberationserif", true, false) => {
            LIBERATION_SERIF_BOLD
        }
        ("times" | "timesnewroman" | "timesnewromanps" | "liberationserif", false, true) => {
            LIBERATION_SERIF_ITALIC
        }
        ("times" | "timesnewroman" | "timesnewromanps" | "liberationserif", true, true) => {
            LIBERATION_SERIF_BOLD_ITALIC
        }

        // Courier / Courier New family → Liberation Mono
        ("courier" | "couriernew" | "liberationmono", false, false) => LIBERATION_MONO_REGULAR,
        ("courier" | "couriernew" | "liberationmono", true, false) => LIBERATION_MONO_BOLD,
        ("courier" | "couriernew" | "liberationmono", false, true) => LIBERATION_MONO_ITALIC,
        ("courier" | "couriernew" | "liberationmono", true, true) => LIBERATION_MONO_BOLD_ITALIC,

        // Symbol and ZapfDingbats — best-effort fallback with DejaVu Sans
        ("symbol" | "zapfdingbats", _, _) => DEJAVU_SANS,

        // Unknown family (e.g. CID names like "CIDFont+F1") — DejaVu Sans covers
        // Latin Extended, Vietnamese, Greek, and Cyrillic as a Unicode fallback.
        _ => DEJAVU_SANS,
    })
}

// ─── Name normalisation helpers ───────────────────────────────────────────────

/// Split a PDF font name into (base_family_lowercase, is_bold, is_italic).
///
/// Handles names like `"Helvetica-BoldOblique"`, `"Times-Roman"`,
/// `"Arial,BoldItalic"`, `"Arial MT"`.
pub fn normalize_font_name(name: &str) -> (String, bool, bool) {
    let lower = name.to_lowercase();
    let bold = lower.contains("bold");
    let italic = lower.contains("italic") || lower.contains("oblique");

    // Everything before the first '-' or ',' is the family name.
    let family_raw = name.split(['-', ',']).next().unwrap_or(name);
    // Strip common suffixes like " MT", " PS" and collapse whitespace.
    let family = family_raw
        .trim_end_matches(" MT")
        .trim_end_matches(" PS")
        .trim();
    let family = family
        .to_lowercase()
        .replace([' ', '_'], "")
        .replace("mt", "")
        .replace("ps", "");

    (family, bold, italic)
}

/// Normalise a font file stem for directory index lookup.
///
/// `"LiberationSans-Regular"` → `"liberationsansregular"`
#[cfg(not(target_arch = "wasm32"))]
fn normalize_for_index(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

// ─── EmbeddedFontResolver ────────────────────────────────────────────────────

/// Resolves fonts using statically embedded Liberation and DejaVu font bytes.
///
/// Covers all 14 Standard PDF fonts plus common aliases. Always available,
/// including in WASM builds.
pub struct EmbeddedFontResolver;

impl FontResolver for EmbeddedFontResolver {
    fn resolve(&self, name: &str, _bold: bool, _italic: bool) -> Option<Vec<u8>> {
        // Runtime-registered fonts take highest priority.
        if let Some(data) = lookup_registered(name) {
            return Some(data);
        }
        let (family, bold, italic) = normalize_font_name(name);
        embedded_font_bytes(&family, bold, italic).map(|b| b.to_vec())
    }
}

// ─── DirectoryFontResolver (native only) ──────────────────────────────────────

/// Resolves fonts by walking a `core-fonts` directory at runtime.
///
/// Covers all 186 fonts in the ONLYOFFICE core-fonts repository.
/// Falls back to [`EmbeddedFontResolver`] for standard PDF fonts when the
/// directory does not contain a direct name match.
///
/// Not available in WASM builds — use `pdf_register_font` from the JS host
/// to supply additional fonts at runtime.
#[cfg(not(target_arch = "wasm32"))]
pub struct DirectoryFontResolver {
    /// Normalised font name → path on disk.
    index: HashMap<String, std::path::PathBuf>,
    fallback: EmbeddedFontResolver,
}

#[cfg(not(target_arch = "wasm32"))]
impl DirectoryFontResolver {
    /// Build a resolver by walking `root` (the `core-fonts` directory).
    ///
    /// All `.ttf` and `.otf` files found recursively are indexed; path
    /// errors are logged and skipped.
    pub fn new(root: &std::path::Path) -> Self {
        let mut index = HashMap::new();
        walk_fonts(root, &mut index);
        log::debug!(
            "DirectoryFontResolver: indexed {} fonts from {}",
            index.len(),
            root.display()
        );
        DirectoryFontResolver {
            index,
            fallback: EmbeddedFontResolver,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl FontResolver for DirectoryFontResolver {
    fn resolve(&self, name: &str, bold: bool, italic: bool) -> Option<Vec<u8>> {
        // Runtime-registered fonts take highest priority.
        if let Some(data) = lookup_registered(name) {
            return Some(data);
        }
        // EmbeddedFontResolver handles the 14 Standard PDF fonts and common aliases.
        if let Some(data) = self.fallback.resolve(name, bold, italic) {
            return Some(data);
        }
        // Try direct normalised-name lookup against the directory index.
        let key = normalize_for_index(name);
        if let Some(path) = self.index.get(&key) {
            match std::fs::read(path) {
                Ok(data) => return Some(data),
                Err(e) => log::warn!(
                    "font read failed for '{}' at {}: {}",
                    name,
                    path.display(),
                    e
                ),
            }
        }
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn walk_fonts(root: &std::path::Path, index: &mut HashMap<String, std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_fonts(&path, index);
        } else {
            let ext = path
                .extension()
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default();
            if ext == "ttf" || ext == "otf" {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    // First file wins for any given normalised key.
                    index.entry(normalize_for_index(stem)).or_insert(path);
                }
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_font_name_standard() {
        let cases = [
            ("Helvetica", "helvetica", false, false),
            ("Helvetica-Bold", "helvetica", true, false),
            ("Helvetica-Oblique", "helvetica", false, true),
            ("Helvetica-BoldOblique", "helvetica", true, true),
            ("Times-Roman", "times", false, false),
            ("Times-Bold", "times", true, false),
            ("Times-Italic", "times", false, true),
            ("Times-BoldItalic", "times", true, true),
            ("Courier", "courier", false, false),
            ("Courier-Bold", "courier", true, false),
            ("Courier-Oblique", "courier", false, true),
            ("Courier-BoldOblique", "courier", true, true),
            ("Symbol", "symbol", false, false),
            ("ZapfDingbats", "zapfdingbats", false, false),
        ];
        for (input, expected_family, expected_bold, expected_italic) in cases {
            let (family, bold, italic) = normalize_font_name(input);
            assert_eq!(family, expected_family, "family mismatch for '{}'", input);
            assert_eq!(bold, expected_bold, "bold mismatch for '{}'", input);
            assert_eq!(italic, expected_italic, "italic mismatch for '{}'", input);
        }
    }

    #[test]
    fn test_embedded_resolver_covers_all_14_standard_fonts() {
        let resolver = EmbeddedFontResolver;
        let standard_fonts = [
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-Oblique",
            "Helvetica-BoldOblique",
            "Times-Roman",
            "Times-Bold",
            "Times-Italic",
            "Times-BoldItalic",
            "Courier",
            "Courier-Bold",
            "Courier-Oblique",
            "Courier-BoldOblique",
            "Symbol",
            "ZapfDingbats",
        ];
        for name in standard_fonts {
            let result = resolver.resolve(name, false, false);
            assert!(
                result.is_some(),
                "EmbeddedFontResolver should resolve standard font '{}'",
                name
            );
            assert!(
                result.unwrap().len() > 1000,
                "font data for '{}' looks too small",
                name
            );
        }
    }

    #[test]
    fn test_embedded_resolver_common_aliases() {
        let resolver = EmbeddedFontResolver;
        for name in ["Arial", "TimesNewRoman", "CourierNew"] {
            assert!(
                resolver.resolve(name, false, false).is_some(),
                "should resolve alias '{}'",
                name
            );
        }
    }

    #[test]
    fn test_runtime_registry_round_trip() {
        register_font("TestFont".to_string(), vec![1, 2, 3, 4]);
        let result = lookup_registered("testfont");
        assert_eq!(result, Some(vec![1, 2, 3, 4]));
        // Resolver also picks it up.
        let resolver = EmbeddedFontResolver;
        assert!(resolver.resolve("TestFont", false, false).is_some());
        // Clean up to avoid affecting other tests.
        FONT_REGISTRY.with(|r| r.borrow_mut().remove("testfont"));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_directory_resolver_indexes_core_fonts() {
        // Prefer the commercial-grade font directory; fall back to core-fonts for dev builds.
        let commercial =
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../commercial-fonts"));
        let legacy = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../core-fonts"));
        let font_dir = if commercial.exists() {
            commercial
        } else if legacy.exists() {
            legacy
        } else {
            return; // Skip if neither directory is present in CI
        };
        let resolver = DirectoryFontResolver::new(font_dir);
        assert!(
            resolver.index.len() > 100,
            "expected >100 fonts indexed, got {}",
            resolver.index.len()
        );
        // Standard fonts should still resolve via the embedded fallback.
        assert!(resolver.resolve("Helvetica", false, false).is_some());
    }
}
