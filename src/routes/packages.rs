#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

const PACKAGE_MAX_SIZE: usize = 5 * 1024 * 1024 * 1024; // 5GB

fn check_package_size(req: &Request<Body>) -> Result<(), Response> {
    let content_length = req
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > PACKAGE_MAX_SIZE {
        Err((StatusCode::PAYLOAD_TOO_LARGE, "Package too large").into_response())
    } else {
        Ok(())
    }
}

pub async fn handle_maven_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_npm_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_nuget_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_conan_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_generic_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_pypi_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_debian_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_rpm_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_rubygems_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_terraform_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_helm_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_ml_models_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if let Err(resp) = check_package_size(&req) {
        return resp;
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
    fn test_package_max_size() {
        assert_eq!(PACKAGE_MAX_SIZE, 5 * 1024 * 1024 * 1024);
    }
}
