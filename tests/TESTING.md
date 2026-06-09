# Testing with Real PDF Files

## Quick Start

Run only the real-PDF integration tests:

```bash
cargo test --test real_pdf
```

Run with output to see which fixtures passed:

```bash
cargo test --test real_pdf -- --nocapture
```

## How It Works

```
tests/
├── fixtures/          ← Drop .pdf files here
│   ├── minimal.pdf        (1 page, no content)
│   ├── multipage.pdf      (3 pages, text streams)
│   └── with_stream.pdf   (1 page, FlateDecode)
└── real_pdf.rs        ← Integration test file
```

### Auto-Discovery Test

The `all_fixtures_parse_successfully` test automatically scans `tests/fixtures/` and
attempts to parse every `.pdf` file it finds. This means:

1. Drop any real PDF into `tests/fixtures/`
2. Run `cargo test --test real_pdf`
3. If parsing fails, you'll get the exact error and byte offset

No code changes needed to test new files.

## Adding Your Own PDFs

Place any PDF file in `tests/fixtures/`. The auto-discovery test will pick it up
immediately. For targeted assertions (page count, specific objects, etc.), add a
dedicated test function in `tests/real_pdf.rs`:

```rust
#[test]
fn my_custom_pdf_has_expected_pages() {
    let doc = load_fixture("my_custom.pdf");
    assert_eq!(doc.page_count().unwrap(), 5);
}
```

## Filtering Tests

Run a single test by name:

```bash
cargo test --test real_pdf minimal_pdf_has_one_page
```

Run all tests matching a pattern:

```bash
cargo test --test real_pdf multipage
```

## What Gets Tested

| Test | Validates |
|------|-----------|
| `all_fixtures_parse_successfully` | Every .pdf in fixtures/ parses without error |
| `*_parses_successfully` | XRef + trailer parsing |
| `*_has_*_page(s)` | Page tree traversal, /Count resolution |
| `*_has_catalog` | /Root → Catalog dict |
| `*_page_has_mediabox` | Page object field access |
| `*_content_streams_decode` | Raw stream extraction |
| `*_decodes_flate_content` | FlateDecode filter pipeline |
| `*_has_font_resource` | Object retrieval by ID |
| `garbage_bytes_fail_to_parse` | Graceful error on invalid input |
| `truncated_pdf_fails` | Graceful error on incomplete file |

## Troubleshooting

**"failed to parse fixture X: Invalid token at byte offset N"**

The parser hit something it doesn't support yet. Common causes:
- Encrypted PDF (needs `crypto` feature)
- XRef streams with unusual /W widths
- Linearized PDF with unusual structure

**"no .pdf files found"**

The `tests/fixtures/` directory is empty or missing. Regenerate fixtures:

```bash
python3 tests/gen_fixtures.py
```

## Generating Fixtures

The fixture PDFs were generated with a Python script. To regenerate or modify:

```bash
python3 tests/gen_fixtures.py
```

This produces valid PDFs with correct byte offsets in the XRef table.
