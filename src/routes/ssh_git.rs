#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

pub async fn handle_ssh_upload_pack(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_ssh_receive_pack(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}
