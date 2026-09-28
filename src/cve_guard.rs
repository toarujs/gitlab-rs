//! Defensive reconstruction of GitLab 19.3.2/19.3.3 Workhorse-side checks.
//!
//! Covers the commits/files write surface (CVE-2026-85706 class) and strips
//! client-injected send-data headers. CI regex RCE (CVE-2026-89078 / 93577)
//! lives in Rails/Onigmo and still needs CE 19.3.3 for the native engine.

use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode},
};
use http_body_util::BodyExt;

const CLIENT_FORBIDDEN_HEADERS: &[&str] = &[
    "gitlab-workhorse-send-data",
    "gitlab-workhorse-detect-content-type",
    "gitlab-workhorse-api-request",
    "gitlab-workhorse-proxy-start",
    "gitlab-workhorse",
    "x-sendfile",
    "x-sendfile-type",
];

pub fn is_client_forbidden_workhorse_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    CLIENT_FORBIDDEN_HEADERS.contains(&lower.as_str())
}

const WORKHORSE_PATH_KEYS: &[&str] = &[
    "file.path",
    "file[path]",
    "metadata.path",
    "metadata[path]",
];

pub fn has_api_credential(headers: &HeaderMap) -> bool {
    const KEYS: &[&str] = &[
        "private-token",
        "job-token",
        "deploy-token",
        "authorization",
    ];
    for key in KEYS {
        if headers
            .get(*key)
            .and_then(|v| v.to_str().ok())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            return true;
        }
    }
    if let Some(cookie) = headers.get("cookie").and_then(|v| v.to_str().ok()) {
        let lower = cookie.to_ascii_lowercase();
        if lower.contains("_gitlab_session") || lower.contains("remember_user_token") {
            return true;
        }
    }
    false
}

pub fn normalize_api_path(path: &str) -> String {
    let mut current = path.to_string();
    for _ in 0..4 {
        let decoded = percent_decode(&current);
        if decoded == current {
            break;
        }
        current = decoded;
    }
    let mut s = current.trim_end_matches('/').to_string();
    if s.is_empty() {
        return "/".to_string();
    }
    if let Some((head, last)) = s.rsplit_once('/') {
        let last_lower = last.to_ascii_lowercase();
        if let Some(stem) = last_lower.strip_suffix(".json") {
            s = if head.is_empty() {
                format!("/{stem}")
            } else {
                format!("{head}/{stem}")
            };
        }
    }
    s
}

pub fn is_repository_commits_or_files_path(path: &str) -> bool {
    let normalized = normalize_api_path(path).to_ascii_lowercase();
    if !normalized.starts_with("/api/v4/projects/") {
        return false;
    }
    normalized.contains("/repository/commits") || normalized.contains("/repository/files")
}

pub fn is_mutating_repository_path(method: &Method, path: &str) -> bool {
    matches!(*method, Method::POST | Method::PUT | Method::PATCH | Method::DELETE)
        && is_repository_commits_or_files_path(path)
}

pub fn body_has_client_upload_path_field(bytes: &[u8], content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("json") {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
            return json_has_workhorse_upload_path_field(&value);
        }
        return raw_has_workhorse_path_key(bytes);
    }
    if ct.contains("application/x-www-form-urlencoded") {
        return form_has_workhorse_upload_path_field(bytes);
    }
    if ct.contains("multipart/") {
        return multipart_has_workhorse_path_field(bytes);
    }
    json_has_workhorse_upload_path_field(
        &serde_json::from_slice::<serde_json::Value>(bytes).unwrap_or(serde_json::Value::Null),
    ) || form_has_workhorse_upload_path_field(bytes)
        || raw_has_workhorse_path_key(bytes)
}

pub async fn guard_repository_write(
    method: &Method,
    path: &str,
    headers: HeaderMap,
    body: Body,
) -> Result<(HeaderMap, Body), StatusCode> {
    if !is_mutating_repository_path(method, path) {
        return Ok((headers, body));
    }
    if !has_api_credential(&headers) {
        tracing::warn!(path, "Rejected unauthenticated repository write");
        return Err(StatusCode::UNAUTHORIZED);
    }
    let bytes = body
        .collect()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .to_bytes();
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if body_has_client_upload_path_field(&bytes, content_type) {
        tracing::warn!(path, "Rejected client workhorse upload metadata on repository write");
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((headers, Body::from(bytes)))
}

fn json_has_workhorse_upload_path_field(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                let k = key.to_ascii_lowercase();
                if WORKHORSE_PATH_KEYS.contains(&k.as_str()) {
                    return true;
                }
                if k == "file" || k == "metadata" {
                    if val
                        .as_object()
                        .map(|inner| inner.contains_key("path"))
                        .unwrap_or(false)
                    {
                        return true;
                    }
                }
                if json_has_workhorse_upload_path_field(val) {
                    return true;
                }
            }
            false
        }
        serde_json::Value::Array(arr) => arr.iter().any(json_has_workhorse_upload_path_field),
        _ => false,
    }
}

fn form_has_workhorse_upload_path_field(bytes: &[u8]) -> bool {
    let raw = String::from_utf8_lossy(bytes);
    for pair in raw.split('&') {
        let key = pair.split('=').next().unwrap_or("");
        let decoded = percent_decode(&key.replace('+', " ")).to_ascii_lowercase();
        if WORKHORSE_PATH_KEYS.contains(&decoded.as_str()) {
            return true;
        }
    }
    false
}

fn multipart_has_workhorse_path_field(bytes: &[u8]) -> bool {
    let lower = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    lower.contains("name=\"file.path\"")
        || lower.contains("name=\"file[path]\"")
        || lower.contains("name=\"metadata.path\"")
        || lower.contains("name=\"metadata[path]\"")
}

fn raw_has_workhorse_path_key(bytes: &[u8]) -> bool {
    let lower = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    lower.contains("\"file.path\"")
        || lower.contains("\"metadata.path\"")
        || lower.contains("file.path=")
        || lower.contains("metadata.path=")
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_json_suffix_slash_and_encoding() {
        assert_eq!(
            normalize_api_path("/api/v4/projects/1/repository/commits.json/"),
            "/api/v4/projects/1/repository/commits"
        );
        assert_eq!(
            normalize_api_path("/api/v4/projects/1/repository/%63ommits"),
            "/api/v4/projects/1/repository/commits"
        );
        assert_eq!(
            normalize_api_path("/api/v4/projects/1/repository/%2563ommits.json"),
            "/api/v4/projects/1/repository/commits"
        );
    }

    #[test]
    fn matches_commits_and_files_paths() {
        assert!(is_repository_commits_or_files_path(
            "/api/v4/projects/1/repository/commits.json/"
        ));
        assert!(is_repository_commits_or_files_path(
            "/api/v4/projects/group%2Fproj/repository/files/README.md"
        ));
        assert!(!is_repository_commits_or_files_path("/api/v4/projects/1"));
        assert!(!is_repository_commits_or_files_path("/api/v4/user"));
    }

    #[test]
    fn mutating_only_on_write_methods() {
        let path = "/api/v4/projects/1/repository/commits";
        assert!(is_mutating_repository_path(&Method::POST, path));
        assert!(!is_mutating_repository_path(&Method::GET, path));
    }

    #[test]
    fn detects_json_file_path_metadata() {
        let body = br#"{"branch":"main","file.path":"/etc/passwd"}"#;
        assert!(body_has_client_upload_path_field(body, "application/json"));
        let nested = br#"{"file":{"path":"/tmp/x"},"branch":"main"}"#;
        assert!(body_has_client_upload_path_field(nested, "application/json"));
        let legit = br#"{"branch":"main","commit_message":"x","actions":[{"action":"create","file_path":"README.md","content":"hi"}]}"#;
        assert!(!body_has_client_upload_path_field(legit, "application/json"));
    }

    #[test]
    fn detects_form_file_path_metadata() {
        let body = b"branch=main&file.path=%2Fetc%2Fpasswd";
        assert!(body_has_client_upload_path_field(
            body,
            "application/x-www-form-urlencoded"
        ));
    }

    #[test]
    fn credential_headers() {
        let mut headers = HeaderMap::new();
        assert!(!has_api_credential(&headers));
        headers.insert("private-token", "glpat-test".parse().unwrap());
        assert!(has_api_credential(&headers));
    }

    #[test]
    fn strips_client_send_data_header_name() {
        assert!(is_client_forbidden_workhorse_header(
            "Gitlab-Workhorse-Send-Data"
        ));
        assert!(is_client_forbidden_workhorse_header("x-sendfile"));
        assert!(!is_client_forbidden_workhorse_header("authorization"));
    }

    #[tokio::test]
    async fn guard_rejects_unauthenticated_post() {
        let headers = HeaderMap::new();
        let body = Body::from(r#"{"branch":"main"}"#);
        let err = guard_repository_write(
            &Method::POST,
            "/api/v4/projects/1/repository/commits",
            headers,
            body,
        )
        .await
        .unwrap_err();
        assert_eq!(err, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn guard_rejects_client_file_path() {
        let mut headers = HeaderMap::new();
        headers.insert("private-token", "glpat-test".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        let body = Body::from(r#"{"file.path":"/etc/passwd","branch":"main"}"#);
        let err = guard_repository_write(
            &Method::POST,
            "/api/v4/projects/1/repository/commits.json",
            headers,
            body,
        )
        .await
        .unwrap_err();
        assert_eq!(err, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn guard_allows_authenticated_commit_json() {
        let mut headers = HeaderMap::new();
        headers.insert("private-token", "glpat-test".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        let body = Body::from(
            r#"{"branch":"main","commit_message":"x","actions":[{"action":"create","file_path":"a.txt","content":"b"}]}"#,
        );
        let result = guard_repository_write(
            &Method::POST,
            "/api/v4/projects/1/repository/commits",
            headers,
            body,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn guard_skips_get() {
        let headers = HeaderMap::new();
        let body = Body::from("");
        let result = guard_repository_write(
            &Method::GET,
            "/api/v4/projects/1/repository/commits",
            headers,
            body,
        )
        .await;
        assert!(result.is_ok());
    }
}
