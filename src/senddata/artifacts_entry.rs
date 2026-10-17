use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use serde::Deserialize;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ArtifactsEntryParams {
    archive: String,
    entry: String,
}

/// Rails sends `Entry` through `Base64.encode64`, which wraps lines at 60
/// characters and always appends a newline. Go's base64 decoder ignores those
/// line breaks; Rust's does not, so strip them before decoding.
fn decode_entry(encoded: &str) -> Result<String, StatusCode> {
    let compact: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = BASE64_STANDARD
        .decode(compact)
        .map_err(|e| {
            tracing::error!("Failed to decode artifacts-entry path: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    String::from_utf8(bytes).map_err(|e| {
        tracing::error!("Artifacts entry path is not UTF-8: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

pub async fn artifacts_entry_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: ArtifactsEntryParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse artifacts-entry params: {}", e);
        StatusCode::BAD_REQUEST
    })?;
    let entry = decode_entry(&params.entry)?;

    let archive_path = Path::new(&params.archive);
    if !archive_path.exists() {
        tracing::error!("Archive not found: {}", params.archive);
        return Err(StatusCode::NOT_FOUND);
    }

    let file = std::fs::File::open(archive_path).map_err(|e| {
        tracing::error!("Failed to open archive: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        tracing::error!("Failed to read ZIP archive: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut zip_file = archive.by_name(&entry).map_err(|e| {
        tracing::error!("Entry '{}' not found in archive: {}", entry, e);
        StatusCode::NOT_FOUND
    })?;

    let mut data = Vec::new();
    zip_file.read_to_end(&mut data).map_err(|e| {
        tracing::error!("Failed to read entry data: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let content_type = mime_guess::from_path(&entry)
        .first_or_octet_stream()
        .to_string();

    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", content_type.parse().unwrap());
    response_headers.insert("content-length", data.len().to_string().parse().unwrap());

    Ok((StatusCode::OK, response_headers, data).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_with_trailing_newline() {
        // `Base64.encode64("out.txt")` == "b3V0LnR4dA==\n"
        assert_eq!(decode_entry("b3V0LnR4dA==\n").unwrap(), "out.txt");
    }

    #[test]
    fn decodes_wrapped_base64_path() {
        let path = "some/very/long/directory/name/that/exceeds/sixty/characters/file.txt";
        let encoded = base64_wrapped(path);
        assert!(encoded.contains('\n'));
        assert_eq!(decode_entry(&encoded).unwrap(), path);
    }

    fn base64_wrapped(value: &str) -> String {
        let encoded = BASE64_STANDARD.encode(value);
        let mut out = String::new();
        for chunk in encoded.as_bytes().chunks(60) {
            out.push_str(std::str::from_utf8(chunk).unwrap());
            out.push('\n');
        }
        out
    }
}
