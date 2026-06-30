#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

const ARTIFACTS_MAX_SIZE: usize = 500 * 1024 * 1024; // 500MB

pub async fn handle_artifacts_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !content_type.contains("multipart/form-data") {
        return (StatusCode::BAD_REQUEST, "Expected multipart/form-data").into_response();
    }

    let content_length = req
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > ARTIFACTS_MAX_SIZE {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Artifact too large").into_response();
    }

    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_artifacts_download(
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
    fn test_artifacts_max_size() {
        assert_eq!(ARTIFACTS_MAX_SIZE, 500 * 1024 * 1024);
    }
}
