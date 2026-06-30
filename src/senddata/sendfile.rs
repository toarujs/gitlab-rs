use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::Deserialize;
use std::path::PathBuf;
use tokio_util::io::ReaderStream;

#[derive(Debug, Deserialize)]
pub struct SendFileParams {
    pub path: String,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub content_disposition: Option<String>,
}

pub async fn send_file_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: SendFileParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse send-file params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let file_path = PathBuf::from(&params.path);

    let canonical = tokio::fs::canonicalize(&file_path).await.map_err(|_| {
        tracing::warn!("Send-file: path not found or invalid: {}", params.path);
        StatusCode::NOT_FOUND
    })?;

    if !canonical.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }

    let metadata = tokio::fs::metadata(&canonical).await.map_err(|e| {
        tracing::error!("Failed to get file metadata: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let file_size = metadata.len();

    let file = tokio::fs::File::open(&canonical).await.map_err(|e| {
        tracing::error!("Failed to open send-file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let reader_stream = ReaderStream::new(file);
    let body_stream = reader_stream
        .map(|result| {
            let mapped: Result<Bytes, Box<dyn std::error::Error + Send + Sync>> = match result {
                Ok(bytes) => Ok(Bytes::from(bytes)),
                Err(e) => Err(Box::new(e)),
            };
            mapped
        });
    let body = Body::from_stream(body_stream);

    let mut response_headers = HeaderMap::new();

    let content_type = params.content_type.as_deref().unwrap_or("application/octet-stream");
    response_headers.insert("content-type", content_type.parse().unwrap());
    response_headers.insert("content-length", file_size.to_string().parse().unwrap());

    if let Some(disposition) = &params.content_disposition {
        response_headers.insert("content-disposition", disposition.parse().unwrap());
    } else {
        let filename = canonical.file_name().unwrap_or_default().to_string_lossy();
        let sanitized = sanitize_filename(&filename);
        response_headers.insert(
            "content-disposition",
            format!("attachment; filename=\"{}\"", sanitized)
                .parse()
                .unwrap(),
        );
    }

    response_headers.insert("cache-control", "private, max-age=0, must-revalidate".parse().unwrap());

    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    *response.headers_mut() = response_headers;
    Ok(response)
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c == '\r' || c == '\n' || c == '"' || c == '\\' { '_' } else { c })
        .collect()
}
