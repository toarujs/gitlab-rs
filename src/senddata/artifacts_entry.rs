use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ArtifactsEntryParams {
    archive: String,
    entry: String,
}

pub async fn artifacts_entry_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: ArtifactsEntryParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse artifacts-entry params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

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

    let mut zip_file = archive.by_name(&params.entry).map_err(|e| {
        tracing::error!("Entry '{}' not found in archive: {}", params.entry, e);
        StatusCode::NOT_FOUND
    })?;

    let mut data = Vec::new();
    zip_file.read_to_end(&mut data).map_err(|e| {
        tracing::error!("Failed to read entry data: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let content_type = mime_guess::from_path(&params.entry)
        .first_or_octet_stream()
        .to_string();

    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", content_type.parse().unwrap());
    response_headers.insert("content-length", data.len().to_string().parse().unwrap());

    Ok((StatusCode::OK, response_headers, data).into_response())
}
