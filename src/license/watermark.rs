//! Trial watermark applied on save when no Pro/Enterprise license is active.
//!
//! Burns a diagonal "UNLICENSED — pdf-core trial" text overlay onto every page
//! using the standard Helvetica font (always available in PDF viewers). The
//! overlay is appended as an additional content stream so it does not disturb
//! existing page content.

use crate::editor::PdfEditor;
use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::content_builder::ContentBuilder;
use crate::writer::streams::make_flate_stream;

const WATERMARK_TEXT: &str = "UNLICENSED - pdf-core trial";

/// Apply a diagonal "UNLICENSED — pdf-core trial" overlay to every page.
///
/// The watermark is light gray, 45° rotated, centred on each page, using
/// the standard `/Helv` (Helvetica) font. It is appended to `/Contents` so
/// it renders on top of all existing page content.
pub fn apply_trial_watermark(editor: &mut PdfEditor) -> Result<()> {
    let page_count = editor.page_count()?;
    for i in 0..page_count {
        apply_to_page(editor, i)?;
    }
    Ok(())
}

fn apply_to_page(editor: &mut PdfEditor, page_index: usize) -> Result<()> {
    let (page_id, page_dict) = editor.get_page_dict(page_index)?;

    let (pw, ph) = page_dimensions(&page_dict);
    let cx = pw / 2.0;
    let cy = ph / 2.0;
    // Font size scales with page width; clamp to a readable range.
    let font_size = (pw / 12.0).clamp(18.0, 36.0);
    // Rough approximation: Helvetica average glyph width ≈ 0.5 em.
    let approx_text_width = font_size * 0.5 * WATERMARK_TEXT.len() as f64;

    // Build a content stream that:
    //   1. Saves graphics state.
    //   2. Applies a 45° CTM around the page centre.
    //   3. Draws light-gray text centred at the origin (now at page centre).
    //   4. Restores graphics state.
    let mut cb = ContentBuilder::new();
    cb.save()
        .concat_matrix(0.707, 0.707, -0.707, 0.707, cx, cy)
        .set_fill_gray(0.75)
        .begin_text()
        .set_font("Helv", font_size)
        .move_text_pos(-approx_text_width / 2.0, -font_size / 2.0)
        .show_text(WATERMARK_TEXT.as_bytes())
        .end_text()
        .restore();

    let bytes = cb.build();
    let stream = make_flate_stream(&bytes, PdfDict::new())?;
    let stream_id = editor.add_object(PdfObject::Stream(Box::new(stream)));

    // Append the watermark stream ref to /Contents (handles array, single ref,
    // or absent /Contents).
    let mut updated_page = page_dict.clone();
    let new_contents = match updated_page.get("Contents") {
        Some(PdfObject::Array(arr)) => {
            let mut a = arr.clone();
            a.push(PdfObject::Reference(stream_id, 0));
            PdfObject::Array(a)
        }
        Some(single) => PdfObject::Array(vec![single.clone(), PdfObject::Reference(stream_id, 0)]),
        None => PdfObject::Reference(stream_id, 0),
    };
    updated_page.insert("Contents".to_owned(), new_contents);
    editor.replace_object(page_id, PdfObject::Dictionary(updated_page));
    Ok(())
}

fn page_dimensions(page_dict: &PdfDict) -> (f64, f64) {
    match page_dict.get("MediaBox") {
        Some(PdfObject::Array(a)) if a.len() == 4 => {
            let w = to_f64(&a[2]) - to_f64(&a[0]);
            let h = to_f64(&a[3]) - to_f64(&a[1]);
            (w, h)
        }
        _ => (612.0, 792.0), // US Letter default
    }
}

fn to_f64(o: &PdfObject) -> f64 {
    match o {
        PdfObject::Real(r) => *r,
        PdfObject::Integer(i) => *i as f64,
        _ => 0.0,
    }
}
