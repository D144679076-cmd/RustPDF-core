//! Multipart form helpers for REST API handlers.

use std::collections::HashMap;

use axum::extract::Multipart;

/// Collect all file parts from a multipart body into a `Vec<Vec<u8>>`.
///
/// Part order matches the order they appear in the request. Returns an error
/// string on I/O failure.
pub async fn collect_multipart_files(mut multipart: Multipart) -> Result<Vec<Vec<u8>>, String> {
    let mut files = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        let data = field
            .bytes()
            .await
            .map_err(|e| format!("multipart read error: {e}"))?;
        files.push(data.to_vec());
    }
    Ok(files)
}

/// Collect all named parts from a multipart body into a `HashMap<name, bytes>`.
///
/// Parts without a name are ignored. Returns an error string on I/O failure.
pub async fn collect_named_parts(
    mut multipart: Multipart,
) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut parts = HashMap::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_owned();
        let data = field
            .bytes()
            .await
            .map_err(|e| format!("multipart read error: {e}"))?;
        if !name.is_empty() {
            parts.insert(name, data.to_vec());
        }
    }
    Ok(parts)
}
