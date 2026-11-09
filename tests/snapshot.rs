//! Render-snapshot harness: compare our full-page renders against MuPDF
//! reference images (Option A, user decision 2026-11-09).
//!
//! Reference images live in `tests/fixtures/ref/<stem>-<N>.ppm`, one per page
//! (1-based, MuPDF `mutool convert -r 72` output). Generate them with
//! `tests/gen_snapshot_refs.sh` (requires `mutool`; see
//! `.doc/render-snapshot-harness-2026-11-09.md` for the AGPL decision).
//!
//! Comparison is tolerant: renderers legitimately differ in anti-aliasing and
//! gamma handling. We assert on aggregate metrics (mean channel delta +
//! worst-pixel budget) rather than exact pixel equality, and print per-fixture
//! stats so regressions are visible in the log.
//!
//! PPM (P6) is chosen over PNG because the crate has no image-decode dep and
//! the harness must stay dependency-free and WASM-safe (rules R2/R3).
#![cfg(feature = "render")]

use pdf_core::{render::render_page_rgba, PdfDocument};
use std::path::{Path, PathBuf};

/// Per-pixel channel delta budget: pixels with any channel differing more
/// than this many levels count as "divergent".
///
/// Baseline calibration (2026-11-09, first MuPDF-parity run): 60 reference
/// pages over 9 documents showed mean Δ ≤ 10.6 with divergent ≤ 12.9% —
/// almost entirely anti-aliased glyph / vector edges, where two independent
/// rasterizers legitimately differ. Bars sit above that with headroom.
const PIXEL_TOLERANCE: f64 = 16.0;
/// Maximum fraction of divergent pixels before failure.
const MAX_DIVERGENT_FRACTION: f64 = 0.25;
/// Maximum mean absolute channel difference (0–255 scale) before failure.
const MAX_MEAN_DELTA: f64 = 16.0;

#[derive(Debug, Clone)]
struct Ppm {
    width: u32,
    height: u32,
    /// RGB triplets (maxval 255).
    rgb: Vec<u8>,
}

/// Parse a binary PPM (P6) with maxval 255. Header may contain `#` comments.
fn parse_ppm(data: &[u8]) -> Result<Ppm, String> {
    // --- header: magic "P6", then 3 whitespace-separated integers.
    let mut fields: Vec<u32> = Vec::new();
    let mut i = 2usize; // skip 'P6'
    while fields.len() < 3 {
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < data.len() && data[i] == b'#' {
            while i < data.len() && data[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < data.len() && data[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            return Err(format!("PPM header ends before field {}", fields.len()));
        }
        let s = std::str::from_utf8(&data[start..i]).map_err(|e| e.to_string())?;
        fields.push(
            s.parse::<u32>()
                .map_err(|_| format!("bad PPM field `{}`", s))?,
        );
    }
    if data.get(i).copied().map(|b| b.is_ascii_whitespace()) != Some(true) {
        return Err("PPM header not whitespace-terminated".to_string());
    }
    i += 1; // single whitespace byte after maxval

    let (width, height, maxval) = (fields[0], fields[1], fields[2]);
    if maxval != 255 {
        return Err(format!("unsupported PPM maxval {}", maxval));
    }
    let need = (width as usize) * (height as usize) * 3;
    if data.len() < i + need {
        return Err(format!(
            "PPM pixel data truncated: need {} bytes at offset {}, have {}",
            need,
            i,
            data.len() - i
        ));
    }
    Ok(Ppm {
        width,
        height,
        rgb: data[i..i + need].to_vec(),
    })
}

/// Per-page comparison. Returns `(mean_delta, divergent_fraction)`.
fn compare(ours_rgba: &[u8], w: u32, h: u32, ref_: &Ppm) -> (f64, f64) {
    assert_eq!(
        (w, h),
        (ref_.width, ref_.height),
        "page size mismatch: ours {}x{} vs ref {}x{}",
        w,
        h,
        ref_.width,
        ref_.height
    );
    let px = (w * h) as f64;
    let mut total = 0.0f64;
    let mut divergent = 0.0f64;
    for p in 0..(w * h) as usize {
        let o = p * 4;
        let r = p * 3;
        // Alpha-98 pixels: MuPDF paints the page background; compare RGB only.
        let (d0, d1, d2) = (
            (ours_rgba[o] as i32 - ref_.rgb[r] as i32).abs(),
            (ours_rgba[o + 1] as i32 - ref_.rgb[r + 1] as i32).abs(),
            (ours_rgba[o + 2] as i32 - ref_.rgb[r + 2] as i32).abs(),
        );
        total += (d0 + d1 + d2) as f64 / 3.0;
        if d0 as f64 > PIXEL_TOLERANCE || d1 as f64 > PIXEL_TOLERANCE || d2 as f64 > PIXEL_TOLERANCE
        {
            divergent += 1.0;
        }
    }
    (total / px, divergent / px)
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn ref_path(stem: &Path, page_1based: usize) -> PathBuf {
    stem.parent().unwrap().join("ref").join(format!(
        "{}-{}.ppm",
        stem.file_stem().unwrap().to_string_lossy(),
        page_1based
    ))
}

/// Every fixture PDF with a MuPDF reference must stay within tolerance.
#[test]
fn snapshots_match_mupdf_reference() {
    let dir = fixtures_dir();
    let mut compared = 0usize;
    let mut skipped = Vec::new();
    let mut failures = String::new();

    let mut pdfs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixtures dir readable")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("pdf"))
        .collect();
    pdfs.sort();

    for pdf in pdfs {
        let data = std::fs::read(&pdf).expect("fixture readable");
        let name = pdf.file_name().unwrap().to_string_lossy().to_string();
        let Ok(doc) = PdfDocument::parse(data) else {
            skipped.push(format!("{} (unparseable)", name));
            continue;
        };
        let Ok(pages) = doc.page_count() else {
            skipped.push(format!("{} (no page count)", name));
            continue;
        };
        for page in 0..pages {
            let refp = ref_path(&pdf, page + 1);
            if !refp.exists() {
                skipped.push(format!("{} p{} (no reference)", name, page + 1));
                continue;
            }
            let ref_bytes = std::fs::read(&refp).expect("reference readable");
            let ref_ppm = match parse_ppm(&ref_bytes) {
                Ok(p) => p,
                Err(e) => {
                    failures.push_str(&format!(
                        "FAIL {}: bad reference {}: {}\n",
                        name,
                        refp.display(),
                        e
                    ));
                    continue;
                }
            };
            let (w, h, rgba) = match render_page_rgba(&doc, page, 1.0) {
                Ok((w, h, v)) => (w, h, v),
                Err(e) => {
                    failures.push_str(&format!(
                        "FAIL {}: render error p{}: {}\n",
                        name,
                        page + 1,
                        e
                    ));
                    continue;
                }
            };
            if w != ref_ppm.width || h != ref_ppm.height {
                failures.push_str(&format!(
                    "FAIL {} p{}: size ours {}x{} vs ref {}x{}\n",
                    name,
                    page + 1,
                    w,
                    h,
                    ref_ppm.width,
                    ref_ppm.height
                ));
                continue;
            }
            let (mean, div) = compare(&rgba, w, h, &ref_ppm);
            compared += 1;
            eprintln!(
                "  {:18} p{}  mean Δ={:6.2}  divergent={:5.2}%  -> {}",
                name,
                page + 1,
                mean,
                div * 100.0,
                if mean > MAX_MEAN_DELTA || div > MAX_DIVERGENT_FRACTION {
                    "OVER"
                } else {
                    "ok"
                },
            );
            if mean > MAX_MEAN_DELTA {
                failures.push_str(&format!(
                    "FAIL {} p{}: mean Δ {:.2} > {:.2}\n",
                    name,
                    page + 1,
                    mean,
                    MAX_MEAN_DELTA
                ));
            }
            if div > MAX_DIVERGENT_FRACTION {
                failures.push_str(&format!(
                    "FAIL {} p{}: divergent {:.2}% > {:.2}%\n",
                    name,
                    page + 1,
                    div * 100.0,
                    MAX_DIVERGENT_FRACTION * 100.0
                ));
            }
        }
    }

    eprintln!(
        "Snapshot harness: {} pages compared, {} skipped {}",
        compared,
        skipped.len(),
        if skipped.is_empty() {
            "(references complete)"
        } else {
            "(run tests/gen_snapshot_refs.sh to fill)"
        }
    );
    assert!(failures.is_empty(), "snapshot divergences:\n{}", failures);
    assert!(compared > 0, "no pages compared — generate references with tests/gen_snapshot_refs.sh or remove this fixture set");
}

// ---------------------------------------------------------------------------
// parse_ppm tests (crafted bytes; error paths)
// ---------------------------------------------------------------------------

#[test]
fn ppm_happy_path_parses() {
    let tiny = b"P6\n2 1\n255\n\xde\xad\xbe\xef\xff\x10";
    let p = parse_ppm(tiny).expect("parses");
    assert_eq!((p.width, p.height), (2, 1));
    assert_eq!(p.rgb, vec![0xde, 0xad, 0xbe, 0xef, 0xff, 0x10]);
}

#[test]
fn ppm_with_comment_lines() {
    let tiny = b"P6\n# a comment\n2 # inline\n1\n255\n\x00\x00\x00\x00\x00\x00";
    let p = parse_ppm(tiny).expect("parses");
    assert_eq!((p.width, p.height), (2, 1));
}

#[test]
fn ppm_truncated_is_error() {
    let tiny = b"P6\n3 3\n255\n\x00\x00\x00"; // need 27 bytes, have 3
    assert!(parse_ppm(tiny).is_err());
}

#[test]
fn ppm_bad_maxval_is_error() {
    let tiny = b"P6\n2 1\n65535\n\x00\x00\x00\x00\x00\x00";
    assert!(parse_ppm(tiny).is_err());
}
