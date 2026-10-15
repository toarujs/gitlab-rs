#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};
use http_body_util::BodyExt;
use uuid::Uuid;

const UPLOAD_MAX_SIZE: usize = 100 * 1024 * 1024; // 100MB

/// True when the request hits the collection `POST /repository/commits`
/// endpoint, which Rails serves through Workhorse's request-body uploader.
fn is_commits_body_upload(path: &str) -> bool {
    crate::cve_guard::normalize_api_path(path).ends_with("/repository/commits")
}

/// True when the request hits a `POST`/`PUT /repository/files/:path` write,
/// which likewise needs the Workhorse body uploader. The internal
/// `.../authorize` sub-endpoint is excluded so a direct client call is proxied
/// to Rails untouched instead of being authorized recursively.
fn is_files_body_upload(path: &str) -> bool {
    let normalized = crate::cve_guard::normalize_api_path(path).to_ascii_lowercase();
    normalized.contains("/repository/files/") && !normalized.ends_with("/authorize")
}

/// Buffer the client body to a temp file and rewrite it into the signed
/// `file.*` form Rails' commits/files write API expects.
async fn handle_repository_body_upload(
    state: crate::state::AppState,
    req: Request<Body>,
) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
    let path = parts.uri.path().to_string();
    // Inspect the original client body before buffering it; the guard rejects
    // client-injected workhorse upload path fields.
    let (headers, body) =
        match crate::cve_guard::guard_repository_write(&method, &path, parts.headers, body, false)
            .await
        {
            Ok(pair) => pair,
            Err(status) => return (status, "").into_response(),
        };
    crate::upload::request_body::accelerate_request_body(state, method, parts.uri, headers, body).await
}

pub async fn handle_project_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let content_length = req
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > UPLOAD_MAX_SIZE {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Upload too large").into_response();
    }

    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_avatar_upload(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let (parts, body) = req.into_parts();

    let body_bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => return (StatusCode::BAD_REQUEST, "Failed to read body").into_response(),
    };

    if body_bytes.len() > UPLOAD_MAX_SIZE {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Upload too large").into_response();
    }

    let content_type = parts.headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let upload_id = Uuid::new_v4().to_string();
    let tmp_dir = std::path::PathBuf::from("/var/opt/gitlab/gitlab-rails/tmp/workhorse");
    if let Err(e) = tokio::fs::create_dir_all(&tmp_dir).await {
        tracing::error!("Failed to create workhorse temp dir: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "Upload setup failed").into_response();
    }

    let tmp_path = tmp_dir.join(&upload_id);
    if let Err(e) = tokio::fs::write(&tmp_path, &body_bytes).await {
        tracing::error!("Failed to write temp upload file: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "Upload failed").into_response();
    }

    let metadata = serde_json::json!({
        "file.path": tmp_path.to_string_lossy(),
        "file.size": body_bytes.len(),
        "file.content_type": content_type,
        "file.original_filename": "avatar",
    });
    let json_body = metadata.to_string();

    let mut new_parts = parts;
    new_parts.headers.insert(
        header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    new_parts.headers.insert(
        header::CONTENT_LENGTH,
        json_body.len().to_string().parse().unwrap(),
    );

    let new_req = Request::from_parts(new_parts, Body::from(json_body));

    let result = proxy::proxy_handler(State(state.clone()), new_req).await;

    if let Ok(ref resp) = result {
        if resp.status().is_success() {
            if let Some(ref cache) = state.cache {
                let removed = cache.remove_by_prefix("/uploads/-/system/user/avatar/").await;
                if removed > 0 {
                    tracing::info!("Invalidated {} avatar cache entries after upload", removed);
                }
            }
        }
    }

    match result {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_sbom_scan(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_repository_commits(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if req.method() == Method::GET {
        match crate::hotpath::repo::accelerate(&state, &req).await {
            crate::hotpath::repo::AccelResult::Hit(resp) => return resp,
            crate::hotpath::repo::AccelResult::Fallback { reason, error } => {
                tracing::warn!(
                    path_template = %if req.uri().path().contains("/repository/commits/") {
                        crate::hotpath::COMMIT_SHOW_PATH_TEMPLATE
                    } else {
                        crate::hotpath::COMMITS_PATH_TEMPLATE
                    },
                    reason = %reason,
                    error = error,
                    "Repository commit GET accelerate fallback to Puma"
                );
            }
        }
    } else if req.method() == Method::POST && is_commits_body_upload(req.uri().path()) {
        return handle_repository_body_upload(state, req).await;
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_repository_tree(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match crate::hotpath::repo::accelerate(&state, &req).await {
        crate::hotpath::repo::AccelResult::Hit(resp) => return resp,
        crate::hotpath::repo::AccelResult::Fallback { reason, error } => {
            tracing::warn!(
                path_template = %crate::hotpath::TREE_PATH_TEMPLATE,
                reason = %reason,
                error = error,
                "Repository tree GET accelerate fallback to Puma"
            );
        }
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_repository_branches(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match crate::hotpath::repo::accelerate(&state, &req).await {
        crate::hotpath::repo::AccelResult::Hit(resp) => return resp,
        crate::hotpath::repo::AccelResult::Fallback { reason, error } => {
            tracing::warn!(
                path_template = %crate::hotpath::BRANCHES_PATH_TEMPLATE,
                reason = %reason,
                error = error,
                "Repository branches GET accelerate fallback to Puma"
            );
        }
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_repository_archive(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    match crate::hotpath::repo::accelerate(&state, &req).await {
        crate::hotpath::repo::AccelResult::Hit(resp) => return resp,
        crate::hotpath::repo::AccelResult::Fallback { reason, error } => {
            tracing::warn!(
                path_template = %crate::hotpath::ARCHIVE_PATH_TEMPLATE,
                reason = %reason,
                error = error,
                "Repository archive GET accelerate fallback to Puma"
            );
        }
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

pub async fn handle_repository_files(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let method = req.method().clone();
    if method == Method::GET || method == Method::HEAD {
        match crate::hotpath::files::accelerate(&state, &req).await {
            crate::hotpath::files::AccelResult::Hit(resp) => return resp,
            crate::hotpath::files::AccelResult::Fallback { reason, error } => {
                tracing::warn!(
                    path_template = %crate::hotpath::FILES_PATH_TEMPLATE,
                    reason = %reason,
                    error = error,
                    "Files GET accelerate fallback to Puma"
                );
            }
        }
    } else if (method == Method::POST || method == Method::PUT)
        && is_files_body_upload(req.uri().path())
    {
        return handle_repository_body_upload(state, req).await;
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
    fn test_upload_max_size() {
        assert_eq!(UPLOAD_MAX_SIZE, 100 * 1024 * 1024);
    }
}
