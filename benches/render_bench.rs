/// Criterion benchmarks for the pdf-core rendering pipeline.
///
/// Run with:
///   cargo bench --features render --bench render_bench
use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, Criterion};
use pdf_core::{
    document::{catalog::Catalog, page::Page},
    parser::objects::PdfDocument,
    render::tile::TileCache,
    render::{
        render_block_tile, render_page, render_tile, render_tile_content, render_tile_with_cache,
        render_tile_with_render_cache, GlyphCache, RenderCache, TileRect,
    },
};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn load_doc(name: &str) -> PdfDocument {
    let bytes = std::fs::read(fixture_path(name)).expect("fixture not found");
    PdfDocument::parse(bytes).expect("parse failed")
}

fn first_page(doc: &PdfDocument) -> Page {
    let catalog = Catalog::from_document(doc).expect("catalog");
    let page_dict = catalog.get_page_dict(doc, 0).expect("page dict");
    Page::from_dict(doc, &page_dict).expect("page")
}

// ---------------------------------------------------------------------------
// Benchmark 1: full-page render, cold (new doc each iteration to bypass stream cache)
// ---------------------------------------------------------------------------

fn bench_render_page_cold(c: &mut Criterion) {
    c.bench_function("render_page_cold", |b| {
        b.iter(|| {
            let doc = load_doc("minimal.pdf");
            let page = first_page(&doc);
            render_page(&doc, &page, 1.0).expect("render failed")
        })
    });
}

// ---------------------------------------------------------------------------
// Benchmark 2: full-page render, warm content-stream cache (same doc, repeated)
// ---------------------------------------------------------------------------

fn bench_render_page_warm_stream(c: &mut Criterion) {
    let doc = load_doc("minimal.pdf");
    let page = first_page(&doc);
    // Prime the decoded_stream_cache.
    let _ = render_page(&doc, &page, 1.0).expect("prime");

    c.bench_function("render_page_warm_stream_cache", |b| {
        b.iter(|| render_page(&doc, &page, 1.0).expect("render failed"))
    });
}

// ---------------------------------------------------------------------------
// Benchmark 3: tiled render — N tiles, three cache strategies compared
// ---------------------------------------------------------------------------

fn bench_render_tiled(c: &mut Criterion) {
    let doc = load_doc("minimal.pdf");
    let page = first_page(&doc);
    let mb = page.media_box;
    let tiles = TileRect::tile_grid(&mb, 150.0);

    let mut group = c.benchmark_group("render_tiled");
    group.sample_size(10);

    group.bench_function("no_cache", |b| {
        b.iter(|| {
            for &tile in &tiles {
                render_tile(&doc, &page, 1.0, tile).expect("render failed");
            }
        })
    });

    group.bench_function("glyph_cache_only", |b| {
        b.iter(|| {
            let mut cache = GlyphCache::new();
            for &tile in &tiles {
                let (_, c) =
                    render_tile_with_cache(&doc, &page, 1.0, tile, cache).expect("render failed");
                cache = c;
            }
        })
    });

    group.bench_function("render_cache_full", |b| {
        b.iter(|| {
            let mut cache = RenderCache::new();
            for &tile in &tiles {
                let (_, c) = render_tile_with_render_cache(&doc, &page, 1.0, tile, cache)
                    .expect("render failed");
                cache = c;
            }
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark 4: content stream decode — fresh doc vs. warm stream cache
// ---------------------------------------------------------------------------

fn bench_decode_contents(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode_contents");

    group.bench_function("cold_doc", |b| {
        b.iter(|| {
            // Reload doc to ensure the stream cache is empty.
            let doc = load_doc("minimal.pdf");
            let page = first_page(&doc);
            page.decode_contents(&doc).expect("decode failed")
        })
    });

    // Warm: doc-level decoded_stream_cache already populated.
    let doc = load_doc("minimal.pdf");
    let page = first_page(&doc);
    let _ = page.decode_contents(&doc).expect("prime");

    group.bench_function("warm_stream_cache", |b| {
        b.iter(|| page.decode_contents(&doc).expect("decode failed"))
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark 5: TileCache LRU throughput (insert + get mix)
// ---------------------------------------------------------------------------

fn bench_tile_cache_lru(c: &mut Criterion) {
    use pdf_core::render::{tile::TileKey, PixmapBuffer};

    fn make_key(i: u32) -> TileKey {
        TileKey::new(
            0,
            1.0,
            &TileRect {
                x: i as f64 * 10.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
        )
    }

    c.bench_function("tile_cache_1000_insert_get", |b| {
        b.iter(|| {
            let mut cache = TileCache::new(500 * 10 * 10 * 4); // fits ~500 10×10 tiles
            for i in 0..1000u32 {
                let buf = PixmapBuffer::new(10, 10).expect("alloc failed");
                cache.insert(make_key(i), buf);
                cache.get(&make_key(i.saturating_sub(1)));
            }
        })
    });
}

// ---------------------------------------------------------------------------
// Benchmark 6: page lookup — Fix A (O(1) page table vs per-call tree walk)
//
// Uses a 23-page fixture and looks up the LAST page (worst case for the old
// O(N) walk). `warm` = table already built (the per-keystroke case → O(1)).
// `cold_rebuild` clears the table each call so it rebuilds (≈ the old per-call
// O(N) tree walk). `warm_first` vs `warm_last` shows page number no longer
// affects lookup cost.
// ---------------------------------------------------------------------------

fn bench_page_lookup(c: &mut Criterion) {
    let doc = load_doc("Unit_1.pdf");
    let catalog = Catalog::from_document(&doc).expect("catalog");
    let last = catalog.page_count - 1;
    // Prime the table once.
    let _ = catalog.get_page_dict(&doc, last).expect("prime");

    let mut group = c.benchmark_group("page_lookup");

    group.bench_function("warm_first", |b| {
        b.iter(|| catalog.get_page_dict(&doc, 0).expect("lookup"))
    });
    group.bench_function("warm_last", |b| {
        b.iter(|| catalog.get_page_dict(&doc, last).expect("lookup"))
    });
    group.bench_function("cold_rebuild_last", |b| {
        b.iter(|| {
            doc.clear_page_table();
            catalog.get_page_dict(&doc, last).expect("lookup")
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark 7: edit preview render — Fix B (block tile vs full-page-then-crop)
//
// Same page content both ways: the old preview rendered the whole page and
// cropped; the new path renders only the block's tile. Shows the per-keystroke
// render cost drop (and that it scales with block size, not page size).
// ---------------------------------------------------------------------------

fn bench_edit_preview(c: &mut Criterion) {
    let doc = load_doc("Group-3.pdf");
    let page = first_page(&doc);
    let mb = page.media_box;
    let scale = 2.0f32; // typical device scale (CSS scale × dpr)

    // A line-height-ish block near the top of the page.
    let block_tile = TileRect {
        x: mb.x1 + 50.0,
        y: mb.y1 + mb.height() - 120.0,
        width: 240.0,
        height: 24.0,
    };
    let full_tile = TileRect {
        x: mb.x1,
        y: mb.y1,
        width: mb.width(),
        height: mb.height(),
    };

    // The real preview feeds *block-only* content (all other text + images are
    // dropped by `edit_render_content_ops`), so the only thing Fix B changes is
    // the buffer size: a full-page raster vs a block raster. Model that with a
    // tiny content stream drawing in the block's region, so neither path is
    // dominated by interpreting unrelated page content.
    let content = format!(
        "0 0 1 rg {} {} {} {} re f",
        block_tile.x, block_tile.y, block_tile.width, block_tile.height
    )
    .into_bytes();

    let mut group = c.benchmark_group("edit_preview");
    group.sample_size(30);

    // Old path: render the whole page buffer, then crop the block out of it.
    group.bench_function("full_page_then_crop", |b| {
        b.iter(|| render_tile_content(&doc, &page, scale, full_tile, &content).expect("render"))
    });
    // New path: render only the block-sized buffer.
    group.bench_function("block_tile", |b| {
        b.iter(|| render_block_tile(&doc, &page, scale, block_tile, &content).expect("render"))
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_render_page_cold,
    bench_render_page_warm_stream,
    bench_render_tiled,
    bench_decode_contents,
    bench_tile_cache_lru,
    bench_page_lookup,
    bench_edit_preview,
);
criterion_main!(benches);
