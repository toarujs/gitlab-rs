#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

const TERRAFORM_BODY_LIMIT: usize = 4 * 1024; // 4KB for lock/unlock

pub async fn handle_terraform_state_lock(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if *req.method() != Method::POST {
        return (StatusCode::METHOD_NOT_ALLOWED, "Method not allowed").into_response();
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_terraform_state_unlock(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if *req.method() != Method::DELETE {
        return (StatusCode::METHOD_NOT_ALLOWED, "Method not allowed").into_response();
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_terraform_state(
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
    fn test_terraform_body_limit() {
        assert_eq!(TERRAFORM_BODY_LIMIT, 4 * 1024);
    }
}
