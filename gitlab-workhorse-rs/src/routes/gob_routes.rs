#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use crate::gob::GobProxy;
use crate::proxy::{self, ProxyState};

/// Rewrite the request path for GOB (GitLab Observability Backend) requests
/// Extracts project ID and rewrites path to /write/* or /read/*
fn rewrite_gob_path(uri: &Uri) -> (Option<String>, String) {
    let path = uri.path();
    let project_id = GobProxy::extract_project_id(path);
    let rewritten = GobProxy::rewrite_path(path);
    (project_id, rewritten)
}

pub async fn handle_gob_traces(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let (project_id, _rewritten_path) = rewrite_gob_path(req.uri());

    if project_id.is_none() {
        return (StatusCode::BAD_REQUEST, "Missing project ID").into_response();
    }

    // Build new URI with rewritten path
    let query = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();
    let new_uri = format!("/api/v4/projects/{}/observability/v1/traces{}", project_id.unwrap(), query);

    // Create new request with rewritten path
    let mut new_req = req;
    *new_req.uri_mut() = new_uri.parse().unwrap();

    match proxy::proxy_handler(State(state), new_req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_gob_logs(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let (project_id, _rewritten_path) = rewrite_gob_path(req.uri());

    if project_id.is_none() {
        return (StatusCode::BAD_REQUEST, "Missing project ID").into_response();
    }

    let query = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();
    let new_uri = format!("/api/v4/projects/{}/observability/v1/logs{}", project_id.unwrap(), query);

    let mut new_req = req;
    *new_req.uri_mut() = new_uri.parse().unwrap();

    match proxy::proxy_handler(State(state), new_req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_gob_metrics(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let (project_id, _rewritten_path) = rewrite_gob_path(req.uri());

    if project_id.is_none() {
        return (StatusCode::BAD_REQUEST, "Missing project ID").into_response();
    }

    let query = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();
    let new_uri = format!("/api/v4/projects/{}/observability/v1/metrics{}", project_id.unwrap(), query);

    let mut new_req = req;
    *new_req.uri_mut() = new_uri.parse().unwrap();

    match proxy::proxy_handler(State(state), new_req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_gob_analytics(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let (project_id, _rewritten_path) = rewrite_gob_path(req.uri());

    if project_id.is_none() {
        return (StatusCode::BAD_REQUEST, "Missing project ID").into_response();
    }

    let query = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();
    let new_uri = format!("/api/v4/projects/{}/observability/v1/analytics{}", project_id.unwrap(), query);

    let mut new_req = req;
    *new_req.uri_mut() = new_uri.parse().unwrap();

    match proxy::proxy_handler(State(state), new_req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_gob_services(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let (project_id, _rewritten_path) = rewrite_gob_path(req.uri());

    if project_id.is_none() {
        return (StatusCode::BAD_REQUEST, "Missing project ID").into_response();
    }

    let query = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();
    let new_uri = format!("/api/v4/projects/{}/observability/v1/services{}", project_id.unwrap(), query);

    let mut new_req = req;
    *new_req.uri_mut() = new_uri.parse().unwrap();

    match proxy::proxy_handler(State(state), new_req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rewrite_gob_path() {
        let uri: Uri = "/api/v4/projects/123/traces".parse().unwrap();
        let (project_id, rewritten) = rewrite_gob_path(&uri);
        assert_eq!(project_id.unwrap(), "123");
        assert_eq!(rewritten, "/traces");
    }

    #[test]
    fn test_rewrite_gob_path_no_project() {
        let uri: Uri = "/api/v4/users".parse().unwrap();
        let (project_id, _rewritten) = rewrite_gob_path(&uri);
        assert!(project_id.is_none());
    }
}
