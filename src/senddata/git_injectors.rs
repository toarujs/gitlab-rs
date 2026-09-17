#![allow(dead_code)]
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::path::PathBuf;

use base64::Engine;
use prost::Message as _;

use super::super::gitaly::{gitaly::GetArchiveRequest, GitalyClient, GitalyServer, RepoInfo};

/// Gitaly server configuration (Go Workhorse compatible)
#[derive(Debug, Deserialize)]
pub struct GitalyServerParams {
    pub address: Option<String>,
    pub token: Option<String>,
    #[serde(default)]
    pub call_metadata: Option<std::collections::HashMap<String, String>>,
}

/// Gitaly repository (Go Workhorse compatible)
#[derive(Debug, Deserialize)]
pub struct GitalyRepositoryParams {
    pub storage_name: Option<String>,
    pub relative_path: Option<String>,
    #[serde(default)]
    pub gl_project_path: Option<String>,
    #[serde(default)]
    pub gl_repository: Option<String>,
}

/// Connect to Gitaly from the send-data server params.
async fn connect_gitaly_client(server: &GitalyServerParams) -> Result<GitalyClient, StatusCode> {
    let address = server.address.as_deref().ok_or(StatusCode::BAD_REQUEST)?;

    let gs = GitalyServer {
        address: address.to_string(),
        token: server.token.clone().unwrap_or_default(),
        call_metadata: server.call_metadata.clone().unwrap_or_default(),
    };

    GitalyClient::connect(&gs).await.map_err(|e| {
        tracing::error!("Gitaly connect failed: {}", e);
        StatusCode::BAD_GATEWAY
    })
}

/// Connect to Gitaly and build the `RepoInfo` described by the send-data payload.
async fn connect_gitaly(
    server: &GitalyServerParams,
    repository: &GitalyRepositoryParams,
) -> Result<(GitalyClient, RepoInfo), StatusCode> {
    let storage = repository.storage_name.as_deref().unwrap_or("default");
    let relative = repository.relative_path.as_deref().ok_or(StatusCode::BAD_REQUEST)?;

    let client = connect_gitaly_client(server).await?;

    let gl_project_path = repository
        .gl_project_path
        .clone()
        .unwrap_or_else(|| format!("/{}", storage));
    let gl_repository = repository
        .gl_repository
        .clone()
        .unwrap_or_else(|| relative.rsplit('/').next().unwrap_or("unknown").to_string());

    let repo = RepoInfo::new(
        storage,
        relative,
        &gl_project_path,
        &gl_repository,
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
    /// base64 (`Base64.encode64`) encoded `Gitaly::GetArchiveRequest` protobuf.
    #[serde(default)]
    pub get_archive_request: Option<String>,
}

pub async fn git_archive_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: GitArchiveParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse git-archive params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    // Rails serializes the already-resolved Gitaly request. Decode it up front so
    // the cache path and the fresh-generation path agree on format and filename.
    let request = params
        .get_archive_request
        .as_deref()
        .map(|encoded| {
            decode_binary(encoded)
                .map_err(|e| {
                    tracing::error!("git-archive: bad GetArchiveRequest base64: {}", e);
                    StatusCode::BAD_REQUEST
                })
                .and_then(|raw| {
                    GetArchiveRequest::decode(raw.as_slice()).map_err(|e| {
                        tracing::error!("git-archive: bad GetArchiveRequest protobuf: {}", e);
                        StatusCode::BAD_REQUEST
                    })
                })
        })
        .transpose()?;

    let format = match &request {
        Some(req) => format_name(req.format).to_string(),
        None => params.format.clone().unwrap_or_else(|| "tar.gz".to_string()),
    };
    let prefix = match &request {
        Some(req) if !req.prefix.is_empty() => req.prefix.clone(),
        _ => params
            .archive_prefix
            .clone()
            .unwrap_or_else(|| "archive".to_string()),
    };

    // Serve the cached archive when Rails handed us a path and caching is enabled.
    if !params.disable_cache.unwrap_or(false) {
        if let Some(path) = params.archive_path.as_deref() {
            match tokio::fs::read(path).await {
                Ok(data) => {
                    tracing::info!(
                        "git-archive served from cache: {} ({} bytes)",
                        path,
                        data.len()
                    );
                    let cached_format =
                        if request.is_some() { format.clone() } else { format_from_path(path) };
                    return Ok(archive_response(data, cached_format, Some(prefix)));
                }
                Err(e) => tracing::info!("git-archive cache miss {}: {}", path, e),
            }
        }
    }

    // Generate the archive through Gitaly from the serialized request Rails sent us.
    if let (Some(server), Some(request)) = (&params.gitaly_server, request) {
        let mut client = connect_gitaly_client(server).await?;
        let data = client.get_archive(request).await.map_err(|e| {
            tracing::error!("Gitaly get_archive failed: {}", e);
            StatusCode::BAD_GATEWAY
        })?;

        // Best-effort cache write so repeat downloads skip Gitaly.
        if !params.disable_cache.unwrap_or(false) {
            if let Some(path) = params.archive_path.as_deref() {
                if let Some(parent) = std::path::Path::new(path).parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                // Write to a sibling temp file then rename, so concurrent readers
                // never observe a partially written archive.
                let tmp = format!("{}.tmp{}", path, std::process::id());
                match tokio::fs::write(&tmp, &data).await {
                    Ok(()) => {
                        if let Err(e) = tokio::fs::rename(&tmp, path).await {
                            tracing::warn!("git-archive: failed to commit cache {}: {}", path, e);
                            let _ = tokio::fs::remove_file(&tmp).await;
                        }
                    }
                    Err(e) => tracing::warn!("git-archive: failed to cache {}: {}", path, e),
                }
            }
        }

        return Ok(archive_response(data, format, Some(prefix)));
    }

    // Fallback: local filesystem
    let repo_path = resolve_local_repo_path(&params.repo_path, &params.gitaly_repository)?;
    let repo = gix::open(&repo_path).map_err(|e| {
        tracing::error!("Failed to open git repo for archive: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let fmt = params.format.as_deref().unwrap_or("tar.gz").to_string();
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
    Ok(archive_response(
        data,
        fmt,
        Some(params.archive_prefix.as_deref().unwrap_or("archive").to_string()),
    ))
}

/// Content type matching the archive format (mirrors Go workhorse).
fn archive_content_type(format: &str) -> &'static str {
    match format {
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "tar.gz" | "tgz" => "application/gzip",
        "tar.bz2" => "application/x-bzip2",
        _ => "application/octet-stream",
    }
}

/// Map the `Gitaly::GetArchiveRequest::Format` enum to its file extension.
fn format_name(format: i32) -> &'static str {
    match format {
        0 => "zip",
        1 => "tar",
        3 => "tar.bz2",
        _ => "tar.gz",
    }
}

/// Approximate Go's `http.DetectContentType` for the signatures that matter for
/// `git-blob` responses. Rails asks workhorse to detect the type via the
/// `Gitlab-Workhorse-Detect-Content-Type` header, so blob content must be sniffed
/// rather than trusting the `text/plain` default Rails renders.
fn sniff_content_type(data: &[u8]) -> &'static str {
    let d = &data[..data.len().min(512)];

    // Container formats need a deeper magic match.
    if d.starts_with(b"RIFF") && d.len() >= 12 {
        return match &d[8..12] {
            b"WEBP" => "image/webp",
            b"WAVE" => "audio/wav",
            b"AVI " => "video/x-msvideo",
            _ => "application/octet-stream",
        };
    }
    if d.starts_with(b"FORM") && d.len() >= 12 && &d[8..12] == b"AIFF" {
        return "audio/aiff";
    }

    const SIGNATURES: &[(&[u8], &str)] = &[
        (b"%PDF-", "application/pdf"),
        (b"%!PS-Adobe-", "application/postscript"),
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"BM", "image/bmp"),
        (b"\x00\x00\x01\x00", "image/x-icon"),
        (b"\x00\x00\x02\x00", "image/x-icon"),
        (b"ID3", "audio/mpeg"),
        (b"OggS", "application/ogg"),
        (b"MThd", "audio/midi"),
        (b"\x1f\x8b\x08", "application/x-gzip"),
        (b"BZh", "application/x-bzip2"),
        (b"Rar!\x1a\x07", "application/x-rar-compressed"),
        (b"7z\xbc\xaf\x27\x1c", "application/x-7z-compressed"),
        (b"PK\x03\x04", "application/zip"),
        (b"\x1aE\xdf\xa3", "video/webm"),
        (b"\x00asm", "application/wasm"),
        (b"\x7fELF", "application/octet-stream"),
        (b"wOFF", "font/woff"),
        (b"wOF2", "font/woff2"),
        (b"OTTO", "font/otf"),
        (b"ttcf", "font/collection"),
        (b"\x00\x01\x00\x00", "font/ttf"),
        (b"\xef\xbb\xbf", "text/plain; charset=utf-8"),
        (b"\xff\xfe", "text/plain; charset=utf-16le"),
        (b"\xfe\xff", "text/plain; charset=utf-16be"),
    ];
    for (sig, ct) in SIGNATURES {
        if d.starts_with(sig) {
            return ct;
        }
    }

    // XML / HTML, tolerating leading whitespace and case-insensitive tags.
    let trimmed: Vec<u8> = d.iter().skip_while(|b| b.is_ascii_whitespace()).copied().collect();
    let upper: Vec<u8> = trimmed.iter().map(|b| b.to_ascii_uppercase()).collect();
    if upper.starts_with(b"<?XML") {
        return "text/xml; charset=utf-8";
    }
    const HTML_TAGS: &[&[u8]] = &[
        b"<!DOCTYPE HTML", b"<HTML", b"<HEAD", b"<SCRIPT", b"<IFRAME", b"<H1", b"<DIV",
        b"<FONT", b"<TABLE", b"<A", b"<STYLE", b"<TITLE", b"<B", b"<BODY", b"<BR", b"<P",
        b"<!--",
    ];
    for tag in HTML_TAGS {
        if upper.starts_with(tag) {
            return "text/html; charset=utf-8";
        }
    }

    if is_plain_text(&trimmed) {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

fn is_plain_text(data: &[u8]) -> bool {
    if std::str::from_utf8(data).is_err() {
        return false;
    }
    !data
        .iter()
        .any(|&b| b == 0 || (b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r' && b != 0x0c))
}

fn format_from_path(path: &str) -> String {
    if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
        "tar.gz".to_string()
    } else if path.ends_with(".tar.bz2") {
        "tar.bz2".to_string()
    } else if path.ends_with(".tar") {
        "tar".to_string()
    } else if path.ends_with(".zip") {
        "zip".to_string()
    } else {
        "tar.gz".to_string()
    }
}

/// Decode the `Base64.encode64` output Rails uses for binary send-data fields
/// (standard alphabet, line-wrapped at 60 columns).
fn decode_binary(encoded: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(cleaned.as_bytes())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(cleaned.as_bytes()))
}

fn archive_response(data: Vec<u8>, format: String, prefix: Option<String>) -> Response {
    let filename = match prefix {
        Some(prefix) if !prefix.is_empty() => format!("{}.{}", prefix, format),
        _ => format!("archive.{}", format),
    };
    let mut response_headers = HeaderMap::new();
    response_headers.insert("content-type", archive_content_type(&format).parse().unwrap());
    response_headers.insert(
        "content-disposition",
        format!("attachment; filename=\"{}\"", filename).parse().unwrap(),
    );
    response_headers.insert("content-transfer-encoding", "binary".parse().unwrap());
    (StatusCode::OK, response_headers, data).into_response()
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
    if let Some(server) = &params.gitaly_server {
        let repository = match blob_repository(&params) {
            Some(repo) => repo,
            None => {
                tracing::error!("git-blob missing repository in GitalyRepository/GetBlobRequest");
                return Err(StatusCode::BAD_REQUEST);
            }
        };
        let (mut client, repo) = connect_gitaly(server, &repository).await?;
        let oid = blob_oid(&params);
        let limit = blob_limit(&params);
        let data = client.get_blob(&repo, oid, limit).await.map_err(|e| {
            tracing::error!("Gitaly get_blob failed: {}", e);
            StatusCode::BAD_GATEWAY
        })?;
        let mut response_headers = HeaderMap::new();
        response_headers
            .insert("content-type", sniff_content_type(&data).parse().unwrap());
        response_headers.insert("content-length", data.len().to_string().parse().unwrap());
        return Ok((StatusCode::OK, response_headers, data).into_response());
    }

    // Fallback: local filesystem
    let repo_path = resolve_local_repo_path(&params.repo_path, &None)?;
    let repo = gix::open(&repo_path).map_err(|e| {
        tracing::error!("Failed to open git repo for blob: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let blob_id_str = blob_oid(&params);
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
    response_headers.insert("content-type", sniff_content_type(&data).parse().unwrap());
    response_headers.insert("content-length", data.len().to_string().parse().unwrap());
    Ok((StatusCode::OK, response_headers, data).into_response())
}

fn blob_oid(params: &GitBlobParams) -> &str {
    json_str(&params.get_blob_request, "oid")
        .or(json_str(&params.get_blob_request, "Oid"))
        .or(params.blob_id.as_deref())
        .unwrap_or("")
}

fn blob_limit(params: &GitBlobParams) -> i64 {
    json_i64(&params.get_blob_request, "limit")
        .or_else(|| json_i64(&params.get_blob_request, "Limit"))
        .unwrap_or(-1)
}

fn blob_repository(params: &GitBlobParams) -> Option<GitalyRepositoryParams> {
    if let Some(repo) = &params.gitaly_repository {
        return Some(GitalyRepositoryParams {
            storage_name: repo.storage_name.clone(),
            relative_path: repo.relative_path.clone(),
            gl_project_path: repo.gl_project_path.clone(),
            gl_repository: repo.gl_repository.clone(),
        });
    }
    let repo = params.get_blob_request.as_ref()?.get("repository")
        .or_else(|| params.get_blob_request.as_ref()?.get("Repository"))?;
    Some(GitalyRepositoryParams {
        storage_name: json_value_str(repo, "storageName")
            .or_else(|| json_value_str(repo, "storage_name"))
            .or_else(|| json_value_str(repo, "StorageName"))
            .map(ToString::to_string),
        relative_path: json_value_str(repo, "relativePath")
            .or_else(|| json_value_str(repo, "relative_path"))
            .or_else(|| json_value_str(repo, "RelativePath"))
            .map(ToString::to_string),
        gl_project_path: json_value_str(repo, "glProjectPath")
            .or_else(|| json_value_str(repo, "gl_project_path"))
            .map(ToString::to_string),
        gl_repository: json_value_str(repo, "glRepository")
            .or_else(|| json_value_str(repo, "gl_repository"))
            .map(ToString::to_string),
    })
}

fn json_str<'a>(value: &'a Option<serde_json::Value>, key: &str) -> Option<&'a str> {
    value.as_ref()?.get(key)?.as_str()
}

fn json_i64(value: &Option<serde_json::Value>, key: &str) -> Option<i64> {
    value.as_ref()?.get(key)?.as_i64()
}

fn json_value_str<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
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
    fn test_parse_official_get_blob_request() {
        let json = r#"{
            "GitalyServer": {"Address": "unix:/var/opt/gitlab/gitaly/gitaly.socket", "Token": "secret"},
            "GetBlobRequest": {
                "repository": {"storage_name": "default", "relative_path": "@hashed/ab/cd/abcd.git"},
                "oid": "deadbeef",
                "limit": -1
            }
        }"#;
        let params: GitBlobParams = serde_json::from_str(json).unwrap();
        assert!(params.gitaly_server.is_some());
        assert_eq!(blob_oid(&params), "deadbeef");
        assert_eq!(blob_limit(&params), -1);
        let repo = blob_repository(&params).unwrap();
        assert_eq!(repo.storage_name.as_deref(), Some("default"));
        assert_eq!(repo.relative_path.as_deref(), Some("@hashed/ab/cd/abcd.git"));
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
