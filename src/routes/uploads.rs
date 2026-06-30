#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

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
    match proxy::proxy_handler(State(state), req).await {
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
