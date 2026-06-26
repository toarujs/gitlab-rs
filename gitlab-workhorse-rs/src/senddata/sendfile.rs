use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;

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

    if !file_path.exists() || !file_path.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }

    let metadata = tokio::fs::metadata(&file_path).await.map_err(|e| {
        tracing::error!("Failed to get file metadata: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let file_size = metadata.len();

    let mut file = tokio::fs::File::open(&file_path).await.map_err(|e| {
        tracing::error!("Failed to open send-file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut buf = Vec::with_capacity(file_size as usize);
    file.read_to_end(&mut buf).await.map_err(|e| {
        tracing::error!("Failed to read send-file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut response_headers = HeaderMap::new();

    let content_type = params.content_type.as_deref().unwrap_or("application/octet-stream");
    response_headers.insert("content-type", content_type.parse().unwrap());
    response_headers.insert("content-length", file_size.to_string().parse().unwrap());

    if let Some(disposition) = &params.content_disposition {
        response_headers.insert("content-disposition", disposition.parse().unwrap());
    } else {
        let filename = file_path.file_name().unwrap_or_default().to_string_lossy();
        response_headers.insert(
            "content-disposition",
            format!("attachment; filename=\"{}\"", filename)
                .parse()
                .unwrap(),
        );
    }

    response_headers.insert("cache-control", "private, max-age=0, must-revalidate".parse().unwrap());

    Ok((StatusCode::OK, response_headers, buf).into_response())
}
