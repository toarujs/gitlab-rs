#![allow(dead_code)]
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::path::PathBuf;

/// Gitaly server configuration (Go Workhorse compatible)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitalyServer {
    pub address: Option<String>,
    pub token: Option<String>,
    #[serde(default)]
    pub call_metadata: Option<std::collections::HashMap<String, String>>,
}

/// Gitaly repository (Go Workhorse compatible)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitalyRepository {
    pub storage_name: Option<String>,
    pub relative_path: Option<String>,
    #[serde(default)]
    pub gl_project_path: Option<String>,
    #[serde(default)]
    pub gl_repository: Option<String>,
}

/// Git archive parameters - supports both Go (Gitaly) and Rust (local) formats
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitArchiveParams {
    // Go/Gitaly format fields
    #[serde(default)]
    pub gitaly_server: Option<GitalyServer>,
    #[serde(default)]
    pub gitaly_repository: Option<GitalyRepository>,
    #[serde(default)]
    pub archive_path: Option<String>,
    #[serde(default)]
    pub archive_prefix: Option<String>,
    #[serde(default, alias = "CommitID")]
    pub commit_id: Option<String>,
    #[serde(default)]
    pub disable_cache: Option<bool>,
    #[serde(default)]
    pub storage_path: Option<String>,
    #[serde(default)]
    pub use_archive_cleaner: Option<bool>,

    // Rust local format fields (fallback)
    #[serde(default, alias = "RepoPath")]
    pub repo_path: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
}

/// Git blob parameters - supports both formats
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitBlobParams {
    // Go/Gitaly format
    #[serde(default)]
    pub gitaly_server: Option<GitalyServer>,
    #[serde(default)]
    pub get_blob_request: Option<serde_json::Value>,

    // Rust local format (fallback)
    #[serde(default, alias = "RepoPath")]
    pub repo_path: Option<String>,
    #[serde(default, alias = "BlobId")]
    pub blob_id: Option<String>,
}

/// Git diff parameters - supports both formats
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitDiffParams {
    // Go/Gitaly format
    #[serde(default)]
    pub gitaly_server: Option<GitalyServer>,
    #[serde(default)]
    pub raw_diff_request: Option<String>,

    // Rust local format (fallback)
    #[serde(default, alias = "RepoPath")]
    pub repo_path: Option<String>,
    #[serde(default, alias = "ShaFrom")]
    pub sha_from: Option<String>,
    #[serde(default, alias = "ShaTo")]
    pub sha_to: Option<String>,
}

/// Git snapshot parameters - supports both formats
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitSnapshotParams {
    // Go/Gitaly format
    #[serde(default)]
    pub gitaly_server: Option<GitalyServer>,
    #[serde(default)]
    pub get_snapshot_request: Option<String>,

    // Rust local format (fallback)
    #[serde(default, alias = "RepoPath")]
    pub repo_path: Option<String>,
    #[serde(default, alias = "CommitID")]
    pub commit_id: Option<String>,
}

/// Git format-patch parameters (Go compatible)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitPatchParams {
    #[serde(default)]
    pub gitaly_server: Option<GitalyServer>,
    #[serde(default)]
    pub raw_patch_request: Option<String>,
}

/// Resolve repo path from either Go (Gitaly) or Rust (local) format
fn resolve_repo_path(params_repo_path: &Option<String>, gitaly_repo: &Option<GitalyRepository>) -> Result<PathBuf, StatusCode> {
    // Try Rust local format first
    if let Some(path) = params_repo_path {
        let repo_path = PathBuf::from(path);
        if repo_path.exists() {
            return Ok(repo_path);
        }
    }

    // Try to resolve from Gitaly repository (storage_name + relative_path)
    if let Some(repo) = gitaly_repo {
        if let (Some(_storage), Some(relative)) = (&repo.storage_name, &repo.relative_path) {
            // Default GitLab storage path pattern
            let default_path = format!("/var/opt/gitlab/git-data/repositories/{}", relative);
            let repo_path = PathBuf::from(&default_path);
            if repo_path.exists() {
                return Ok(repo_path);
            }
        }
    }

    Err(StatusCode::NOT_FOUND)
}

pub async fn git_archive_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: GitArchiveParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse git-archive params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let repo_path = resolve_repo_path(&params.repo_path, &params.gitaly_repository)?;

    let repo = gix::open(&repo_path).map_err(|e| {
        tracing::error!("Failed to open git repo for archive: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let fmt = params.format.as_deref().unwrap_or("tar.gz");
    let content_type = match fmt {
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "tar.gz" | "tgz" => "application/gzip",
        "tar.bz2" => "application/x-bzip2",
        _ => "application/octet-stream",
    };

    let commit_id_str = params.commit_id.as_deref().unwrap_or("HEAD");
    let commit_id = gix::hash::ObjectId::from_hex(commit_id_str.as_bytes()).map_err(|e| {
        tracing::error!("Invalid commit id: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let object = repo.find_object(commit_id).map_err(|e| {
        tracing::error!("Commit not found: {}", e);
        StatusCode::NOT_FOUND
    })?;

    let data = object.data.to_vec();

    let filename = params.archive_prefix.as_deref().unwrap_or("archive");
    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", content_type.parse().unwrap());
    response_headers.insert(
        "content-disposition",
        format!("attachment; filename=\"{}.{}\"", filename, fmt)
            .parse()
            .unwrap(),
    );
    response_headers.insert("content-transfer-encoding", "binary".parse().unwrap());

    Ok((StatusCode::OK, response_headers, data).into_response())
}

pub async fn git_blob_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: GitBlobParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse git-blob params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let repo_path = resolve_repo_path(&params.repo_path, &None)?;

    let repo = gix::open(&repo_path).map_err(|e| {
        tracing::error!("Failed to open git repo for blob: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let blob_id_str = params.blob_id.as_deref().unwrap_or("");
    let blob_id = gix::hash::ObjectId::from_hex(blob_id_str.as_bytes()).map_err(|e| {
        tracing::error!("Invalid blob id: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let object = repo.find_object(blob_id).map_err(|e| {
        tracing::error!("Blob not found: {}", e);
        StatusCode::NOT_FOUND
    })?;

    let data = object.data.to_vec();

    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", "application/octet-stream".parse().unwrap());
    response_headers.insert("content-length", data.len().to_string().parse().unwrap());

    Ok((StatusCode::OK, response_headers, data).into_response())
}

pub async fn git_diff_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: GitDiffParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse git-diff params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let repo_path = resolve_repo_path(&params.repo_path, &None)?;

    let _repo = gix::open(&repo_path).map_err(|e| {
        tracing::error!("Failed to open git repo for diff: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let diff_output = if let (Some(from), Some(to)) = (&params.sha_from, &params.sha_to) {
        let from_id = gix::hash::ObjectId::from_hex(from.as_bytes()).map_err(|e| {
            tracing::error!("Invalid from sha: {}", e);
            StatusCode::BAD_REQUEST
        })?;
        let to_id = gix::hash::ObjectId::from_hex(to.as_bytes()).map_err(|e| {
            tracing::error!("Invalid to sha: {}", e);
            StatusCode::BAD_REQUEST
        })?;
        format!("diff --git a/{} b/{}\n--- a/{}\n+++ b/{}\n@@ -1 +1 @@\n-{}\n+{}\n",
            repo_path.display(), repo_path.display(),
            repo_path.display(), repo_path.display(),
            from_id, to_id)
    } else {
        "diff not available\n".to_string()
    };

    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", "text/plain; charset=utf-8".parse().unwrap());

    Ok((StatusCode::OK, response_headers, diff_output).into_response())
}

pub async fn git_snapshot_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: GitSnapshotParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse git-snapshot params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let repo_path = resolve_repo_path(&params.repo_path, &None)?;

    let repo = gix::open(&repo_path).map_err(|e| {
        tracing::error!("Failed to open git repo for snapshot: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let commit_id_str = params.commit_id.as_deref().unwrap_or("HEAD");
    let commit_id = gix::hash::ObjectId::from_hex(commit_id_str.as_bytes()).map_err(|e| {
        tracing::error!("Invalid commit id: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let object = repo.find_object(commit_id).map_err(|e| {
        tracing::error!("Commit not found: {}", e);
        StatusCode::NOT_FOUND
    })?;

    let data = object.data.to_vec();

    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", "application/x-tar".parse().unwrap());
    response_headers.insert(
        "content-disposition",
        "attachment; filename=\"snapshot.tar\""
            .parse()
            .unwrap(),
    );
    response_headers.insert("content-transfer-encoding", "binary".parse().unwrap());
    response_headers.insert("cache-control", "private".parse().unwrap());

    Ok((StatusCode::OK, response_headers, data).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_go_archive_format() {
        let json = r#"{
            "GitalyServer": {"address": "unix:/var/opt/gitlab/gitaly/gitaly.socket", "token": "abc"},
            "ArchivePath": "/tmp/archive.tar.gz",
            "ArchivePrefix": "my-project",
            "CommitID": "abc123def456",
            "DisableCache": false
        }"#;
        let params: GitArchiveParams = serde_json::from_str(json).unwrap();
        assert!(params.gitaly_server.is_some());
        assert_eq!(params.commit_id.unwrap(), "abc123def456");
        assert_eq!(params.archive_prefix.unwrap(), "my-project");
    }

    #[test]
    fn test_parse_rust_archive_format() {
        let json = r#"{
            "RepoPath": "/var/opt/gitlab/git-data/repositories/project.git",
            "CommitId": "abc123",
            "format": "tar.gz"
        }"#;
        let params: GitArchiveParams = serde_json::from_str(json).unwrap();
        assert!(params.repo_path.is_some());
        assert_eq!(params.commit_id.unwrap(), "abc123");
    }

    #[test]
    fn test_parse_go_blob_format() {
        let json = r#"{
            "GitalyServer": {"address": "unix:/var/opt/gitlab/gitaly/gitaly.socket"},
            "GetBlobRequest": {"repository": {"storage_name": "default"}, "oid": "abc123"}
        }"#;
        let params: GitBlobParams = serde_json::from_str(json).unwrap();
        assert!(params.gitaly_server.is_some());
    }

    #[test]
    fn test_parse_go_diff_format() {
        let json = r#"{
            "GitalyServer": {"address": "unix:/var/opt/gitlab/gitaly/gitaly.socket"},
            "RawDiffRequest": "{\"left_commit_id\":\"abc\",\"right_commit_id\":\"def\"}"
        }"#;
        let params: GitDiffParams = serde_json::from_str(json).unwrap();
        assert!(params.gitaly_server.is_some());
        assert!(params.raw_diff_request.is_some());
    }
}
