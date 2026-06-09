use std::path::PathBuf;

use pdf_core::display::{DisplayItem, DisplayList};
use pdf_core::document::catalog::Catalog;
use pdf_core::document::page::Page;
use pdf_core::parser::objects::{PdfDocument, PdfObject};
use pdf_core::text::TextExtractor;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: pdf-inspect <input.pdf> [output.md]");
        std::process::exit(1);
    }

    let input_path = PathBuf::from(&args[1]);
    let output_path = if args.len() >= 3 {
        PathBuf::from(&args[2])
    } else {
        let stem = input_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        input_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(format!("{}_inspect.md", stem))
    };

    let bytes = std::fs::read(&input_path)
        .map_err(|e| format!("Failed to read '{}': {}", input_path.display(), e))?;

    let file_size = bytes.len();

    let doc = PdfDocument::parse(bytes)
        .map_err(|e| format!("Failed to parse '{}': {}", input_path.display(), e))?;

    let catalog =
        Catalog::from_document(&doc).map_err(|e| format!("Failed to read catalog: {}", e))?;

    let mut out = String::new();

    // Header
    out.push_str(&format!(
        "# PDF Inspect: {}\n\n",
        input_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
    ));

    // Document info
    out.push_str("## Document Info\n\n");
    out.push_str(&format!("- File size: {} bytes\n", file_size));
    out.push_str(&format!("- Page count: {}\n", catalog.page_count));

    let trailer_keys: Vec<&str> = doc.trailer.keys().map(|k| k.as_str()).collect();
    let mut trailer_keys_sorted = trailer_keys.clone();
    trailer_keys_sorted.sort();
    out.push_str(&format!(
        "- Trailer keys: {}\n\n",
        trailer_keys_sorted.join(", ")
    ));

    // Trailer dictionary
    out.push_str("## Trailer Dictionary\n\n");
    let mut trailer_entries: Vec<(&String, &PdfObject)> = doc.trailer.iter().collect();
    trailer_entries.sort_by_key(|(k, _)| k.as_str());
    for (key, val) in &trailer_entries {
        out.push_str(&format!("- `{}`: {}\n", key, fmt_object(val)));
    }
    out.push('\n');

    // Pages
    for i in 0..catalog.page_count {
        out.push_str(&format!("## Page {}\n\n", i + 1));

        let page_dict = catalog
            .get_page_dict(&doc, i)
            .map_err(|e| format!("Failed to get page {} dict: {}", i + 1, e))?;

        let page = Page::from_dict(&doc, &page_dict)
            .map_err(|e| format!("Failed to build page {}: {}", i + 1, e))?;

        let mb = page.media_box;
        out.push_str(&format!(
            "- MediaBox: [{} {} {} {}]  ({:.1} × {:.1} pt)\n",
            mb.x1,
            mb.y1,
            mb.x2,
            mb.y2,
            mb.width(),
            mb.height()
        ));
        out.push_str(&format!("- Rotation: {}\n", page.rotate));
        out.push('\n');

        out.push_str("### Extracted Text\n\n");

        match TextExtractor::extract_from_page(&doc, &page) {
            Ok(extractor) => {
                let lines = extractor.into_lines();
                if lines.is_empty() {
                    out.push_str("_(no text found)_\n");
                } else {
                    for line in &lines {
                        out.push_str(&line.text());
                        out.push('\n');
                    }
                }
            }
            Err(e) => {
                out.push_str(&format!("_(text extraction error: {})_\n", e));
            }
        }

        out.push('\n');

        out.push_str("### Display List\n\n");

        match DisplayList::from_page(&doc, &page) {
            Ok(list) => {
                if list.is_empty() {
                    out.push_str("_(no display items)_\n");
                } else {
                    out.push_str(&format!(
                        "Total items: {} (text: {}, images: {})\n\n",
                        list.len(),
                        list.text_items().count(),
                        list.image_items().count(),
                    ));
                    for (idx, item) in list.items.iter().enumerate() {
                        match item {
                            DisplayItem::StrokePath { path, style, ctm } => {
                                out.push_str(&format!(
                                    "- [{}] StrokePath  segments={} color={} lw={:.2} alpha={:.2} ctm=[{:.2},{:.2},{:.2},{:.2},{:.2},{:.2}]\n",
                                    idx,
                                    path.segments.len(),
                                    fmt_color_rgba(&style.color),
                                    style.line_width,
                                    style.alpha,
                                    ctm.a, ctm.b, ctm.c, ctm.d, ctm.e, ctm.f,
                                ));
                            }
                            DisplayItem::FillPath {
                                path,
                                style,
                                rule,
                                ctm,
                            } => {
                                out.push_str(&format!(
                                    "- [{}] FillPath    segments={} color={} rule={:?} alpha={:.2} ctm=[{:.2},{:.2},{:.2},{:.2},{:.2},{:.2}]\n",
                                    idx,
                                    path.segments.len(),
                                    fmt_color_rgba(&style.color),
                                    rule,
                                    style.alpha,
                                    ctm.a, ctm.b, ctm.c, ctm.d, ctm.e, ctm.f,
                                ));
                            }
                            DisplayItem::DrawText(t) => {
                                out.push_str(&format!(
                                    "- [{}] DrawText    {:?} x={:.2} y={:.2} size={:.2} font={} color={} alpha={:.2}\n",
                                    idx,
                                    t.text,
                                    t.x,
                                    t.y,
                                    t.font_size,
                                    t.font_name,
                                    fmt_color_rgba(&t.color),
                                    t.alpha,
                                ));
                            }
                            DisplayItem::DrawImage(img) => {
                                out.push_str(&format!(
                                    "- [{}] DrawImage   bytes={} blend={} alpha={:.2} ctm=[{:.2},{:.2},{:.2},{:.2},{:.2},{:.2}]\n",
                                    idx,
                                    img.data.len(),
                                    img.blend_mode,
                                    img.alpha,
                                    img.ctm.a, img.ctm.b, img.ctm.c, img.ctm.d, img.ctm.e, img.ctm.f,
                                ));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                out.push_str(&format!("_(display list error: {})_\n", e));
            }
        }

        out.push('\n');
    }

    std::fs::write(&output_path, &out)
        .map_err(|e| format!("Failed to write '{}': {}", output_path.display(), e))?;

    println!("Written to {}", output_path.display());
    Ok(())
}

/// Render a PdfObject as a compact readable string for the report.
fn fmt_object(obj: &PdfObject) -> String {
    match obj {
        PdfObject::Null => "null".to_string(),
        PdfObject::Boolean(b) => b.to_string(),
        PdfObject::Integer(n) => n.to_string(),
        PdfObject::Real(r) => format!("{:.4}", r),
        PdfObject::String(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) => format!("\"{}\"", s.escape_default()),
            Err(_) => format!("<{}>", hex_encode(bytes)),
        },
        PdfObject::Name(n) => format!("/{}", n),
        PdfObject::Array(arr) => {
            let items: Vec<String> = arr.iter().map(fmt_object).collect();
            format!("[{}]", items.join(" "))
        }
        PdfObject::Dictionary(d) => {
            let mut keys: Vec<&String> = d.keys().collect();
            keys.sort();
            let pairs: Vec<String> = keys
                .iter()
                .map(|k| format!("/{} {}", k, fmt_object(&d[*k])))
                .collect();
            format!("<< {} >>", pairs.join("  "))
        }
        PdfObject::Stream(s) => {
            let len = s.raw_data.len();
            let filter = s.filter_names().join(",");
            if filter.is_empty() {
                format!("stream({} bytes)", len)
            } else {
                format!("stream({} bytes, filter={})", len, filter)
            }
        }
        PdfObject::Reference(id, gen) => format!("{} {} R", id, gen),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn fmt_color_rgba(color: &pdf_core::content::graphics_state::Color) -> String {
    let [r, g, b, a] = color.to_rgba();
    format!("rgba({},{},{},{})", r, g, b, a)
}
