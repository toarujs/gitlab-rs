#![allow(dead_code)]

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

use super::state::AppState;

#[derive(Debug, Clone)]
pub struct DownloadState {
    pub document_root: PathBuf,
    pub max_file_size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DownloadQuery {
    pub inline: Option<bool>,
    pub download: Option<bool>,
}

pub async fn handle_download(
    State(state): State<AppState>,
    Path(path): Path<String>,
    Query(query): Query<DownloadQuery>,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let file_path = state.download.document_root.join(&path);

    if !file_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    let metadata = tokio::fs::metadata(&file_path).await.map_err(|e| {
        tracing::error!("Failed to get file metadata: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !metadata.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }

    if metadata.len() > state.download.max_file_size {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let file_size = metadata.len();

    let mut file = File::open(&file_path).await.map_err(|e| {
        tracing::error!("Failed to open file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut buf = Vec::with_capacity(file_size as usize);
    file.read_to_end(&mut buf).await.map_err(|e| {
        tracing::error!("Failed to read file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let content = buf;
    state.metrics.record_download(file_size);

    let content_type = mime_guess::from_path(&file_path)
        .first_or_octet_stream()
        .to_string();

    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", content_type.parse().unwrap());
    response_headers.insert(
        "content-length",
        file_size.to_string().parse().unwrap(),
    );

    let disposition = if query.inline.unwrap_or(false) {
        "inline"
    } else if query.download.unwrap_or(true) {
        "attachment"
    } else {
        "inline"
    };

    let filename = file_path.file_name().unwrap_or_default().to_string_lossy();
    response_headers.insert(
        "content-disposition",
        format!("{}; filename=\"{}\"", disposition, filename)
            .parse()
            .unwrap(),
    );

    response_headers.insert(
        "cache-control",
        "private, max-age=0, must-revalidate".parse().unwrap(),
    );
    response_headers.insert("etag", format!("\"{}\"", file_size).parse().unwrap());

    Ok((StatusCode::OK, response_headers, content).into_response())
}

pub async fn handle_download_stream(
    State(state): State<AppState>,
    Path(path): Path<String>,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let file_path = state.download.document_root.join(&path);

    if !file_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    let metadata = tokio::fs::metadata(&file_path).await.map_err(|e| {
        tracing::error!("Failed to get file metadata: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !metadata.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }

    if metadata.len() > state.download.max_file_size {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let file_size = metadata.len();

    let file = File::open(&file_path).await.map_err(|e| {
        tracing::error!("Failed to open file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let stream = tokio_util::io::ReaderStream::new(file);

    state.metrics.record_download(file_size);

    let mut response_headers = HeaderMap::new();
    let content_type = mime_guess::from_path(&file_path)
        .first_or_octet_stream()
        .to_string();
    response_headers.insert("content-type", content_type.parse().unwrap());
    response_headers.insert(
        "content-length",
        file_size.to_string().parse().unwrap(),
    );

    let filename = file_path.file_name().unwrap_or_default().to_string_lossy();
    response_headers.insert(
        "content-disposition",
        format!("attachment; filename=\"{}\"", filename)
            .parse()
            .unwrap(),
    );

    Ok((
        StatusCode::OK,
        response_headers,
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}
