use pdf_core::streaming::{ByteCache, StreamingDocument};

// ---------------------------------------------------------------------------
// ByteCache tests
// ---------------------------------------------------------------------------

#[test]
fn byte_cache_empty_get_returns_none() {
    let c = ByteCache::new(100);
    assert!(c.get(0, 1).is_none());
}

#[test]
fn byte_cache_feed_and_get() {
    let mut c = ByteCache::new(100);
    c.feed(10, vec![1, 2, 3]);
    assert_eq!(c.get(10, 3), Some(&[1u8, 2, 3][..]));
    assert_eq!(c.get(11, 2), Some(&[2u8, 3][..]));
    assert!(c.get(9, 1).is_none());
    assert!(c.get(13, 1).is_none());
}

#[test]
fn byte_cache_overlapping_feeds_merge() {
    let mut c = ByteCache::new(100);
    c.feed(0, vec![0, 1, 2, 3]);
    c.feed(2, vec![2, 3, 4, 5]);
    assert_eq!(c.chunks().count(), 1);
    assert_eq!(c.get(0, 6), Some(&[0u8, 1, 2, 3, 4, 5][..]));
}

#[test]
fn byte_cache_non_overlapping_feeds_stay_separate() {
    let mut c = ByteCache::new(100);
    c.feed(0, vec![0, 1]);
    c.feed(10, vec![10, 11]); // gap at 2..10
    assert_eq!(c.chunks().count(), 2);
    assert!(c.get(0, 12).is_none()); // gap means 12-byte read unavailable
    assert_eq!(c.get(0, 2), Some(&[0u8, 1][..]));
    assert_eq!(c.get(10, 2), Some(&[10u8, 11][..]));
}

#[test]
fn byte_cache_total_len() {
    let c = ByteCache::new(50_000_000);
    assert_eq!(c.total_len(), 50_000_000);
}

// ---------------------------------------------------------------------------
// StreamingDocument tests
// ---------------------------------------------------------------------------

#[test]
fn streaming_document_from_minimal_pdf() {
    let bytes = include_bytes!("fixtures/minimal.pdf");
    let total = bytes.len() as u64;
    let tail_start = total.saturating_sub(4096) as usize;
    let tail = &bytes[tail_start..];

    let mut streaming = StreamingDocument::from_tail(tail, total).expect("from_tail failed");

    // Feed ranges until page 0 is ready (max 10 iterations to prevent infinite loop).
    for _ in 0..10 {
        if streaming.page_ready(0) {
            break;
        }
        let ranges = streaming.needed_ranges_for_page(0);
        assert!(
            !ranges.is_empty(),
            "page not ready but needed_ranges returned empty"
        );
        for (offset, length) in ranges {
            let start = offset as usize;
            let end = (offset + length).min(total) as usize;
            streaming.feed(offset, bytes[start..end].to_vec());
        }
    }

    assert!(
        streaming.page_ready(0),
        "page 0 should be ready after feeding all needed ranges"
    );

    let doc = streaming
        .build_page_document(0)
        .expect("build_page_document failed");
    assert!(doc.page_count().unwrap() >= 1);
}

#[test]
fn streaming_document_from_multipage_pdf() {
    let bytes = include_bytes!("fixtures/multipage.pdf");
    let total = bytes.len() as u64;
    let tail_start = total.saturating_sub(4096) as usize;
    let tail = &bytes[tail_start..];

    let mut streaming = StreamingDocument::from_tail(tail, total).expect("from_tail failed");

    for _ in 0..16 {
        if streaming.page_ready(0) {
            break;
        }
        for (offset, length) in streaming.needed_ranges_for_page(0) {
            let start = offset as usize;
            let end = (offset + length).min(total) as usize;
            streaming.feed(offset, bytes[start..end].to_vec());
        }
    }

    assert!(streaming.page_ready(0));
    let doc = streaming.build_page_document(0).unwrap();
    assert!(
        doc.page_count().unwrap() > 1,
        "multipage.pdf should have more than 1 page"
    );
}

#[test]
fn streaming_document_invalid_tail_returns_error() {
    let tail = b"not a pdf tail at all -- no startxref here";
    let result = StreamingDocument::from_tail(tail, 1000);
    assert!(result.is_err(), "invalid tail should return Err");
}

#[test]
fn needed_ranges_empty_when_page_ready() {
    let bytes = include_bytes!("fixtures/minimal.pdf");
    let total = bytes.len() as u64;
    let tail_start = total.saturating_sub(4096) as usize;
    let tail = &bytes[tail_start..];
    let mut streaming = StreamingDocument::from_tail(tail, total).unwrap();

    // Feed the entire file.
    streaming.feed(0, bytes.to_vec());

    // After feeding everything, needed_ranges should be empty.
    let ranges = streaming.needed_ranges_for_page(0);
    assert!(
        ranges.is_empty(),
        "needed_ranges should be empty when all bytes are cached, got: {:?}",
        ranges
    );
    assert!(streaming.page_ready(0));
}
