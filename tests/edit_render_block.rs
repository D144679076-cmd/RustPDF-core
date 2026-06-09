//! Regression test for the edit-text block preview render.
//!
//! `WasmEditor::text_edit_render_block` renders the full page through the real
//! renderer, then crops the clicked block's rectangle. This guards the bug where
//! rendering a *sub-tile* directly mis-mapped content on pages with a top-level
//! flip `cm` — the title's tile rendered the Group-Members text instead.
//!
//! This test mirrors that crop path and asserts the title crop actually contains
//! dark title ink in a single horizontal band (not the whole page, not blank).

#![cfg(all(feature = "render", feature = "writer"))]

use pdf_core::editor::build_text_model;
use pdf_core::render::{render_tile_content, TileRect};
use pdf_core::{document::catalog::Catalog, document::page::Page, PdfDocument};
use std::path::PathBuf;

fn load(name: &str) -> PdfDocument {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    PdfDocument::parse(std::fs::read(&path).unwrap()).unwrap()
}

#[test]
fn title_block_crop_contains_title_ink_in_one_band() {
    let doc = load("Group-3.pdf");
    let model = build_text_model(&doc, 0).expect("build model");
    assert!(!model.blocks.is_empty(), "page 0 should have text blocks");

    // The title is the largest-font block.
    let title = model
        .blocks
        .iter()
        .max_by(|a, b| a.font_size.partial_cmp(&b.font_size).unwrap())
        .expect("a block");

    let catalog = Catalog::from_document(&doc).unwrap();
    let page_dict = catalog.get_page_dict(&doc, 0).unwrap();
    let page = Page::from_dict(&doc, &page_dict).unwrap();
    let mb = page.media_box;
    let page_h = mb.height();

    // Block bounding box in PDF user space (same padding as the WASM path).
    let fs = title.font_size;
    let ascent = fs * 0.85;
    let descent = fs * 0.30;
    let pad = (fs * 0.15).max(1.0);
    let tile = TileRect {
        x: title.x - pad,
        y: title.y - descent,
        width: title.width + 2.0 * pad,
        height: ascent + descent,
    };

    // Render the full page, then crop the block rectangle (the production path).
    let scale = 2.0_f32;
    let full_tile = TileRect {
        x: mb.x1,
        y: mb.y1,
        width: mb.width(),
        height: page_h,
    };
    let (_origin, full) = render_tile_content(
        &doc,
        &page,
        scale,
        full_tile,
        &page.decode_contents(&doc).unwrap(),
    )
    .expect("full render");

    let fw = full.width as i64;
    let fh = full.height as i64;
    let crop_x = (tile.x * scale as f64).round() as i64;
    let crop_y = ((page_h - tile.y - tile.height) * scale as f64).round() as i64;
    let crop_w = (tile.width * scale as f64).round() as i64;
    let crop_h = (tile.height * scale as f64).round() as i64;
    let x0 = crop_x.clamp(0, fw) as usize;
    let y0 = crop_y.clamp(0, fh) as usize;
    let x1 = (crop_x + crop_w).clamp(0, fw) as usize;
    let y1 = (crop_y + crop_h).clamp(0, fh) as usize;
    let out_w = x1 - x0;
    let out_h = y1 - y0;
    assert!(out_w > 100, "title crop too narrow: {}", out_w);
    assert!((1..=200).contains(&out_h), "crop not one line: {}", out_h);

    // Count inked rows inside the crop. A single title line inks a contiguous band
    // — at least a few rows, but not zero (blank) and not every row (whole page).
    let src = full.data();
    let fw_us = full.width as usize;
    let mut inked_rows = 0usize;
    let mut total_ink = 0usize;
    for ry in 0..out_h {
        let mut row_ink = 0usize;
        for rx in 0..out_w {
            let i = ((y0 + ry) * fw_us + (x0 + rx)) * 4;
            let p = &src[i..i + 4];
            // tiny-skia premultiplied: title ink is dark navy on white.
            if p[3] > 10 && (p[0] < 200 || p[1] < 200 || p[2] < 200) {
                row_ink += 1;
            }
        }
        if row_ink > 0 {
            inked_rows += 1;
            total_ink += row_ink;
        }
    }

    assert!(
        total_ink > 50,
        "title crop has too little ink: {}",
        total_ink
    );
    assert!(
        inked_rows >= 5 && inked_rows < out_h,
        "ink band looks wrong: inked_rows={} of {}",
        inked_rows,
        out_h
    );
}
