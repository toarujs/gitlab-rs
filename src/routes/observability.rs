#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

pub async fn handle_observability_traces(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_observability_logs(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_observability_metrics(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_observability_analytics(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_observability_services(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}
