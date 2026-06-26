#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

const WIKI_ATTACHMENT_MAX_SIZE: usize = 50 * 1024 * 1024; // 50MB

pub async fn handle_wiki_attachment(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let content_length = req
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > WIKI_ATTACHMENT_MAX_SIZE {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Attachment too large").into_response();
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
    fn test_wiki_attachment_max_size() {
        assert_eq!(WIKI_ATTACHMENT_MAX_SIZE, 50 * 1024 * 1024);
    }
}
