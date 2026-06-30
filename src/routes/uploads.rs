#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};
use http_body_util::BodyExt;
use uuid::Uuid;

const UPLOAD_MAX_SIZE: usize = 100 * 1024 * 1024; // 100MB

pub async fn handle_project_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let content_length = req
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > UPLOAD_MAX_SIZE {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Upload too large").into_response();
    }

    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_avatar_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let (parts, body) = req.into_parts();

    let body_bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => return (StatusCode::BAD_REQUEST, "Failed to read body").into_response(),
    };

    if body_bytes.len() > UPLOAD_MAX_SIZE {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Upload too large").into_response();
    }

    let content_type = parts.headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let upload_id = Uuid::new_v4().to_string();
    let tmp_dir = std::path::PathBuf::from("/var/opt/gitlab/gitlab-rails/tmp/workhorse");
    if let Err(e) = tokio::fs::create_dir_all(&tmp_dir).await {
        tracing::error!("Failed to create workhorse temp dir: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "Upload setup failed").into_response();
    }

    let tmp_path = tmp_dir.join(&upload_id);
    if let Err(e) = tokio::fs::write(&tmp_path, &body_bytes).await {
        tracing::error!("Failed to write temp upload file: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "Upload failed").into_response();
    }

    let metadata = serde_json::json!({
        "file.path": tmp_path.to_string_lossy(),
        "file.size": body_bytes.len(),
        "file.content_type": content_type,
        "file.original_filename": "avatar",
    });
    let json_body = metadata.to_string();

    let mut new_parts = parts;
    new_parts.headers.insert(
        header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    new_parts.headers.insert(
        header::CONTENT_LENGTH,
        json_body.len().to_string().parse().unwrap(),
    );

    let new_req = Request::from_parts(new_parts, Body::from(json_body));

    match proxy::proxy_handler(State(state), new_req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_sbom_scan(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_repository_commits(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_repository_files(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upload_max_size() {
        assert_eq!(UPLOAD_MAX_SIZE, 100 * 1024 * 1024);
    }
}
