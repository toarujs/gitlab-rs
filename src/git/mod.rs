#![allow(dead_code)]

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod pack;

use super::state::AppState;

#[derive(Debug, Clone)]
pub struct GitState {
    pub repository_root: PathBuf,
    pub gitaly_address: Option<String>,
    pub gitaly_token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GitQuery {
    pub service: Option<String>,
    pub state: Option<String>,
}

pub async fn handle_git_request(
    State(state): State<AppState>,
    _method: Method,
    Path(path): Path<String>,
    Query(query): Query<GitQuery>,
    _headers: HeaderMap,
    body: String,
) -> Result<Response, StatusCode> {
    let repo_path = state.git.repository_root.join(&path);

    let canonical = match tokio::fs::canonicalize(&repo_path).await {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!("Git: path not canonicalizable: {}", path);
            return Err(StatusCode::NOT_FOUND);
        }
    };

    if !canonical.starts_with(&state.git.repository_root) {
        tracing::warn!("Git: path traversal attempt: {}", path);
        return Err(StatusCode::FORBIDDEN);
    }

    state.metrics.record_git_operation();

    let service = query.service.unwrap_or_else(|| {
        if path.ends_with("/info/refs") {
            if body.contains("git-receive-pack") || path.contains("git-receive-pack") {
                "git-receive-pack".to_string()
            } else {
                "git-upload-pack".to_string()
            }
        } else if path.ends_with("/git-receive-pack") {
            "git-receive-pack".to_string()
        } else if path.ends_with("/git-upload-pack") {
            "git-upload-pack".to_string()
        } else {
            "unknown".to_string()
        }
    });

    if !canonical.exists() && service != "unknown" {
        return Err(StatusCode::NOT_FOUND);
    }

    match service.as_str() {
        "git-upload-pack" => handle_upload_pack(state, path, _headers, body, canonical).await,
        "git-receive-pack" => handle_receive_pack(state, path, _headers, body, canonical).await,
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

async fn handle_upload_pack(
    state: AppState,
    path: String,
    _headers: HeaderMap,
    body: String,
    repo_path: PathBuf,
) -> Result<Response, StatusCode> {
    if path.ends_with("/info/refs") {
        let refs_list = pack::create_info_refs(&repo_path).map_err(|e| {
            tracing::error!("Failed to get references: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let mut response_headers = HeaderMap::new();
        response_headers.insert(
            "content-type",
            "application/x-git-upload-pack-advertisement"
                .parse()
                .unwrap(),
        );
        response_headers.insert("cache-control", "no-cache".parse().unwrap());

        Ok((StatusCode::OK, response_headers, refs_list).into_response())
    } else if path.ends_with("/git-upload-pack") {
        let wants = pack::parse_pack_request(body.as_bytes()).map_err(|e| {
            tracing::error!("Failed to parse pack request: {}", e);
            StatusCode::BAD_REQUEST
        })?;

        let pack_data = pack::create_pack_file(&repo_path, &wants).map_err(|e| {
            tracing::error!("Failed to create pack file: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let mut response_headers = HeaderMap::new();
        response_headers.insert(
            "content-type",
            "application/x-git-packed-objects".parse().unwrap(),
        );
        response_headers.insert("cache-control", "no-cache".parse().unwrap());

        Ok((StatusCode::OK, response_headers, pack_data.to_vec()).into_response())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn handle_receive_pack(
    state: AppState,
    path: String,
    _headers: HeaderMap,
    body: String,
    repo_path: PathBuf,
) -> Result<Response, StatusCode> {
    if path.ends_with("/info/refs") {
        if !repo_path.exists() {
            return Err(StatusCode::NOT_FOUND);
        }

        let refs_list = pack::create_info_refs(&repo_path).map_err(|e| {
            tracing::error!("Failed to get references: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let mut response_headers = HeaderMap::new();
        response_headers.insert(
            "content-type",
            "application/x-git-receive-pack-advertisement"
                .parse()
                .unwrap(),
        );
        response_headers.insert("cache-control", "no-cache".parse().unwrap());

        Ok((StatusCode::OK, response_headers, refs_list).into_response())
    } else if path.ends_with("/git-receive-pack") {
        if !repo_path.exists() {
            return Err(StatusCode::NOT_FOUND);
        }

        match pack::process_receive_pack(&repo_path, body.as_bytes()) {
            Ok(report) => {
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    "content-type",
                    "application/x-git-receive-pack-result".parse().unwrap(),
                );

                Ok((StatusCode::OK, response_headers, report).into_response())
            }
            Err(e) => {
                tracing::error!("Failed to process receive-pack: {}", e);
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    "content-type",
                    "application/x-git-receive-pack-result".parse().unwrap(),
                );
                let error_report = format!("unpack error\n{}", e);
                Ok((StatusCode::OK, response_headers, error_report).into_response())
            }
        }
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub async fn handle_git_clone(
    State(state): State<AppState>,
    Path(path): Path<String>,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let repo_path = state.git.repository_root.join(&path);

    let canonical = match tokio::fs::canonicalize(&repo_path).await {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!("Git clone: path not found: {}", path);
            return Err(StatusCode::NOT_FOUND);
        }
    };

    if !canonical.starts_with(&state.git.repository_root) {
        tracing::warn!("Git clone: path traversal attempt: {}", path);
        return Err(StatusCode::FORBIDDEN);
    }

    if !canonical.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    state.metrics.record_git_operation();

    let pack_data = pack::create_full_pack(&canonical).map_err(|e| {
        tracing::error!("Failed to create clone pack: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "content-type",
        "application/x-git-packed-objects".parse().unwrap(),
    );
    let filename = canonical.file_name().unwrap_or_default().to_string_lossy();
    let sanitized = filename.chars()
        .map(|c| if c == '\r' || c == '\n' || c == '"' || c == '\\' { '_' } else { c })
        .collect::<String>();
    response_headers.insert(
        "content-disposition",
        format!("attachment; filename=\"{}.pack\"", sanitized)
            .parse()
            .unwrap(),
    );

    Ok((StatusCode::OK, response_headers, pack_data.to_vec()).into_response())
}
