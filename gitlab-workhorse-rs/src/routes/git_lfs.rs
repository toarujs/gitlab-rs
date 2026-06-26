#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

const LFS_MAX_OBJECT_SIZE: usize = 5 * 1024 * 1024 * 1024; // 5GB

pub async fn handle_lfs_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if *req.method() != Method::PUT {
        return (StatusCode::METHOD_NOT_ALLOWED, "Method not allowed").into_response();
    }

    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if content_type != "application/octet-stream" {
        return (StatusCode::BAD_REQUEST, "Expected application/octet-stream").into_response();
    }

    let content_length = req
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > LFS_MAX_OBJECT_SIZE {
        return (StatusCode::PAYLOAD_TOO_LARGE, "LFS object too large").into_response();
    }

    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lfs_max_object_size() {
        assert_eq!(LFS_MAX_OBJECT_SIZE, 5 * 1024 * 1024 * 1024);
    }
}
