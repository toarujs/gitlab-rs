#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

pub async fn handle_oauth_authorize(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_oauth_authorize_device(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_oauth_token(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_oauth_revoke(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_oauth_introspect(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}
