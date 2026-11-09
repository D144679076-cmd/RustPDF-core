//! Associated Files and portable file specifications (ISO 32000-2 §7.11.3/§7.11.4).
//!
//! PDF 2.0 tightens both structures:
//! - `FileSpec` dictionaries carry `/UF` (Unicode filename, *required* in
//!   PDF 2.0) alongside `/F` (ASCII name).
//! - `/AFRelationship` classifies the file's role: `Source`, `Data`,
//!   `Supplement`, `EncryptedPayload`, `FormData`, `Schema`, `Unspecified`.
//!   Unknown values degrade to `Unspecified` with a warning — never an error.
//! - `/AF` arrays may live at **document level** (Catalog `/AF`) and
//!   **page level** (page dict `/AF`) — both parsed here.
//!
//! Reader-first per the plan; the writer helper lives in `writer/document.rs`
//! (`PdfWriter::add_associated_file`).

use crate::error::{PdfError, Result};
use crate::parser::objects::{PdfDict, PdfDocument, PdfObject};

use super::text_string::decode_pdf_text_string;

/// `/AFRelationship` values (ISO 32000-2 Table 43).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfRelationship {
    /// The file is the original source material for the content.
    Source,
    /// Source data not suitable for long-term archiving (spreadsheets, exports).
    Data,
    /// A file that is supportive of the content but not its data.
    Supplement,
    /// Encrypted payload data delivered under the security envelope.
    EncryptedPayload,
    /// Form data delivered as a PDF/X interactive form.
    FormData,
    /// Model/schema data (e.g. for content refinement).
    Schema,
    /// No relationship claim (also the fallback for unknown values).
    Unspecified,
}

impl AfRelationship {
    /// Parse the `/AFRelationship` name. Unknown names → `Unspecified` + warn
    /// (forward-compatible extension values are tolerated, never errors).
    pub fn from_name(name: &str) -> Self {
        match name {
            "Source" => AfRelationship::Source,
            "Data" => AfRelationship::Data,
            "Supplement" => AfRelationship::Supplement,
            "EncryptedPayload" => AfRelationship::EncryptedPayload,
            "FormData" => AfRelationship::FormData,
            "Schema" => AfRelationship::Schema,
            "Unspecified" => AfRelationship::Unspecified,
            other => {
                log::warn!(
                    "Unknown /AFRelationship '{}' — treated as Unspecified",
                    other
                );
                AfRelationship::Unspecified
            }
        }
    }

    /// The PDF name spelling of this relationship (writer-side).
    pub fn as_name(self) -> &'static str {
        match self {
            AfRelationship::Source => "Source",
            AfRelationship::Data => "Data",
            AfRelationship::Supplement => "Supplement",
            AfRelationship::EncryptedPayload => "EncryptedPayload",
            AfRelationship::FormData => "FormData",
            AfRelationship::Schema => "Schema",
            AfRelationship::Unspecified => "Unspecified",
        }
    }
}

/// `/Params` of an `/EmbeddedFile` stream (ISO 32000-2 Table 44).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmbeddedFileParams {
    /// `/Size` — size of the uncompressed embedded file, in bytes.
    pub size: Option<u64>,
    /// `/CreationDate` — PDF date text string (`D:…`).
    pub creation_date: Option<String>,
    /// `/ModDate` — PDF date text string (`D:…`).
    pub mod_date: Option<String>,
    /// `/CheckSum` — 16-byte MD5 of the uncompressed file (hex-exposed).
    pub checksum_hex: Option<String>,
}

/// An `/EmbeddedFile` stream resolved with its `/Params` metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedFile {
    /// Stream `/Subtype` when present (e.g. `text/plain`, name-escaped).
    pub subtype: Option<String>,
    /// Uncompressed file bytes (filters + encryption applied).
    pub data: Vec<u8>,
    /// Decoded `/Params` dictionary.
    pub params: EmbeddedFileParams,
}

/// A portable file-specification dictionary (`/Type /Filespec`, §7.11.3).
#[derive(Debug, Clone, PartialEq)]
pub struct FileSpec {
    /// `/F` — ASCII file name (may be absent in UF-only writers).
    pub name_f: Option<String>,
    /// `/UF` — Unicode file name, **required in PDF 2.0 conforming files**.
    pub name_uf: Option<String>,
    /// `/Desc` — human-facing description.
    pub description: Option<String>,
    /// `/AFRelationship` — `Unspecified` when absent or unknown.
    pub relationship: AfRelationship,
    /// Content of `/EF` when the spec embeds actual data.
    pub embedded: Option<EmbeddedFile>,
}

impl FileSpec {
    /// Best available display name: `/UF` first, `/F` fallback (§7.11.3 prefers
    /// the Unicode name for presentation).
    pub fn display_name(&self) -> Option<&str> {
        self.name_uf.as_deref().or(self.name_f.as_deref())
    }
}

/// Parse all file specs referenced by an `/AF` entry (Catalog- or page-level).
///
/// `/AF` may be an array of file-spec dicts/refs or a single spec dict.
/// Entries that don't resolve to file-spec dictionaries are skipped with a
/// warning — a broken `/AF` array must never fail document open (R1).
pub fn parse_af_entry(doc: &PdfDocument, af_obj: &PdfObject) -> Result<Vec<FileSpec>> {
    let resolved = doc.resolve(af_obj)?;
    let entries: Vec<PdfObject> = match resolved {
        PdfObject::Array(arr) => arr,
        PdfObject::Dictionary(_) => vec![resolved], // unarrayed single spec
        PdfObject::Null => Vec::new(),
        other => {
            log::warn!(
                "/AF entry of unexpected kind ({:?}) — ignored",
                mem_desc(&other)
            );
            Vec::new()
        }
    };

    let mut specs = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let resolved_entry = doc.resolve(entry)?;
        if let PdfObject::Dictionary(d) = resolved_entry {
            specs.push(parse_file_spec_dict(doc, &d)?);
        } else {
            log::warn!("/AF entry {} is not a file spec dictionary — skipped", i);
        }
    }
    Ok(specs)
}

/// Enumerate the document-level associated files (`Catalog /AF`).
pub fn catalog_associated_files(doc: &PdfDocument, catalog: &PdfDict) -> Result<Vec<FileSpec>> {
    match catalog.get("AF") {
        Some(af) => parse_af_entry(doc, af),
        None => Ok(Vec::new()),
    }
}

/// Enumerate the page-level associated files (page dict `/AF`).
pub fn page_associated_files(doc: &PdfDocument, page_dict: &PdfDict) -> Result<Vec<FileSpec>> {
    match page_dict.get("AF") {
        Some(af) => parse_af_entry(doc, af),
        None => Ok(Vec::new()),
    }
}

/// Parse one `/Type /Filespec` dictionary into a [`FileSpec`].
///
/// `/EF` stream decode failure degrades to `embedded = None` with the decoded
/// metadata intact (metadata must survive content problems, R1).
pub(crate) fn parse_file_spec_dict(doc: &PdfDocument, dict: &PdfDict) -> Result<FileSpec> {
    let name_f = dict_obj_string(dict, "F").map(|s| sanitize_name(decode_pdf_text_string(&s)));
    let name_uf = dict_obj_string(dict, "UF").map(|s| sanitize_name(decode_pdf_text_string(&s)));
    let description = dict_obj_string(dict, "Desc").map(|s| decode_pdf_text_string(&s));
    let relationship = match dict.get("AFRelationship") {
        Some(PdfObject::Name(n)) => AfRelationship::from_name(n),
        // Absent / Null is legal (v1.x-era AF entries predate the key).
        _ => AfRelationship::Unspecified,
    };

    let embedded = match dict.get("EF") {
        Some(ef) => match parse_embedded_file(doc, ef) {
            Ok(ef) => Some(ef),
            Err(e) => {
                log::warn!("EmbeddedFile decode skipped: {}", e);
                None
            }
        },
        None => None,
    };

    Ok(FileSpec {
        name_f,
        name_uf,
        description,
        relationship,
        embedded,
    })
}

/// Resolve `/EF → { F, UF }` to the EmbeddedFile stream and decode its content.
///
/// Indirect streams go through [`PdfDocument::get_stream_data`] so encryption
/// and filters are applied; inline (unreferenced) streams decode directly.
fn parse_embedded_file(doc: &PdfDocument, ef_obj: &PdfObject) -> Result<EmbeddedFile> {
    let ef_dict = match doc.resolve(ef_obj)? {
        PdfObject::Dictionary(d) => d,
        _ => return Err(PdfError::invalid_structure("/EF is not a dictionary")),
    };

    // /EF maps "file role" → stream ref; F is always present, UF duplicates
    // only the Unicode-name side (§7.11.4). Read F, fall back to UF.
    let stream_ref = ef_dict
        .get("F")
        .or_else(|| ef_dict.get("UF"))
        .ok_or_else(|| {
            PdfError::invalid_structure("/EF dictionary has no 'F' or 'UF' stream entry")
        })?;

    let stream_obj = doc.resolve(stream_ref)?;
    let stream_dict = stream_obj
        .as_dict()
        .ok_or_else(|| PdfError::invalid_structure("/EF target is not a stream"))?
        .clone();

    let subtype = stream_dict
        .get("Subtype")
        .and_then(|s| s.as_name())
        .map(decode_pdf_name);

    let params = decode_embedded_file_params(doc, stream_dict.get("Params"));

    let data = match &stream_obj {
        PdfObject::Stream(_) => match stream_ref {
            PdfObject::Reference(id, _) => doc.get_stream_data(*id)?,
            _ => match stream_obj {
                // Inline stream: decode directly through the filter pipeline.
                PdfObject::Stream(s) => s.decode_with_doc(doc)?,
                _ => unreachable!("variant matched above"),
            },
        },
        _ => {
            return Err(PdfError::invalid_structure(
                "/EF target did not resolve to a stream",
            ))
        }
    };

    Ok(EmbeddedFile {
        subtype,
        data,
        params,
    })
}

/// Decode the `/Params` dictionary of an EmbeddedFile stream.
fn decode_embedded_file_params(
    doc: &PdfDocument,
    params_obj: Option<&PdfObject>,
) -> EmbeddedFileParams {
    let Some(obj) = params_obj else {
        return EmbeddedFileParams::default();
    };

    let resolved = match obj {
        PdfObject::Reference(..) => doc.resolve(obj).ok(),
        other => Some(other.clone()),
    };

    let Some(PdfObject::Dictionary(params)) = resolved else {
        return EmbeddedFileParams::default();
    };

    EmbeddedFileParams {
        size: match params.get("Size") {
            Some(PdfObject::Integer(n)) if *n >= 0 => Some(*n as u64),
            _ => None,
        },
        creation_date: params.get("CreationDate").and_then(obj_as_text),
        mod_date: params.get("ModDate").and_then(obj_as_text),
        checksum_hex: params.get("CheckSum").and_then(obj_as_text),
    }
}

/// Pull a string payload out of a dictionary entry (direct values only).
fn dict_obj_string(dict: &PdfDict, key: &str) -> Option<Vec<u8>> {
    match dict.get(key) {
        Some(PdfObject::String(bytes)) => Some(bytes.clone()),
        _ => None,
    }
}

/// Text-string payload of an object.
fn obj_as_text(obj: &PdfObject) -> Option<String> {
    match obj {
        PdfObject::String(bytes) => Some(decode_pdf_text_string(bytes)),
        _ => None,
    }
}

/// PDF names hex-escape non-ASCII bytes as `#XX` (§7.3.5). Decode those for
/// display (`text#2Fplain` → `text/plain`); unescapable fragments pass through.
fn decode_pdf_name(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("#0");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Legacy Windows paths in file names surface as backslashes; normalize to
/// forward slashes for display consistency.
fn sanitize_name(s: String) -> String {
    s.replace('\\', "/")
}

/// Human-writable discriminant label for warns (PdfDict has no Debug derive
/// problem, but a short label keeps logs readable).
fn mem_desc(obj: &PdfObject) -> &'static str {
    match obj {
        PdfObject::Null => "null",
        PdfObject::Boolean(_) => "boolean",
        PdfObject::Integer(_) => "integer",
        PdfObject::Real(_) => "real",
        PdfObject::String(_) => "string",
        PdfObject::Name(_) => "name",
        PdfObject::Array(_) => "array",
        PdfObject::Dictionary(_) => "dictionary",
        PdfObject::Stream(_) => "stream",
        PdfObject::Reference(..) => "reference",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::catalog::Catalog;

    /// Build a minimal PDF; xref offsets computed programmatically. Objects
    /// must be numbered sequentially 1..n.
    fn build_pdf(objects: &[(u32, &[u8])]) -> Vec<u8> {
        let mut pdf = Vec::from(b"%PDF-2.0\n" as &[u8]);
        let mut offsets: Vec<(u32, usize)> = Vec::new();
        for (i, (num, body)) in objects.iter().enumerate() {
            assert_eq!(
                *num,
                (i + 1) as u32,
                "fixtures must number 1..n sequentially"
            );
            offsets.push((*num, pdf.len()));
            pdf.extend_from_slice(format!("{} 0 obj\n", num).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let max_id = offsets.last().map(|(n, _)| *n).unwrap_or(0);
        let xref_pos = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", max_id + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \r\n");
        for (_, off) in &offsets {
            pdf.extend_from_slice(format!("{:010} 00000 n \r\n", off).as_bytes());
        }
        let trailer = format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            max_id + 1,
            xref_pos
        );
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn af_objects() -> Vec<(u32, &'static [u8])> {
        vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AF [6 0 R] >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /AF [7 0 R] >>",
            ),
            (
                4,
                b"<< /Type /EmbeddedFile /Subtype /text#2Fplain /Params << /Size 11 /ModDate (D:20260207120000) >> /Length 11 >>\nstream\nHelloWorld!\nendstream",
            ),
            (
                5,
                b"<< /Type /EmbeddedFile /Subtype /text#2Fplain /Params << /Size 7 >> /Length 7 >>\nstream\nABCDEFG\nendstream",
            ),
            (
                6,
                b"<< /Type /Filespec /F (source.csv) /Desc (raw data) /AFRelationship /Data /EF << /F 4 0 R >> >>",
            ),
            (
                7,
                b"<< /Type /Filespec /F (page.cache) /AFRelationship /Supplement /EF << /F 5 0 R >> >>",
            ),
        ]
    }

    fn doc_with_af() -> PdfDocument {
        PdfDocument::parse(build_pdf(&af_objects())).unwrap()
    }

    // --- happy paths ----------------------------------------------------------

    #[test]
    fn test_document_level_af_parsed() {
        let doc = doc_with_af();
        let cat = Catalog::from_document(&doc).unwrap();
        let specs = cat.associated_files(&doc).unwrap();
        assert_eq!(specs.len(), 1);
        let fs = &specs[0];
        assert_eq!(fs.name_f.as_deref(), Some("source.csv"));
        assert_eq!(fs.relationship, AfRelationship::Data);
        assert_eq!(fs.description.as_deref(), Some("raw data"));
        let ef = fs.embedded.as_ref().expect("embedded file present");
        assert_eq!(ef.data, b"HelloWorld!");
        assert_eq!(ef.subtype.as_deref(), Some("text/plain"));
        assert_eq!(ef.params.size, Some(11));
        assert_eq!(ef.params.mod_date.as_deref(), Some("D:20260207120000"));
    }

    #[test]
    fn test_page_level_af_parsed() {
        let doc = doc_with_af();
        let cat = Catalog::from_document(&doc).unwrap();
        let page = cat.get_page_dict(&doc, 0).unwrap();
        let specs = page_associated_files(&doc, &page).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name_f.as_deref(), Some("page.cache"));
        assert_eq!(specs[0].relationship, AfRelationship::Supplement);
        assert_eq!(specs[0].embedded.as_ref().unwrap().data, b"ABCDEFG");
    }

    #[test]
    fn test_display_name_prefers_uf() {
        let mut dict: PdfDict = PdfDict::new();
        dict.insert("F".to_string(), PdfObject::String(b"ascii.bin".to_vec()));
        dict.insert(
            "UF".to_string(),
            PdfObject::String(
                [
                    vec![0xFE, 0xFF],
                    "unicode.bin"
                        .encode_utf16()
                        .flat_map(|c| c.to_be_bytes())
                        .collect(),
                ]
                .concat(),
            ),
        );
        let doc = doc_with_af();
        let fs = parse_file_spec_dict(&doc, &dict).unwrap();
        assert_eq!(fs.name_uf.as_deref(), Some("unicode.bin"));
        assert_eq!(fs.display_name(), Some("unicode.bin"));
    }

    // --- edge / degradation paths ---------------------------------------------

    #[test]
    fn test_no_af_entry_is_empty() {
        // No /AF at all: catalog + page both yield empty.
        let objs: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
        ];
        let doc = PdfDocument::parse(build_pdf(&objs)).unwrap();
        let cat = Catalog::from_document(&doc).unwrap();
        assert!(cat.associated_files(&doc).unwrap().is_empty());
        let page = cat.get_page_dict(&doc, 0).unwrap();
        assert!(page_associated_files(&doc, &page).unwrap().is_empty());
    }

    #[test]
    fn test_filespec_without_ef_yields_unembedded() {
        let doc = doc_with_af();
        let mut dict: PdfDict = PdfDict::new();
        dict.insert("Type".to_string(), PdfObject::Name("Filespec".to_string()));
        dict.insert("F".to_string(), PdfObject::String(b"meta.txt".to_vec()));
        let spec = parse_file_spec_dict(&doc, &dict).unwrap();
        assert_eq!(spec.name_f.as_deref(), Some("meta.txt"));
        assert!(spec.embedded.is_none());
        assert_eq!(spec.relationship, AfRelationship::Unspecified);
    }

    #[test]
    fn test_filespec_with_broken_ef_keeps_metadata() {
        // /EF present but its target does not resolve to a stream — metadata
        // survives, embedded degrades to None (never bubbles, R1).
        let doc = doc_with_af();
        let mut dict: PdfDict = PdfDict::new();
        dict.insert("Type".to_string(), PdfObject::Name("Filespec".to_string()));
        dict.insert("F".to_string(), PdfObject::String(b"broken.csv".to_vec()));
        dict.insert(
            "AFRelationship".to_string(),
            PdfObject::Name("FormData".to_string()),
        );
        dict.insert(
            "EF".to_string(),
            PdfObject::Dictionary(
                [("F".to_string(), PdfObject::Integer(999))]
                    .into_iter()
                    .collect(),
            ),
        );
        let spec = parse_file_spec_dict(&doc, &dict).unwrap();
        assert_eq!(spec.relationship, AfRelationship::FormData);
        assert!(spec.embedded.is_none());
    }

    #[test]
    fn test_unknown_afrelationship_degrades_with_warn() {
        assert_eq!(
            AfRelationship::from_name("FutureExtension"),
            AfRelationship::Unspecified
        );
        assert_eq!(AfRelationship::from_name("Schema"), AfRelationship::Schema);
    }

    #[test]
    fn test_wrong_typed_entries_skipped() {
        // /AF entry that resolves to an integer (not a Filespec dict) → skipped.
        let objs: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AF [3 0 R 999] >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, b"<< /Type /Filespec /F (a.bin) >>"),
        ];
        let doc = PdfDocument::parse(build_pdf(&objs)).unwrap();
        let cat = Catalog::from_document(&doc).unwrap();
        let specs = cat.associated_files(&doc).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name_f.as_deref(), Some("a.bin"));
    }

    #[test]
    fn test_single_unarrayed_af_entry() {
        let objs: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AF 3 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Filespec /F (direct.bin) /AFRelationship /Source >>",
            ),
        ];
        let doc = PdfDocument::parse(build_pdf(&objs)).unwrap();
        let cat = Catalog::from_document(&doc).unwrap();
        let specs = cat.associated_files(&doc).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].relationship, AfRelationship::Source);
    }
}
