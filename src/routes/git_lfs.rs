//! Git LFS object GET/PUT streaming.
//!
//! Batch (`/info/lfs/objects/batch`) stays on Rails. Object GET/PUT use the
//! authorize result (X-Sendfile / send-data / TempPath) so the blob never
//! buffers through Puma.

#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use std::time::Instant;

use crate::compression::SkipGitPackCompression;
use crate::hotpath::LFS_OBJECT_PATH_TEMPLATE;
use crate::proxy;
use crate::state::AppState;

pub const LFS_MAX_OBJECT_SIZE: u64 = 5 * 1024 * 1024 * 1024;

const OBJECTS_MARK: &str = "/gitlab-lfs/objects/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LfsObjectRef {
    pub oid: String,
    pub size: Option<u64>,
}

pub fn parse_lfs_object(path: &str) -> Option<LfsObjectRef> {
    let path = path.split('?').next().unwrap_or(path);
    if !path.contains(".git/") {
        return None;
    }
    let idx = path.find(OBJECTS_MARK)?;
    let rest = path[idx + OBJECTS_MARK.len()..].trim_end_matches('/');
    if rest.is_empty() {
        return None;
    }
    let mut segs = rest.split('/');
    let oid = segs.next()?;
    if oid.len() != 64 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let size = match segs.next() {
        None => None,
        Some(s) => {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(s.parse().ok()?)
        }
    };
    if segs.next().is_some() {
        return None;
    }
    Some(LfsObjectRef {
        oid: oid.to_string(),
        size,
    })
}

pub fn is_lfs_object_put_path(path: &str) -> bool {
    matches!(parse_lfs_object(path), Some(parsed) if parsed.size.is_some())
}

pub fn is_lfs_object_get_path(path: &str) -> bool {
    parse_lfs_object(path).is_some()
}

pub fn exceeds_lfs_cap(size: u64) -> bool {
    size > LFS_MAX_OBJECT_SIZE
}

pub async fn handle_lfs_object(
    state: AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let started = Instant::now();
    let parsed = parse_lfs_object(uri.path());
    match method {
        Method::PUT => handle_put(state, uri, headers, body, parsed, started).await,
        Method::GET | Method::HEAD => {
            handle_get(state, method, uri, headers, started).await
        }
        _ => {
            record(&state, "fallback", started);
            (StatusCode::METHOD_NOT_ALLOWED, "Method not allowed").into_response()
        }
    }
}

pub async fn handle_lfs_upload(
    State(state): State<AppState>,
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
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    if exceeds_lfs_cap(content_length) {
        return too_large();
    }

    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

async fn handle_put(
    state: AppState,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
    parsed: Option<LfsObjectRef>,
    started: Instant,
) -> Response {
    if let Some(size) = parsed.and_then(|obj| obj.size) {
        if exceeds_lfs_cap(size) {
            record(&state, "hit", started);
            return too_large();
        }
    }
    if let Some(len) = content_length(&headers) {
        if exceeds_lfs_cap(len) {
            record(&state, "hit", started);
            return too_large();
        }
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/octet-stream") {
        record(&state, "fallback", started);
        return (StatusCode::BAD_REQUEST, "Expected application/octet-stream").into_response();
    }

    let path = uri.path().to_string();
    let response = crate::upload::request_body::accelerate_request_body(
        state.clone(),
        Method::PUT,
        uri,
        headers,
        body,
    )
    .await;
    let status = response.status();
    let result = if status.is_server_error() {
        "error"
    } else {
        "hit"
    };
    record(&state, result, started);
    tracing::info!(
        path = %path,
        status = status.as_u16(),
        "Git LFS upload accelerated"
    );
    skip_compress(response)
}

async fn handle_get(
    state: AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    started: Instant,
) -> Response {
    let (status, resp_headers, resp_body) =
        match proxy::send_raw_upstream(&state, method, uri, headers, Body::empty()).await {
            Ok(parts) => parts,
            Err(status) => {
                record(&state, "error", started);
                return (status, "").into_response();
            }
        };

    if !status.is_success() {
        record(&state, "fallback", started);
        return passthrough(status, &resp_headers, resp_body);
    }

    if let Some(inject) =
        crate::senddata::intercept_send_data(status, &resp_headers, "", &state.injecters).await
    {
        return finish_streamed(&state, inject, started);
    }

    if let Some(path) = resp_headers
        .get(crate::headers::X_SENDFILE_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        let filtered = crate::forwardheaders::forward_response_headers(&resp_headers, None, &[]);
        match crate::senddata::sendfile::stream_path(path, filtered).await {
            Ok(resp) => return finish_streamed(&state, resp, started),
            Err(err_status) => {
                tracing::warn!(
                    path_template = LFS_OBJECT_PATH_TEMPLATE,
                    reason = "sendfile_failed",
                    status = err_status.as_u16(),
                    "LFS object sendfile failed, falling back to Puma body"
                );
            }
        }
    }

    tracing::warn!(
        path_template = LFS_OBJECT_PATH_TEMPLATE,
        reason = "missing_store_path",
        "LFS object accelerate fallback to Puma"
    );
    record(&state, "fallback", started);
    passthrough(status, &resp_headers, resp_body)
}

fn finish_streamed(state: &AppState, resp: Response, started: Instant) -> Response {
    if let Some(len) = content_length(resp.headers()) {
        if exceeds_lfs_cap(len) {
            record(state, "hit", started);
            return too_large();
        }
    }
    record(state, "hit", started);
    skip_compress(resp)
}

fn record(state: &AppState, result: &str, started: Instant) {
    let _ = state.metrics.record_hotpath(
        LFS_OBJECT_PATH_TEMPLATE,
        result,
        started.elapsed().as_secs_f64(),
    );
}

fn skip_compress(mut resp: Response) -> Response {
    resp.extensions_mut().insert(SkipGitPackCompression);
    resp
}

fn too_large() -> Response {
    (StatusCode::PAYLOAD_TOO_LARGE, "LFS object too large").into_response()
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

fn passthrough(status: StatusCode, headers: &HeaderMap, body: Bytes) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    for (key, value) in headers.iter() {
        if key.as_str().eq_ignore_ascii_case("content-length") {
            continue;
        }
        response.headers_mut().append(key.clone(), value.clone());
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "b3fc39d68311a9d79120ea7a0dcc71b7e05c303c76789370c3ccef27da0bd79b";

    #[test]
    fn test_lfs_max_object_size() {
        assert_eq!(LFS_MAX_OBJECT_SIZE, 5 * 1024 * 1024 * 1024);
        assert!(!exceeds_lfs_cap(LFS_MAX_OBJECT_SIZE));
        assert!(exceeds_lfs_cap(LFS_MAX_OBJECT_SIZE + 1));
    }

    #[test]
    fn parse_download_oid_only() {
        let parsed = parse_lfs_object(&format!("/group/repo.git/gitlab-lfs/objects/{OID}")).unwrap();
        assert_eq!(parsed.oid, OID);
        assert_eq!(parsed.size, None);
        assert!(is_lfs_object_get_path(&format!(
            "/group/repo.git/gitlab-lfs/objects/{OID}"
        )));
        assert!(!is_lfs_object_put_path(&format!(
            "/group/repo.git/gitlab-lfs/objects/{OID}"
        )));
    }

    #[test]
    fn parse_upload_oid_and_size() {
        let parsed =
            parse_lfs_object(&format!("/g/r.git/gitlab-lfs/objects/{OID}/33")).unwrap();
        assert_eq!(parsed.oid, OID);
        assert_eq!(parsed.size, Some(33));
        assert!(is_lfs_object_put_path(&format!(
            "/g/r.git/gitlab-lfs/objects/{OID}/33"
        )));
        assert!(is_lfs_object_get_path(&format!(
            "/g/r.git/gitlab-lfs/objects/{OID}/33"
        )));
    }

    #[test]
    fn parse_rejects_non_git_and_junk() {
        assert!(parse_lfs_object(&format!("/api/v4/projects/1/lfs/objects/{OID}")).is_none());
        assert!(parse_lfs_object("/g/r.git/gitlab-lfs/objects/notahash/33").is_none());
        assert!(parse_lfs_object(&format!(
            "/g/r.git/gitlab-lfs/objects/{OID}/33/extra"
        ))
        .is_none());
        assert!(parse_lfs_object("/g/r.git/info/lfs/objects/batch").is_none());
    }
}
