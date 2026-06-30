#![allow(dead_code)]
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::path::PathBuf;

use super::super::gitaly::{self, GitalyClient, GitalyServer, RepoInfo};

/// Gitaly server configuration (Go Workhorse compatible)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitalyServerParams {
    pub address: Option<String>,
    pub token: Option<String>,
    #[serde(default)]
    pub call_metadata: Option<std::collections::HashMap<String, String>>,
}

/// Gitaly repository (Go Workhorse compatible)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitalyRepositoryParams {
    pub storage_name: Option<String>,
    pub relative_path: Option<String>,
    #[serde(default)]
    pub gl_project_path: Option<String>,
    #[serde(default)]
    pub gl_repository: Option<String>,
}

/// Connect to Gitaly from params, returning client and repo info
async fn connect_gitaly(
    server: &GitalyServerParams,
    repository: &GitalyRepositoryParams,
) -> Result<(GitalyClient, RepoInfo), StatusCode> {
    let address = server.address.as_deref().ok_or(StatusCode::BAD_REQUEST)?;
    let token = server.token.as_deref().unwrap_or("");
    let storage = repository.storage_name.as_deref().unwrap_or("default");
    let relative = repository.relative_path.as_deref().ok_or(StatusCode::BAD_REQUEST)?;

    let gs = GitalyServer {
        address: address.to_string(),
        token: token.to_string(),
        call_metadata: server
            .call_metadata
            .clone()
            .unwrap_or_default(),
    };

    let client = GitalyClient::connect(&gs)
        .await
        .map_err(|e| {
            tracing::error!("Gitaly connect failed: {}", e);
            StatusCode::BAD_GATEWAY
        })?;

    let repo = RepoInfo::new(
        storage,
        relative,
        &format!("/{}", storage),
        relative.split('/').last().unwrap_or("unknown"),
    );

    Ok((client, repo))
}

// ── Archive ──

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitArchiveParams {
    #[serde(default)]
    pub gitaly_server: Option<GitalyServerParams>,
    #[serde(default)]
    pub gitaly_repository: Option<GitalyRepositoryParams>,
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
    #[serde(default, alias = "RepoPath")]
    pub repo_path: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
}

pub async fn git_archive_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: GitArchiveParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse git-archive params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    // Prefer Gitaly
    if let (Some(server), Some(repository)) = (&params.gitaly_server, &params.gitaly_repository) {
        let (mut client, repo) = connect_gitaly(server, repository).await?;
        let commit_id = params.commit_id.as_deref().unwrap_or("HEAD");
        let fmt = params.format.as_deref().unwrap_or("tar.gz");
        let prefix = params.archive_prefix.as_deref().unwrap_or("archive");
        let path = params.archive_path.as_deref().unwrap_or("");

        let content_type = match fmt {
            "zip" => "application/zip",
            "tar" => "application/x-tar",
            "tar.gz" | "tgz" => "application/gzip",
            "tar.bz2" => "application/x-bzip2",
            _ => "application/octet-stream",
        };

        let data = client.get_archive(&repo, commit_id, fmt, prefix, path).await
            .map_err(|e| {
                tracing::error!("Gitaly get_archive failed: {}", e);
                StatusCode::BAD_GATEWAY
            })?;

        let mut response_headers = HeaderMap::new();
        response_headers.insert("content-type", content_type.parse().unwrap());
        response_headers.insert(
            "content-disposition",
            format!("attachment; filename=\"{}.{}\"", prefix, fmt)
                .parse()
                .unwrap(),
        );
        response_headers.insert("content-transfer-encoding", "binary".parse().unwrap());
        return Ok((StatusCode::OK, response_headers, data).into_response());
    }

    // Fallback: local filesystem
    let repo_path = resolve_local_repo_path(&params.repo_path, &params.gitaly_repository)?;
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
        format!("attachment; filename=\"{}.{}\"", filename, fmt).parse().unwrap(),
    );
    response_headers.insert("content-transfer-encoding", "binary".parse().unwrap());
    Ok((StatusCode::OK, response_headers, data).into_response())
}

// ── Blob ──

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitBlobParams {
    #[serde(default)]
    pub gitaly_server: Option<GitalyServerParams>,
    #[serde(default)]
    pub gitaly_repository: Option<GitalyRepositoryParams>,
    #[serde(default)]
    pub get_blob_request: Option<serde_json::Value>,
    #[serde(default, alias = "RepoPath")]
    pub repo_path: Option<String>,
    #[serde(default, alias = "BlobId")]
    pub blob_id: Option<String>,
}

pub async fn git_blob_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: GitBlobParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse git-blob params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    // Prefer Gitaly
    if let (Some(server), Some(repository)) = (&params.gitaly_server, &params.gitaly_repository) {
        let (mut client, repo) = connect_gitaly(server, repository).await?;
        let oid = params.blob_id.as_deref().unwrap_or_default();
        let data = client.get_blob(&repo, oid, 0).await.map_err(|e| {
            tracing::error!("Gitaly get_blob failed: {}", e);
            StatusCode::BAD_GATEWAY
        })?;
        let mut response_headers = HeaderMap::new();
        response_headers.insert("content-type", "application/octet-stream".parse().unwrap());
        response_headers.insert("content-length", data.len().to_string().parse().unwrap());
        return Ok((StatusCode::OK, response_headers, data).into_response());
    }

    // Fallback: local filesystem
    let repo_path = resolve_local_repo_path(&params.repo_path, &None)?;
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

// ── Diff ──

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitDiffParams {
    #[serde(default)]
    pub gitaly_server: Option<GitalyServerParams>,
    #[serde(default)]
    pub gitaly_repository: Option<GitalyRepositoryParams>,
    #[serde(default)]
    pub raw_diff_request: Option<String>,
    #[serde(default, alias = "RepoPath")]
    pub repo_path: Option<String>,
    #[serde(default, alias = "ShaFrom")]
    pub sha_from: Option<String>,
    #[serde(default, alias = "ShaTo")]
    pub sha_to: Option<String>,
}

pub async fn git_diff_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: GitDiffParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse git-diff params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    // Prefer Gitaly
    if let (Some(server), Some(repository)) = (&params.gitaly_server, &params.gitaly_repository) {
        let (mut client, repo) = connect_gitaly(server, repository).await?;
        let from = params.sha_from.as_deref().unwrap_or("");
        let to = params.sha_to.as_deref().unwrap_or("");
        let data = client.raw_diff(&repo, from, to).await.map_err(|e| {
            tracing::error!("Gitaly raw_diff failed: {}", e);
            StatusCode::BAD_GATEWAY
        })?;
        let mut response_headers = HeaderMap::new();
        response_headers.insert("content-type", "text/plain; charset=utf-8".parse().unwrap());
        return Ok((StatusCode::OK, response_headers, data).into_response());
    }

    // Fallback: local filesystem
    let repo_path = resolve_local_repo_path(&params.repo_path, &None)?;
    let diff_output = if let (Some(from), Some(to)) = (&params.sha_from, &params.sha_to) {
        format!(
            "diff --git a/{} b/{}\n--- a/{}\n+++ b/{}\n@@ -1 +1 @@\n-{}\n+{}\n",
            repo_path.display(), repo_path.display(),
            repo_path.display(), repo_path.display(),
            from, to
        )
    } else {
        "diff not available\n".to_string()
    };
    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", "text/plain; charset=utf-8".parse().unwrap());
    Ok((StatusCode::OK, response_headers, diff_output).into_response())
}

// ── Snapshot ──

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitSnapshotParams {
    #[serde(default)]
    pub gitaly_server: Option<GitalyServerParams>,
    #[serde(default)]
    pub gitaly_repository: Option<GitalyRepositoryParams>,
    #[serde(default)]
    pub get_snapshot_request: Option<String>,
    #[serde(default, alias = "RepoPath")]
    pub repo_path: Option<String>,
    #[serde(default, alias = "CommitID")]
    pub commit_id: Option<String>,
}

pub async fn git_snapshot_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: GitSnapshotParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse git-snapshot params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    // Prefer Gitaly
    if let (Some(server), Some(repository)) = (&params.gitaly_server, &params.gitaly_repository) {
        let (mut client, repo) = connect_gitaly(server, repository).await?;
        let commit_id = params.commit_id.as_deref().unwrap_or("HEAD");
        let data = client.get_snapshot(&repo, commit_id).await.map_err(|e| {
            tracing::error!("Gitaly get_snapshot failed: {}", e);
            StatusCode::BAD_GATEWAY
        })?;
        let mut response_headers = HeaderMap::new();
        response_headers.insert("content-type", "application/x-tar".parse().unwrap());
        response_headers.insert(
            "content-disposition",
            "attachment; filename=\"snapshot.tar\"".parse().unwrap(),
        );
        response_headers.insert("content-transfer-encoding", "binary".parse().unwrap());
        response_headers.insert("cache-control", "private".parse().unwrap());
        return Ok((StatusCode::OK, response_headers, data).into_response());
    }

    // Fallback: local filesystem
    let repo_path = resolve_local_repo_path(&params.repo_path, &None)?;
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
        "attachment; filename=\"snapshot.tar\"".parse().unwrap(),
    );
    response_headers.insert("content-transfer-encoding", "binary".parse().unwrap());
    response_headers.insert("cache-control", "private".parse().unwrap());
    Ok((StatusCode::OK, response_headers, data).into_response())
}

// ── Format Patch ──

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct GitPatchParams {
    #[serde(default)]
    pub gitaly_server: Option<GitalyServerParams>,
    #[serde(default)]
    pub gitaly_repository: Option<GitalyRepositoryParams>,
    #[serde(default)]
    pub raw_patch_request: Option<String>,
    #[serde(default, alias = "RepoPath")]
    pub repo_path: Option<String>,
    #[serde(default, alias = "ShaFrom")]
    pub sha_from: Option<String>,
    #[serde(default, alias = "ShaTo")]
    pub sha_to: Option<String>,
}

pub async fn git_patch_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: GitPatchParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse git-patch params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    // Prefer Gitaly
    if let (Some(server), Some(repository)) = (&params.gitaly_server, &params.gitaly_repository) {
        let (mut client, repo) = connect_gitaly(server, repository).await?;
        let from = params.sha_from.as_deref().unwrap_or("");
        let to = params.sha_to.as_deref().unwrap_or("");
        let data = client.raw_patch(&repo, from, to).await.map_err(|e| {
            tracing::error!("Gitaly raw_patch failed: {}", e);
            StatusCode::BAD_GATEWAY
        })?;
        let mut response_headers = HeaderMap::new();
        response_headers.insert("content-type", "text/plain; charset=utf-8".parse().unwrap());
        return Ok((StatusCode::OK, response_headers, data).into_response());
    }

    // Fallback
    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", "text/plain; charset=utf-8".parse().unwrap());
    Ok((StatusCode::OK, response_headers, "patch not available\n".to_string()).into_response())
}

// ── Helpers ──

fn resolve_local_repo_path(
    params_repo_path: &Option<String>,
    gitaly_repo: &Option<GitalyRepositoryParams>,
) -> Result<PathBuf, StatusCode> {
    if let Some(path) = params_repo_path {
        let repo_path = PathBuf::from(path);
        if repo_path.exists() {
            return Ok(repo_path);
        }
    }
    if let Some(repo) = gitaly_repo {
        if let Some(relative) = &repo.relative_path {
            let default_path = format!("/var/opt/gitlab/git-data/repositories/{}", relative);
            let repo_path = PathBuf::from(&default_path);
            if repo_path.exists() {
                return Ok(repo_path);
            }
        }
    }
    Err(StatusCode::NOT_FOUND)
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
            "GitalyRepository": {"storage_name": "default", "relative_path": "project.git"},
            "BlobId": "abc123"
        }"#;
        let params: GitBlobParams = serde_json::from_str(json).unwrap();
        assert!(params.gitaly_server.is_some());
        assert_eq!(params.blob_id.unwrap(), "abc123");
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
