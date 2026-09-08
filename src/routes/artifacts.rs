#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Multipart, Request, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

const ARTIFACTS_MAX_SIZE: usize = 500 * 1024 * 1024; // 500MB

pub async fn handle_artifacts_upload(
    State(state): State<crate::state::AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    crate::upload::accelerate::accelerate_multipart_request(
        state,
        method,
        uri,
        headers,
        multipart,
        ARTIFACTS_MAX_SIZE as u64,
    )
    .await
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
