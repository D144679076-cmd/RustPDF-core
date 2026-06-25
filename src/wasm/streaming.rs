//! WASM bindings for incremental / streaming PDF loading.
//!
//! Exposes [`WasmStreamingDocument`] so JavaScript can open large remote PDFs
//! via HTTP Range requests without waiting for the full file to download.

use wasm_bindgen::prelude::*;

use crate::streaming::StreamingDocument;
use crate::wasm::document::WasmDocument;

/// Incremental PDF document loaded via HTTP Range requests.
///
/// ## Usage (JavaScript)
/// ```js
/// const tail = await fetch(url, { headers: { Range: 'bytes=-4096' } })
///   .then(r => r.arrayBuffer()).then(b => new Uint8Array(b));
/// const streaming = new WasmStreamingDocument(tail, BigInt(totalLen));
///
/// while (!streaming.page_ready(0)) {
///   const ranges = JSON.parse(streaming.needed_ranges(0));
///   await Promise.all(ranges.map(({ offset, length }) =>
///     fetch(url, { headers: { Range: `bytes=${offset}-${offset+length-1}` } })
///       .then(r => r.arrayBuffer())
///       .then(b => streaming.feed(BigInt(offset), new Uint8Array(b)))
///   ));
/// }
/// const doc = streaming.build_page_document(0);
/// ```
#[wasm_bindgen]
pub struct WasmStreamingDocument {
    inner: StreamingDocument,
}

#[wasm_bindgen]
impl WasmStreamingDocument {
    /// Initialise from the tail bytes of the remote file.
    ///
    /// `tail` should be the last 4096 bytes of the file.
    /// `total_len` is the full file size in bytes (from `Content-Length`).
    #[wasm_bindgen(constructor)]
    pub fn new(tail: &[u8], total_len: u64) -> Result<WasmStreamingDocument, JsError> {
        let inner = StreamingDocument::from_tail(tail, total_len)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(Self { inner })
    }

    /// Returns a JSON array of `{"offset": number, "length": number}` objects
    /// describing the byte ranges that must be fetched before `page_index` can
    /// be rendered.  An empty array means the page is ready.
    pub fn needed_ranges(&self, page_index: usize) -> String {
        let ranges = self.inner.needed_ranges_for_page(page_index);
        let items: Vec<String> = ranges
            .iter()
            .map(|(o, l)| format!(r#"{{"offset":{},"length":{}}}"#, o, l))
            .collect();
        format!("[{}]", items.join(","))
    }

    /// Feed a fetched byte range into the cache.
    ///
    /// `offset` is the start byte position within the remote file.
    /// `data` contains the fetched bytes for that range.
    pub fn feed(&mut self, offset: u64, data: &[u8]) {
        self.inner.feed(offset, data.to_vec());
    }

    /// Returns `true` when page `page_index` can be rendered.
    pub fn page_ready(&self, page_index: usize) -> bool {
        self.inner.page_ready(page_index)
    }

    /// Build a [`WasmDocument`] for the given page.
    ///
    /// Returns a `JsError` if the page is not yet ready — check [`page_ready`]
    /// before calling this method.
    pub fn build_page_document(&self, page_index: usize) -> Result<WasmDocument, JsError> {
        let doc = self
            .inner
            .build_page_document(page_index)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(WasmDocument { doc })
    }
}
