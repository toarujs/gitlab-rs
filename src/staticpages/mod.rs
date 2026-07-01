#![allow(dead_code)]
use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use std::path::PathBuf;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

use super::compression;
use super::imageresizer;
use super::state::AppState;

#[derive(Debug, Clone)]
pub struct StaticPagesState {
    pub document_root: PathBuf,
    pub exclude_paths: Vec<String>,
    pub enable_gzip: bool,
    pub enable_error_pages: bool,
}

impl StaticPagesState {
    pub fn new(document_root: PathBuf) -> Self {
        Self {
            document_root,
            exclude_paths: Vec::new(),
            enable_gzip: true,
            enable_error_pages: true,
        }
    }
}

pub async fn serve_static_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Result<Response, StatusCode> {
    let doc_root = &state.download.document_root;
    let client_encoding = compression::negotiate_encoding(&headers);

    let resources = compression::find_compression_resources(doc_root, &path, client_encoding).await;

    if let Some(error_response) = resources.error {
        let status = error_response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(StatusCode::NOT_FOUND);
        }
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let content_type = resources.content_type.as_deref().unwrap_or("application/octet-stream");
    let cache_control = resources.cache_control.as_deref().unwrap_or("public, max-age=3600");

    let is_convertible_image = content_type.contains("image/png") || content_type.contains("image/jpeg");
    let accepts_webp = imageresizer::WebPConverter::supports_webp(&headers);
    let best_format = imageresizer::WebPConverter::best_supported_format(&headers);

    if is_convertible_image && (accepts_webp || best_format == imageresizer::ImageTargetFormat::Avif) {
        if let Some(orig_path) = &resources.original_path {
            if let Ok(file_bytes) = compression::read_file_to_bytes(orig_path).await {
                if file_bytes.len() >= 1024 {
                    let result = match best_format {
                        imageresizer::ImageTargetFormat::Avif => {
                            state.webp_converter.convert_to_avif(&file_bytes).await
                                .map(|b| (b, "image/avif"))
                        }
                        _ => {
                            state.webp_converter.convert_to_webp(&file_bytes).await
                                .map(|b| (b, "image/webp"))
                        }
                    };

                    match result {
                        Ok((converted, mime)) if converted.len() < file_bytes.len() => {
                            let mut response_headers = HeaderMap::new();
                            response_headers.insert("content-type", mime.parse().unwrap());
                            response_headers.insert("content-length", converted.len().to_string().parse().unwrap());
                            response_headers.insert("cache-control", "public, max-age=86400".parse().unwrap());
                            response_headers.insert("vary", "Accept".parse().unwrap());
                            return Ok((StatusCode::OK, response_headers, Body::from(converted)).into_response());
                        }
                        Ok(_) => {
                            tracing::debug!("Image conversion skipped: result not smaller than original");
                        }
                        Err(e) => {
                            tracing::debug!("Image conversion skipped for static file: {}", e);
                        }
                    }
                }
            }
        }
    }

    if let Some(comp_ref) = resources.compressions.first() {
        let content = compression::read_file_to_bytes(&comp_ref.path).await.map_err(|e| {
            tracing::error!("Failed to read compressed file: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let mut response_headers = HeaderMap::new();
        response_headers.insert("content-type", content_type.parse().unwrap());
        response_headers.insert("content-length", comp_ref.size.to_string().parse().unwrap());
        response_headers.insert("content-encoding", comp_ref.encoding.content_encoding().parse().unwrap());
        response_headers.insert("cache-control", cache_control.parse().unwrap());
        response_headers.insert("vary", "Accept-Encoding".parse().unwrap());

        return Ok((StatusCode::OK, response_headers, Body::from(content)).into_response());
    }

    let file = tokio::fs::File::open(resources.original_path.as_ref().unwrap()).await.map_err(|e| {
        tracing::error!("Failed to open static file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", content_type.parse().unwrap());
    response_headers.insert("content-length", resources.original_size.to_string().parse().unwrap());
    response_headers.insert("cache-control", cache_control.parse().unwrap());
    response_headers.insert("vary", "Accept-Encoding".parse().unwrap());

    Ok((StatusCode::OK, response_headers, body).into_response())
}

/// Serve static files from public/-/<path> (e.g., /-/emojis, /-/pwa-icons).
/// Falls through to 404 if file not found — caller should chain with proxy handler.
pub async fn serve_public_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(subpath): Path<String>,
) -> Result<Response, StatusCode> {
    let doc_root = &state.download.document_root;
    let path = format!("-/{}", subpath);
    let client_encoding = compression::negotiate_encoding(&headers);

    let resources = compression::find_compression_resources(doc_root, &path, client_encoding).await;

    if resources.error.is_some() {
        return Err(StatusCode::NOT_FOUND);
    }

    let content_type = resources.content_type.as_deref().unwrap_or("application/octet-stream");
    let cache_control = resources.cache_control.as_deref().unwrap_or("public, max-age=3600");

    if let Some(comp_ref) = resources.compressions.first() {
        let content = compression::read_file_to_bytes(&comp_ref.path).await.map_err(|e| {
            tracing::error!("Failed to read compressed file: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let mut response_headers = HeaderMap::new();
        response_headers.insert("content-type", content_type.parse().unwrap());
        response_headers.insert("content-length", comp_ref.size.to_string().parse().unwrap());
        response_headers.insert("content-encoding", comp_ref.encoding.content_encoding().parse().unwrap());
        response_headers.insert("cache-control", cache_control.parse().unwrap());
        response_headers.insert("vary", "Accept-Encoding".parse().unwrap());

        return Ok((StatusCode::OK, response_headers, Body::from(content)).into_response());
    }

    let file = tokio::fs::File::open(resources.original_path.as_ref().unwrap()).await.map_err(|e| {
        tracing::error!("Failed to open static file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", content_type.parse().unwrap());
    response_headers.insert("content-length", resources.original_size.to_string().parse().unwrap());
    response_headers.insert("cache-control", cache_control.parse().unwrap());
    response_headers.insert("vary", "Accept-Encoding".parse().unwrap());

    Ok((StatusCode::OK, response_headers, body).into_response())
}

pub struct ErrorPage;

impl ErrorPage {
    pub fn render(status: StatusCode) -> Response {
        let message = match status.as_u16() {
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            408 => "Request Timeout",
            413 => "Payload Too Large",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            504 => "Gateway Timeout",
            _ => "Error",
        };

        let html = format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{} - GitLab Workhorse RS</title>
    <style>
        body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #f5f5f5; }}
        .container {{ text-align: center; background: white; padding: 40px; border-radius: 8px; box-shadow: 0 2px 8px rgba(0,0,0,0.1); max-width: 500px; }}
        h1 {{ color: #e24329; font-size: 72px; margin: 0; }}
        p {{ color: #666; font-size: 18px; margin: 16px 0 0; }}
    </style>
</head>
<body>
    <div class="container">
        <h1>{}</h1>
        <p>{}</p>
    </div>
</body>
</html>"#,
            status.as_u16(),
            status.as_u16(),
            message
        );

        let mut response_headers = HeaderMap::new();
        response_headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());

        (status, response_headers, html).into_response()
    }

    pub fn render_json(status: StatusCode) -> Response {
        let message = match status.as_u16() {
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            413 => "Payload Too Large",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            _ => "Error",
        };

        let json = serde_json::json!({
            "error": status.as_u16(),
            "message": message
        });

        let mut response_headers = HeaderMap::new();
        response_headers.insert("content-type", "application/json".parse().unwrap());

        (status, response_headers, json.to_string()).into_response()
    }
}

pub async fn deploy_page(
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    let index_path = state.download.document_root.join("index.html");

    if index_path.exists() {
        let mut file = tokio::fs::File::open(&index_path).await.map_err(|e| {
            tracing::error!("Failed to open index.html: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let mut content = Vec::new();
        file.read_to_end(&mut content).await.map_err(|e| {
            tracing::error!("Failed to read index.html: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let mut response_headers = HeaderMap::new();
        response_headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
        response_headers.insert("cache-control", "no-cache, must-revalidate".parse().unwrap());

        Ok((StatusCode::OK, response_headers, content).into_response())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub async fn error_page_fallback() -> Response {
    ErrorPage::render(StatusCode::NOT_FOUND)
}
