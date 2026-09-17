use axum::{
    body::Body,
    extract::{Multipart, State},
    http::{header, HeaderMap, Method, Request, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use sha1::{Digest, Sha1};
use sha2::{Sha256, Sha512};
use std::path::Path;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::proxy;
use crate::secret::Secret;
use crate::state::AppState;

const MAX_FILES: usize = 10;
const ARTIFACTS_TMP: &str = "/var/opt/gitlab/gitlab-rails/shared/artifacts/tmp/uploads";
/// Upload temp directory backing `Packages::PackageFileUploader.workhorse_upload_path`,
/// which Rails lists in `Gitlab::Middleware::Multipart#allowed_paths`.
pub(crate) const PACKAGES_TMP: &str = "/var/opt/gitlab/gitlab-rails/shared/packages/tmp/uploads";

pub async fn accelerate_multipart_request(
    state: AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    multipart: Multipart,
    max_size: u64,
) -> Response {
    accelerate_multipart_request_to(state, method, uri, headers, multipart, max_size, Path::new(ARTIFACTS_TMP)).await
}

/// Multipart upload acceleration for package repositories (NuGet, PyPI, Helm).
///
/// Upstream Workhorse routes these through its *mime multipart* uploader: file
/// parts are saved to a temp path and replaced with `<field>.<attr>` form
/// fields plus a signed `<field>.gitlab-workhorse-upload` JWT. Ruby only accepts
/// the upload once that rewrite happened, so a plain proxy answers 400.
pub async fn accelerate_package_multipart_request(
    state: AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    multipart: Multipart,
    max_size: u64,
) -> Response {
    accelerate_multipart_request_to(state, method, uri, headers, multipart, max_size, Path::new(PACKAGES_TMP)).await
}

async fn accelerate_multipart_request_to(
    state: AppState,
    method: Method,
    uri: Uri,
    mut headers: HeaderMap,
    multipart: Multipart,
    max_size: u64,
    tmp_dir: &Path,
) -> Response {
    let rewritten = match rewrite_multipart(&state.secret, tmp_dir, max_size, multipart).await {
        Ok(body) => body,
        Err(status) => {
            tracing::error!(status = %status, path = %uri.path(), "Artifact upload accelerate failed");
            return (status, "upload accelerate failed").into_response();
        }
    };

    headers.remove(header::CONTENT_TYPE);
    headers.remove(header::CONTENT_LENGTH);
    headers.remove(header::TRANSFER_ENCODING);
    if !rewritten.rewritten_fields.is_empty() {
        match state.secret.sign_multipart_fields_jwt(&rewritten.rewritten_fields) {
            Ok(token) => match token.parse() {
                Ok(value) => {
                    headers.insert("gitlab-workhorse-multipart-fields", value);
                }
                Err(_) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "invalid multipart jwt").into_response();
                }
            },
            Err(e) => {
                tracing::error!("Failed to sign multipart fields JWT: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "upload accelerate failed").into_response();
            }
        }
    }
    let content_type = format!("multipart/form-data; boundary={}", rewritten.boundary);
    match content_type.parse() {
        Ok(value) => {
            headers.insert(header::CONTENT_TYPE, value);
        }
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "invalid content type").into_response(),
    }
    match rewritten.body.len().to_string().parse() {
        Ok(value) => {
            headers.insert(header::CONTENT_LENGTH, value);
        }
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "invalid content length").into_response(),
    }

    let mut req = Request::new(Body::from(rewritten.body));
    *req.method_mut() = method;
    *req.uri_mut() = uri;
    *req.headers_mut() = headers;

    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

struct RewrittenForm {
    boundary: String,
    body: Vec<u8>,
    rewritten_fields: Vec<String>,
}

async fn rewrite_multipart(
    secret: &Secret,
    tmp_dir: &Path,
    max_size: u64,
    mut multipart: Multipart,
) -> Result<RewrittenForm, StatusCode> {
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut file_count = 0usize;
    let mut rewritten_fields: Vec<String> = Vec::new();

    while let Some(mut field) = multipart.next_field().await.map_err(|e| {
        tracing::error!("Failed to read multipart field: {}", e);
        StatusCode::BAD_REQUEST
    })? {
        let name = field.name().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        if is_reserved_field(&name) {
            tracing::warn!("Rejected injected upload field {}", name);
            return Err(StatusCode::BAD_REQUEST);
        }

        let filename = field.file_name().map(|s| s.to_string()).filter(|s| !s.is_empty());
        if let Some(filename) = filename {
            file_count += 1;
            if file_count > MAX_FILES {
                return Err(StatusCode::BAD_REQUEST);
            }
            let saved = persist_file_field(secret, tmp_dir, max_size, &name, &filename, &mut field).await?;
            rewritten_fields.push(name.clone());
            fields.extend(saved);
        } else {
            let text = field.text().await.map_err(|e| {
                tracing::error!("Failed to read multipart text field {}: {}", name, e);
                StatusCode::BAD_REQUEST
            })?;
            fields.push((name, text));
        }
    }

    Ok(encode_multipart_fields(&fields, rewritten_fields))
}

fn is_reserved_field(name: &str) -> bool {
    let Some((_, suffix)) = name.rsplit_once('.') else {
        return false;
    };
    matches!(
        suffix,
        "path"
            | "name"
            | "size"
            | "md5"
            | "sha1"
            | "sha256"
            | "sha512"
            | "remote_url"
            | "remote_id"
            | "gitlab-workhorse-upload"
    )
}

fn sanitize_filename(filename: &str) -> Result<String, StatusCode> {
    let base = Path::new(filename)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if base.is_empty() || base == "." || base == ".." || base.contains('/') || base.contains('\\') {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(base.to_string())
}

async fn persist_file_field(
    secret: &Secret,
    tmp_dir: &Path,
    max_size: u64,
    field_name: &str,
    filename: &str,
    field: &mut axum::extract::multipart::Field<'_>,
) -> Result<Vec<(String, String)>, StatusCode> {
    let filename = sanitize_filename(filename)?;
    tokio::fs::create_dir_all(tmp_dir).await.map_err(|e| {
        tracing::error!("Failed to create workhorse temp dir {}: {}", tmp_dir.display(), e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    chown_git(tmp_dir);

    let tmp_path = tmp_dir.join(Uuid::new_v4().to_string());
    let mut file = tokio::fs::File::create(&tmp_path).await.map_err(|e| {
        tracing::error!("Failed to create temp upload {}: {}", tmp_path.display(), e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut md5_ctx = md5::Context::new();
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    let mut total_size = 0u64;

    while let Some(chunk) = field.chunk().await.map_err(|e| {
        tracing::error!("Failed to read upload chunk: {}", e);
        StatusCode::BAD_REQUEST
    })? {
        let next = total_size.saturating_add(chunk.len() as u64);
        if next > max_size {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        md5_ctx.consume(&chunk);
        sha1.update(&chunk);
        sha256.update(&chunk);
        sha512.update(&chunk);
        file.write_all(&chunk).await.map_err(|e| {
            tracing::error!("Failed to write upload chunk: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        total_size = next;
    }

    file.flush().await.map_err(|e| {
        tracing::error!("Failed to flush upload file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o644)).await;
        chown_git(&tmp_path);
    }

    let path_str = tmp_path.to_string_lossy().into_owned();
    let md5_hex = hex::encode(md5_ctx.compute().0);
    let sha1_hex = hex::encode(sha1.finalize());
    let sha256_hex = hex::encode(sha256.finalize());
    let sha512_hex = hex::encode(sha512.finalize());

    let upload = serde_json::json!({
        "name": filename,
        "path": path_str,
        "remote_url": "",
        "remote_id": "",
        "size": total_size,
        "md5": md5_hex,
        "sha1": sha1_hex,
        "sha256": sha256_hex,
        "sha512": sha512_hex,
    });
    let jwt = secret.sign_upload_jwt(&upload).map_err(|e| {
        tracing::error!("Failed to sign upload JWT: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let key = |suffix: &str| format!("{}.{}", field_name, suffix);
    Ok(vec![
        (key("name"), filename),
        (key("path"), path_str),
        (key("remote_url"), String::new()),
        (key("remote_id"), String::new()),
        (key("size"), total_size.to_string()),
        (key("md5"), md5_hex),
        (key("sha1"), sha1_hex),
        (key("sha256"), sha256_hex),
        (key("sha512"), sha512_hex),
        (key("gitlab-workhorse-upload"), jwt),
    ])
}

fn encode_multipart_fields(fields: &[(String, String)], rewritten_fields: Vec<String>) -> RewrittenForm {
    let boundary = format!("----GitLabWorkhorseRs{}", Uuid::new_v4().simple());
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(b"--");
        body.extend_from_slice(boundary.as_bytes());
        body.extend_from_slice(b"\r\nContent-Disposition: form-data; name=\"");
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(b"\"\r\n\r\n");
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--");
    body.extend_from_slice(boundary.as_bytes());
    body.extend_from_slice(b"--\r\n");
    RewrittenForm {
        boundary,
        body,
        rewritten_fields,
    }
}

fn git_uid_gid() -> Option<(u32, u32)> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let mut parts = line.split(':');
        if parts.next()? != "git" {
            continue;
        }
        let _password = parts.next()?;
        let uid = parts.next()?.parse().ok()?;
        let gid = parts.next()?.parse().ok()?;
        return Some((uid, gid));
    }
    None
}

fn chown_git(path: &Path) {
    #[cfg(unix)]
    if let Some((uid, gid)) = git_uid_gid() {
        let _ = std::os::unix::fs::chown(path, Some(uid), Some(gid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_fields_match_workhorse_suffixes() {
        assert!(is_reserved_field("file.path"));
        assert!(is_reserved_field("file.gitlab-workhorse-upload"));
        assert!(!is_reserved_field("file"));
        assert!(!is_reserved_field("token"));
    }

    #[test]
    fn sanitize_filename_strips_paths() {
        assert_eq!(sanitize_filename("artifacts.zip").unwrap(), "artifacts.zip");
        assert_eq!(sanitize_filename("/tmp/artifacts.zip").unwrap(), "artifacts.zip");
        assert!(sanitize_filename("..").is_err());
    }

    #[test]
    fn encode_multipart_contains_file_path() {
        let form = encode_multipart_fields(&[
            ("file.path".into(), "/tmp/a".into()),
            ("file.size".into(), "4".into()),
        ], vec!["file".into()]);
        let body = String::from_utf8(form.body).unwrap();
        assert!(body.contains("name=\"file.path\""));
        assert!(body.contains("/tmp/a"));
        assert!(body.contains(&form.boundary));
        assert_eq!(form.rewritten_fields, vec!["file".to_string()]);
    }
}
