use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode},
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

const SENDFILE_ROOTS: &[&str] = &[
    "/var/opt/gitlab",
    "/opt/gitlab/embedded/service/gitlab-rails/public",
];

pub fn is_allowed_sendfile_path(canonical: &std::path::Path) -> bool {
    SENDFILE_ROOTS
        .iter()
        .any(|root| canonical.starts_with(std::path::Path::new(root)))
}

pub async fn send_file_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: SendFileParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse send-file params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let mut headers = HeaderMap::new();
    let content_type = params
        .content_type
        .as_deref()
        .unwrap_or("application/octet-stream");
    headers.insert(
        "content-type",
        content_type
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    if let Some(disposition) = &params.content_disposition {
        headers.insert(
            "content-disposition",
            disposition.parse().map_err(|_| StatusCode::BAD_REQUEST)?,
        );
    }
    headers.insert(
        "cache-control",
        HeaderValue::from_static("private, max-age=0, must-revalidate"),
    );
    stream_path(&params.path, headers).await
}

/// Stream a local file Workhorse is allowed to read. Caller supplies headers
/// already sanitized for the client (content-type / disposition).
pub async fn stream_path(
    path: &str,
    mut response_headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let file_path = PathBuf::from(path);
    let canonical = tokio::fs::canonicalize(&file_path).await.map_err(|_| {
        tracing::warn!("Send-file: path not found or invalid: {}", path);
        StatusCode::NOT_FOUND
    })?;

    if !is_allowed_sendfile_path(&canonical) {
        tracing::warn!("Send-file path refused: {}", canonical.display());
        return Err(StatusCode::NOT_FOUND);
    }

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
    let body_stream = reader_stream.map(|result| {
        result.map(Bytes::from).map_err(|e| {
            let boxed: Box<dyn std::error::Error + Send + Sync> = Box::new(e);
            boxed
        })
    });
    let body = Body::from_stream(body_stream);

    if !response_headers.contains_key("content-type") {
        response_headers.insert(
            "content-type",
            HeaderValue::from_static("application/octet-stream"),
        );
    }
    if let Ok(len) = HeaderValue::from_str(&file_size.to_string()) {
        response_headers.insert("content-length", len);
    }
    if !response_headers.contains_key("content-disposition") {
        let filename = canonical.file_name().unwrap_or_default().to_string_lossy();
        let sanitized = sanitize_filename(&filename);
        if let Ok(value) = format!("attachment; filename=\"{sanitized}\"").parse() {
            response_headers.insert("content-disposition", value);
        }
    }
    response_headers.remove("x-sendfile");
    response_headers.remove("x-sendfile-type");

    tracing::info!(
        path = %canonical.display(),
        size = file_size,
        "X-Sendfile streamed"
    );

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn allows_gitlab_data_paths() {
        assert!(is_allowed_sendfile_path(Path::new(
            "/var/opt/gitlab/gitlab-rails/shared/artifacts/ab.bin"
        )));
        assert!(is_allowed_sendfile_path(Path::new(
            "/opt/gitlab/embedded/service/gitlab-rails/public/favicon.ico"
        )));
    }

    #[test]
    fn refuses_paths_outside_gitlab() {
        assert!(!is_allowed_sendfile_path(Path::new("/etc/passwd")));
        assert!(!is_allowed_sendfile_path(Path::new("/tmp/secret")));
        assert!(!is_allowed_sendfile_path(Path::new("/var/opt/gitlab-evil/x")));
    }
}
