//! Workhorse "request body" upload acceleration.
//!
//! GitLab serves several package-manager uploads (Maven, npm, Conan, generic,
//! PyPI-like, Debian, RPM, RubyGems, Terraform modules, ...) by having
//! Workhorse buffer the raw request body to a temp file and then hand Rails a
//! form-encoded body that references it. Rails never sees the raw payload; it
//! only validates the signed `file.gitlab-workhorse-upload` token.
//!
//! Without this step the raw payload reaches Grape's formatter directly and is
//! rejected with `415 Unsupported Media Type`.

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use sha1::{Digest, Sha1};
use sha2::{Sha256, Sha512};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::proxy;
use crate::state::AppState;

const WORKHORSE_INTERNAL_CONTENT_TYPE: &str = "application/vnd.gitlab-workhorse+json";
const REWRITTEN_FIELDS_HEADER: &str = "gitlab-workhorse-multipart-fields";
const TMP_FILE_PREFIX: &str = "gitlab-workhorse-upload";
const DEFAULT_MAX_SIZE: u64 = 5 * 1024 * 1024 * 1024; // 5GB

/// Accelerate a request-body upload. Returns the proxied response on success or
/// the pre-authorization failure response when Rails rejects the request.
pub async fn accelerate_request_body(
    state: AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let authorize_uri = match authorize_uri(&uri) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid upload path").into_response(),
    };

    let authorize_headers = strip_body_headers(&headers);
    let (status, resp_headers, resp_body) =
        match proxy::send_raw_upstream(&state, method.clone(), authorize_uri, authorize_headers, Body::empty()).await
        {
            Ok(parts) => parts,
            Err(status) => return (status, "").into_response(),
        };

    let content_type = resp_headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if status != StatusCode::OK || !content_type.starts_with(WORKHORSE_INTERNAL_CONTENT_TYPE) {
        // Authorization failed (401/403/404/...): pass Rails' answer back to the
        // client unchanged, exactly as the Go workhorse does.
        return passthrough(status, &resp_headers, resp_body);
    }

    let auth: serde_json::Value = match serde_json::from_slice(&resp_body) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("upload accelerate: invalid authorize JSON: {}", e);
            return (StatusCode::BAD_GATEWAY, "invalid authorization response").into_response();
        }
    };

    let tmp_dir = match auth.get("TempPath").and_then(|v| v.as_str()) {
        Some(dir) => PathBuf::from(dir),
        None => {
            tracing::error!("upload accelerate: authorization response has no TempPath (object storage uploads are not supported)");
            return (StatusCode::INTERNAL_SERVER_ERROR, "unsupported upload destination").into_response();
        }
    };
    let maximum_size = auth
        .get("MaximumSize")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_SIZE);
    let hash_functions = auth
        .get("UploadHashFunctions")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>());

    let stored = match store_body(&tmp_dir, maximum_size, hash_functions, body).await {
        Ok(s) => s,
        Err(status) => return (status, "upload failed").into_response(),
    };

    let original_content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let upload = serde_json::json!({
        "name": "upload",
        "path": stored.path,
        "remote_url": "",
        "remote_id": "",
        "size": stored.size,
        "upload_duration": stored.duration,
        "md5": stored.md5,
        "sha1": stored.sha1,
        "sha256": stored.sha256,
        "sha512": stored.sha512,
    });
    let upload_jwt = match state.secret.sign_upload_jwt(&upload) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("upload accelerate: sign upload jwt: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "upload accelerate failed").into_response();
        }
    };
    let rewritten_jwt = match state.secret.sign_multipart_fields_jwt(&["file".to_string()]) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("upload accelerate: sign rewritten fields jwt: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "upload accelerate failed").into_response();
        }
    };

    // `form_urlencoded::Serializer` is not `Send`, so it must not live across
    // the proxy await below; build the encoded body inside its own scope.
    let form = {
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        ser.append_pair("file.name", "upload");
        ser.append_pair("file.path", &stored.path);
        ser.append_pair("file.remote_url", "");
        ser.append_pair("file.remote_id", "");
        ser.append_pair("file.size", &stored.size.to_string());
        ser.append_pair("file.upload_duration", &stored.duration);
        ser.append_pair("file.md5", &stored.md5);
        ser.append_pair("file.sha1", &stored.sha1);
        ser.append_pair("file.sha256", &stored.sha256);
        ser.append_pair("file.sha512", &stored.sha512);
        ser.append_pair("file.gitlab-workhorse-upload", &upload_jwt);
        ser.append_pair("Content-Type", &original_content_type);
        ser.finish()
    };

    let mut out = headers;
    out.remove(header::CONTENT_TYPE);
    out.remove(header::CONTENT_LENGTH);
    out.remove(header::TRANSFER_ENCODING);
    out.remove(header::CONTENT_ENCODING);
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    if let Ok(value) = form.len().to_string().parse() {
        out.insert(header::CONTENT_LENGTH, value);
    }
    match rewritten_jwt.parse() {
        Ok(value) => {
            out.insert(REWRITTEN_FIELDS_HEADER, value);
        }
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "invalid multipart jwt").into_response(),
    }

    let mut req = Request::new(Body::from(form));
    *req.method_mut() = method;
    *req.uri_mut() = uri;
    *req.headers_mut() = out;

    // `Box::pin` breaks the async recursion: the LFS path dispatches here from
    // inside `proxy_handler` itself, and the rewritten request is then proxied
    // back through `proxy_handler`.
    match Box::pin(proxy::proxy_handler(axum::extract::State(state), req)).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

/// Build the `<path>/authorize` URI, preserving the query string.
fn authorize_uri(uri: &Uri) -> Result<Uri, axum::http::uri::InvalidUri> {
    let mut path = String::with_capacity(uri.path().len() + 10);
    path.push_str(uri.path());
    path.push_str("/authorize");
    if let Some(query) = uri.query() {
        path.push('?');
        path.push_str(query);
    }
    path.parse()
}

/// Clone headers for the body-less pre-authorization subrequest.
fn strip_body_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = headers.clone();
    for key in [
        header::CONTENT_TYPE,
        header::CONTENT_ENCODING,
        header::CONTENT_LENGTH,
        header::CONTENT_DISPOSITION,
        header::ACCEPT_ENCODING,
        header::TRANSFER_ENCODING,
        header::CONNECTION,
        header::TE,
        header::TRAILER,
        header::UPGRADE,
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
    ] {
        out.remove(&key);
    }
    out.remove("keep-alive");
    out
}

struct StoredBody {
    path: String,
    size: u64,
    duration: String,
    md5: String,
    sha1: String,
    sha256: String,
    sha512: String,
}

async fn store_body(
    tmp_dir: &Path,
    maximum_size: u64,
    hash_functions: Option<Vec<String>>,
    body: Body,
) -> Result<StoredBody, StatusCode> {
    if let Err(e) = tokio::fs::create_dir_all(tmp_dir).await {
        tracing::error!("upload accelerate: create temp dir {}: {}", tmp_dir.display(), e);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    chown_git_tree(tmp_dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(tmp_dir, std::fs::Permissions::from_mode(0o700)).await;
    }

    let path = tmp_dir.join(format!("{}{}", TMP_FILE_PREFIX, Uuid::new_v4().simple()));
    let mut file = match tokio::fs::File::create(&path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("upload accelerate: create temp file {}: {}", path.display(), e);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let enabled = |name: &str| match &hash_functions {
        Some(list) => list.iter().any(|f| f == name),
        None => true,
    };
    let mut md5_ctx = enabled("md5").then(md5::Context::new);
    let mut sha1 = enabled("sha1").then(Sha1::new);
    let mut sha256 = enabled("sha256").then(Sha256::new);
    let mut sha512 = enabled("sha512").then(Sha512::new);

    let started = std::time::Instant::now();
    let mut size: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("upload accelerate: read body: {}", e);
                let _ = tokio::fs::remove_file(&path).await;
                return Err(StatusCode::BAD_REQUEST);
            }
        };
        size = size.saturating_add(chunk.len() as u64);
        if size > maximum_size {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        if let Some(ctx) = md5_ctx.as_mut() {
            ctx.consume(&chunk);
        }
        if let Some(h) = sha1.as_mut() {
            h.update(&chunk);
        }
        if let Some(h) = sha256.as_mut() {
            h.update(&chunk);
        }
        if let Some(h) = sha512.as_mut() {
            h.update(&chunk);
        }
        if let Err(e) = file.write_all(&chunk).await {
            tracing::error!("upload accelerate: write temp file: {}", e);
            let _ = tokio::fs::remove_file(&path).await;
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    if let Err(e) = file.flush().await {
        tracing::error!("upload accelerate: flush temp file: {}", e);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await;
    }
    chown_git_tree(&path);

    let duration = format!("{}", started.elapsed().as_secs_f64());
    Ok(StoredBody {
        path: path.to_string_lossy().into_owned(),
        size,
        duration,
        md5: md5_ctx.map(|c| hex::encode(c.compute().0)).unwrap_or_default(),
        sha1: sha1.map(|h| hex::encode(h.finalize())).unwrap_or_default(),
        sha256: sha256.map(|h| hex::encode(h.finalize())).unwrap_or_default(),
        sha512: sha512.map(|h| hex::encode(h.finalize())).unwrap_or_default(),
    })
}

fn passthrough(status: StatusCode, headers: &HeaderMap, body: bytes::Bytes) -> Response {
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

/// Chown `path` to the `git` user, walking up through any ancestor directories
/// that are still owned by another user.
///
/// Rails runs as `git` and expects to create sibling directories (for example
/// `<packages>/tmp/work`) inside the upload temp directory. Because the Rust
/// workhorse runs as root, every directory it creates while materialising the
/// temp path must be handed back to `git`, not just the leaf.
fn chown_git_tree(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let Some((uid, gid)) = git_uid_gid() else {
            return;
        };
        let is_git_owned = |p: &Path| {
            std::fs::metadata(p)
                .map(|md| md.uid() == uid && md.gid() == gid)
                .ok()
        };

        let mut owned = Vec::new();
        let mut current = Some(path.to_path_buf());
        while let Some(p) = current {
            // Never touch the filesystem root.
            let parent = p
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(|parent| parent.to_path_buf());
            let Some(parent) = parent else {
                break;
            };

            // Stop once we reach a directory that, together with its parent, is
            // already owned by git: everything above it is Rails' responsibility.
            if is_git_owned(&p) == Some(true) && is_git_owned(&parent) == Some(true) {
                break;
            }

            owned.push(p);
            current = Some(parent.to_path_buf());
        }

        for entry in owned.iter().rev() {
            let _ = std::os::unix::fs::chown(entry, Some(uid), Some(gid));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_uri_appends_suffix_and_keeps_query() {
        let uri: Uri = "/api/v4/projects/1/packages/generic/pkg/1.0/f.bin?status=default"
            .parse()
            .unwrap();
        let auth = authorize_uri(&uri).unwrap();
        assert_eq!(
            auth.path(),
            "/api/v4/projects/1/packages/generic/pkg/1.0/f.bin/authorize"
        );
        assert_eq!(auth.query(), Some("status=default"));
    }

    #[test]
    fn strip_body_headers_drops_payload_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("10"));
        headers.insert("private-token", HeaderValue::from_static("secret"));
        let out = strip_body_headers(&headers);
        assert!(out.get(header::CONTENT_TYPE).is_none());
        assert!(out.get(header::CONTENT_LENGTH).is_none());
        assert_eq!(out.get("private-token").unwrap(), "secret");
    }
}
