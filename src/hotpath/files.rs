//! Accelerate `GET/HEAD /api/v4/projects/:id/repository/files/*`.

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::time::Instant;

use super::acl;
use super::session;
use super::{FILES_PATH_TEMPLATE, HotPathState};
use crate::gitaly::{GitalyClient, GitalyServer, RepoInfo};
use crate::redis::RedisClient;
use crate::state::AppState;

const NOT_FOUND_BODY: &str = "{\"message\":\"404 File Not Found\"}";

#[derive(Debug)]
pub enum AccelResult {
    Hit(Response),
    Fallback { reason: String, error: bool },
}

#[derive(Debug, Serialize, Clone)]
pub struct RepositoryFileBody {
    pub file_name: String,
    pub file_path: String,
    pub size: i64,
    pub encoding: String,
    pub content: String,
    pub content_sha256: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub blob_id: String,
    pub commit_id: String,
    pub last_commit_id: String,
    pub execute_filemode: bool,
}

#[derive(Debug, Clone)]
pub struct FilesRequest {
    pub project_id: String,
    pub file_path: String,
    pub git_ref: String,
    pub head_only: bool,
}

pub fn blob_exceeds_limit(size: i64, max_bytes: u64) -> bool {
    size < 0 || (size as u64) > max_bytes
}

pub fn parse_files_request(method: &Method, path: &str, query: Option<&str>) -> Result<FilesRequest, String> {
    if method != Method::GET && method != Method::HEAD {
        return Err("method not accelerated".to_string());
    }
    let path = path.split('?').next().unwrap_or(path);
    let prefix = "/api/v4/projects/";
    let rest = path
        .strip_prefix(prefix)
        .ok_or_else(|| "not a files path".to_string())?;
    let (project_id, after_project) = rest
        .split_once("/repository/files/")
        .ok_or_else(|| "not a files path".to_string())?;
    if project_id.is_empty() {
        return Err("missing project id".to_string());
    }
    let file_path = percent_decode(after_project);
    if file_path.is_empty() {
        return Err("missing file path".to_string());
    }
    if is_non_file_suffix(&file_path) {
        return Err("raw/blame/authorize not accelerated".to_string());
    }
    let git_ref = query_param(query, "ref").unwrap_or_default();
    if git_ref.is_empty() {
        return Err("missing ref".to_string());
    }
    Ok(FilesRequest {
        project_id: percent_decode(project_id),
        file_path,
        git_ref,
        head_only: method == Method::HEAD,
    })
}

fn is_non_file_suffix(file_path: &str) -> bool {
    let trimmed = file_path.trim_end_matches('/');
    trimmed.ends_with("/raw") || trimmed.ends_with("/blame") || trimmed.ends_with("/authorize")
}

pub fn accelerate<'a>(
    state: &'a AppState,
    req: &Request<Body>,
) -> impl std::future::Future<Output = AccelResult> + Send + 'a {
    let req = super::CapturedRequest::from_http(req);
    async move {
        let started = Instant::now();
        match tokio::time::timeout(state.hotpath.timeout, accelerate_inner(state, &req)).await {
            Ok(AccelResult::Hit(resp)) => {
                let _ = state.metrics.record_hotpath(
                    FILES_PATH_TEMPLATE,
                    "hit",
                    started.elapsed().as_secs_f64(),
                );
                AccelResult::Hit(resp)
            }
            Ok(other) => other,
            Err(_) => {
                let _ = state.metrics.record_hotpath(
                    FILES_PATH_TEMPLATE,
                    "error",
                    started.elapsed().as_secs_f64(),
                );
                AccelResult::Fallback {
                    reason: "timeout".to_string(),
                    error: true,
                }
            }
        }
    }
}

async fn accelerate_inner(state: &AppState, req: &super::CapturedRequest) -> AccelResult {
    let parsed = match parse_files_request(&req.method, &req.path, req.query.as_deref()) {
        Ok(p) => p,
        Err(reason) => {
            return AccelResult::Fallback { reason, error: false };
        }
    };

    let cookie = req
        .headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(session_id) = session::session_id_from_cookie_header(cookie) else {
        return AccelResult::Fallback {
            reason: "session cookie missing".to_string(),
            error: false,
        };
    };
    let Some(redis_url) = state.hotpath.redis_url.as_deref() else {
        return AccelResult::Fallback {
            reason: "redis not configured".to_string(),
            error: false,
        };
    };
    let user_id = match load_session_user_id(redis_url, &session_id).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return AccelResult::Fallback {
                reason: "session decode failed".to_string(),
                error: false,
            };
        }
        Err(e) => {
            return AccelResult::Fallback {
                reason: format!("session redis: {e}"),
                error: false,
            };
        }
    };

    let Some(database_url) = state.hotpath.database_url.as_deref() else {
        return record_error(
            state,
            "postgres not configured",
        );
    };
    let project = match load_project(database_url, &parsed.project_id).await {
        Ok(Some(p)) => p,
        Ok(None) => return not_found_hit(),
        Err(e) => return record_error(state, &format!("postgres: {e}")),
    };
    if acl::repository_disabled(project.repository_access_level) {
        return not_found_hit();
    }
    let access = match load_access_level(database_url, user_id, project.id).await {
        Ok(level) => level,
        Err(e) => return record_error(state, &format!("postgres acl: {e}")),
    };
    if !acl::can_read_code(project.visibility_level, access) {
        return not_found_hit();
    }

    let max_bytes = state.hotpath.max_file_bytes;
    let file = match fetch_repository_file(state, &project, &parsed, max_bytes).await {
        Ok(FetchFile::Body(body)) => body,
        Ok(FetchFile::Oversize) => {
            return AccelResult::Fallback {
                reason: "blob exceeds memory limit".to_string(),
                error: false,
            };
        }
        Ok(FetchFile::Missing) => return not_found_hit(),
        Err(e) => return record_error(state, &e),
    };

    AccelResult::Hit(file_response(&file, parsed.head_only))
}

fn record_error(state: &AppState, reason: &str) -> AccelResult {
    let _ = state.metrics.record_hotpath(FILES_PATH_TEMPLATE, "error", 0.0);
    AccelResult::Fallback {
        reason: reason.to_string(),
        error: true,
    }
}

fn not_found_hit() -> AccelResult {
    AccelResult::Hit(
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            NOT_FOUND_BODY,
        )
            .into_response(),
    )
}

enum FetchFile {
    Body(RepositoryFileBody),
    Oversize,
    Missing,
}

struct ProjectRow {
    id: i64,
    visibility_level: i32,
    repository_storage: String,
    disk_path: String,
    full_path: String,
    repository_access_level: Option<i32>,
}

async fn load_session_user_id(redis_url: &str, session_id: &str) -> Result<Option<i64>, String> {
    let client = RedisClient::new(redis_url.to_string());
    client.connect().await?;
    for key in session::redis_session_keys(session_id) {
        if let Some(bytes) = client.get_bytes(&key).await? {
            if let Some(id) = session::user_id_from_session_bytes(&bytes) {
                return Ok(Some(id));
            }
        }
    }
    Ok(None)
}

async fn pg_client(connstr: &str) -> Result<tokio_postgres::Client, String> {
    let (client, connection) = tokio_postgres::connect(connstr, tokio_postgres::NoTls)
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

async fn load_project(connstr: &str, project_id: &str) -> Result<Option<ProjectRow>, String> {
    let client = pg_client(connstr).await?;
    let row = if project_id.bytes().all(|b| b.is_ascii_digit()) {
        client
            .query_opt(
                "SELECT p.id, p.visibility_level,
                        COALESCE(p.repository_storage, 'default') AS repository_storage,
                        pr.disk_path, r.path AS full_path, pf.repository_access_level
                 FROM projects p
                 LEFT JOIN project_repositories pr ON pr.project_id = p.id
                 LEFT JOIN routes r ON r.source_id = p.id AND r.source_type = 'Project'
                 LEFT JOIN project_features pf ON pf.project_id = p.id
                 WHERE p.id = $1",
                &[&project_id.parse::<i32>().map_err(|e| e.to_string())?],
            )
            .await
            .map_err(|e| e.to_string())?
    } else {
        client
            .query_opt(
                "SELECT p.id, p.visibility_level,
                        COALESCE(p.repository_storage, 'default') AS repository_storage,
                        pr.disk_path, r.path AS full_path, pf.repository_access_level
                 FROM projects p
                 JOIN routes r ON r.source_id = p.id AND r.source_type = 'Project'
                 LEFT JOIN project_repositories pr ON pr.project_id = p.id
                 LEFT JOIN project_features pf ON pf.project_id = p.id
                 WHERE lower(r.path) = lower($1)",
                &[&project_id],
            )
            .await
            .map_err(|e| e.to_string())?
    };
    Ok(row.map(|row| project_from_row(&row)))
}

fn pg_i64(row: &tokio_postgres::Row, idx: usize) -> i64 {
    if let Ok(v) = row.try_get::<_, i64>(idx) {
        return v;
    }
    if let Ok(v) = row.try_get::<_, i32>(idx) {
        return i64::from(v);
    }
    0
}

fn pg_i32(row: &tokio_postgres::Row, idx: usize) -> i32 {
    if let Ok(v) = row.try_get::<_, i32>(idx) {
        return v;
    }
    if let Ok(v) = row.try_get::<_, i64>(idx) {
        return v as i32;
    }
    0
}

fn pg_opt_i32(row: &tokio_postgres::Row, idx: usize) -> Option<i32> {
    if let Ok(v) = row.try_get::<_, Option<i32>>(idx) {
        return v;
    }
    if let Ok(v) = row.try_get::<_, Option<i64>>(idx) {
        return v.map(|x| x as i32);
    }
    None
}

fn project_from_row(row: &tokio_postgres::Row) -> ProjectRow {
    let id = pg_i64(row, 0);
    let visibility_level = pg_i32(row, 1);
    let repository_storage: String = row
        .try_get::<_, String>(2)
        .unwrap_or_else(|_| "default".to_string());
    let disk_path: Option<String> = row.try_get::<_, Option<String>>(3).ok().flatten();
    let full_path: Option<String> = row.try_get::<_, Option<String>>(4).ok().flatten();
    let repository_access_level = pg_opt_i32(row, 5);
    ProjectRow {
        id,
        visibility_level,
        repository_storage,
        disk_path: disk_path.unwrap_or_else(|| acl::hashed_disk_path(id)),
        full_path: full_path.unwrap_or_else(|| format!("project/{id}")),
        repository_access_level,
    }
}

async fn load_access_level(
    connstr: &str,
    user_id: i64,
    project_id: i64,
) -> Result<Option<i32>, String> {
    let client = pg_client(connstr).await?;
    let row = client
        .query_opt(
            "SELECT access_level FROM project_authorizations
             WHERE user_id = $1 AND project_id = $2
             LIMIT 1",
            &[&(user_id as i32), &(project_id as i32)],
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.map(|r| pg_i32(&r, 0)))
}

async fn fetch_repository_file(
    state: &AppState,
    project: &ProjectRow,
    parsed: &FilesRequest,
    max_bytes: u64,
) -> Result<FetchFile, String> {
    let (gitaly_addr, gitaly_token) = match (
        state.git.gitaly_address.as_deref(),
        state.git.gitaly_token.as_deref(),
    ) {
        (Some(addr), token) => (addr, token.unwrap_or("")),
        _ => return Err("gitaly not configured".to_string()),
    };
    let server = GitalyServer {
        address: gitaly_addr.to_string(),
        token: gitaly_token.to_string(),
        call_metadata: Default::default(),
    };
    let mut client = GitalyClient::connect(&server)
        .await
        .map_err(|e| format!("gitaly connect: {e}"))?;
    let repo = RepoInfo::new(
        &project.repository_storage,
        &project.disk_path,
        &project.full_path,
        &format!("project-{}", project.id),
    );

    let commit_id = match client.find_commit_id(&repo, &parsed.git_ref).await {
        Ok(Some(id)) => id,
        Ok(None) => return Ok(FetchFile::Missing),
        Err(e) => return Err(format!("gitaly find_commit: {e}")),
    };

    let entry = match client
        .tree_entry(&repo, &parsed.git_ref, &parsed.file_path, max_bytes as i64)
        .await
    {
        Ok(entry) => entry,
        Err(status) if status.code() == tonic::Code::FailedPrecondition => {
            return Ok(FetchFile::Oversize);
        }
        Err(status) if status.code() == tonic::Code::NotFound => {
            return Ok(FetchFile::Missing);
        }
        Err(e) => return Err(format!("gitaly tree_entry: {e}")),
    };

    if entry.oid.is_empty() {
        return Ok(FetchFile::Missing);
    }
    if entry.object_type != 1 {
        return Ok(FetchFile::Missing);
    }
    if blob_exceeds_limit(entry.size, max_bytes) {
        return Ok(FetchFile::Oversize);
    }

    let mut data = entry.data;
    if data.is_empty() || (entry.size > 0 && data.len() as i64 != entry.size) {
        let blob = client
            .get_blob_with_meta(&repo, &entry.oid, max_bytes as i64)
            .await
            .map_err(|e| format!("gitaly get_blob: {e}"))?;
        if blob_exceeds_limit(blob.size, max_bytes) {
            return Ok(FetchFile::Oversize);
        }
        data = blob.data;
    }
    if blob_exceeds_limit(data.len() as i64, max_bytes) {
        return Ok(FetchFile::Oversize);
    }

    let last_commit_id = match client
        .last_commit_id_for_path(&repo, &commit_id, &parsed.file_path)
        .await
    {
        Ok(Some(id)) => id,
        Ok(None) => commit_id.clone(),
        Err(_) => commit_id.clone(),
    };

    Ok(FetchFile::Body(build_file_body(
        &parsed.file_path,
        &parsed.git_ref,
        &entry.oid,
        &commit_id,
        &last_commit_id,
        entry.size,
        entry.mode,
        &data,
    )))
}

pub fn build_file_body(
    file_path: &str,
    git_ref: &str,
    blob_id: &str,
    commit_id: &str,
    last_commit_id: &str,
    size: i64,
    mode: i32,
    data: &[u8],
) -> RepositoryFileBody {
    let file_name = file_path.rsplit('/').next().unwrap_or(file_path).to_string();
    let content = base64::engine::general_purpose::STANDARD.encode(data);
    let content_sha256 = hex::encode(Sha256::digest(data));
    let execute_filemode = (mode & 0o111) != 0 || mode == 0o100755 || mode == 33261;
    RepositoryFileBody {
        file_name,
        file_path: file_path.to_string(),
        size,
        encoding: "base64".to_string(),
        content,
        content_sha256,
        git_ref: git_ref.to_string(),
        blob_id: blob_id.to_string(),
        commit_id: commit_id.to_string(),
        last_commit_id: last_commit_id.to_string(),
        execute_filemode,
    }
}

fn file_response(body: &RepositoryFileBody, head_only: bool) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    insert_str(&mut headers, "x-gitlab-file-name", &body.file_name);
    insert_str(&mut headers, "x-gitlab-file-path", &body.file_path);
    insert_str(&mut headers, "x-gitlab-size", &body.size.to_string());
    insert_str(&mut headers, "x-gitlab-encoding", &body.encoding);
    insert_str(&mut headers, "x-gitlab-content-sha256", &body.content_sha256);
    insert_str(&mut headers, "x-gitlab-ref", &body.git_ref);
    insert_str(&mut headers, "x-gitlab-blob-id", &body.blob_id);
    insert_str(&mut headers, "x-gitlab-commit-id", &body.commit_id);
    insert_str(&mut headers, "x-gitlab-last-commit-id", &body.last_commit_id);
    insert_str(
        &mut headers,
        "x-gitlab-execute-filemode",
        if body.execute_filemode { "true" } else { "false" },
    );
    if head_only {
        return (StatusCode::OK, headers).into_response();
    }
    let json = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    (StatusCode::OK, headers, json).into_response()
}

fn insert_str(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        header::HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    let query = query?;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let k = parts.next()?;
        if k == key {
            return Some(percent_decode(parts.next().unwrap_or("")));
        }
    }
    None
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl HotPathState {
    pub fn from_env() -> Self {
        let timeout_ms = std::env::var("GITLAB_RS_HOTPATH_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3000u64);
        let max_file_bytes = std::env::var("GITLAB_RS_FILES_MAX_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(32 * 1024 * 1024);
        Self {
            timeout: std::time::Duration::from_millis(timeout_ms),
            max_file_bytes,
            database_url: resolve_database_url(),
            redis_url: crate::redis::resolve_redis_url(),
        }
    }
}

fn resolve_database_url() -> Option<String> {
    if let Ok(url) = std::env::var("GITLAB_RS_DATABASE_URL") {
        if !url.is_empty() {
            return Some(url);
        }
    }
    if let Ok(url) = std::env::var("DATABASE_URL") {
        if !url.is_empty() {
            return Some(url);
        }
    }
    for path in [
        "/var/opt/gitlab/gitlab-rails/etc/database.yml",
        "/opt/gitlab/embedded/service/gitlab-rails/config/database.yml",
    ] {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(url) = database_url_from_yml(&text) {
                return Some(url);
            }
        }
    }
    if std::path::Path::new("/var/opt/gitlab/postgresql").exists() {
        return Some(
            "host=/var/opt/gitlab/postgresql user=gitlab dbname=gitlabhq_production".to_string(),
        );
    }
    None
}

pub fn database_url_from_yml(text: &str) -> Option<String> {
    let mut in_prod = false;
    let mut db = "gitlabhq_production".to_string();
    let mut user = "gitlab".to_string();
    let mut password = String::new();
    let mut host = "/var/opt/gitlab/postgresql".to_string();
    let mut port: Option<u16> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            in_prod = trimmed.starts_with("production:");
            continue;
        }
        if !in_prod {
            continue;
        }
        let Some((k, v)) = trimmed.split_once(':') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim().trim_matches('"').trim_matches('\'');
        match k {
            "database" if !v.is_empty() => db = v.to_string(),
            "username" if !v.is_empty() => user = v.to_string(),
            "password" => password = v.to_string(),
            "host" if !v.is_empty() => host = v.to_string(),
            "port" => port = v.parse().ok(),
            _ => {}
        }
    }
    let mut parts = vec![
        format!("host={host}"),
        format!("user={user}"),
        format!("dbname={db}"),
    ];
    if !password.is_empty() {
        parts.push(format!("password={password}"));
    }
    if let Some(port) = port {
        parts.push(format!("port={port}"));
    }
    Some(parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_numeric_project_and_nested_path() {
        let req = parse_files_request(
            &Method::GET,
            "/api/v4/projects/14/repository/files/app%2Fmodels%2Fkey.rb",
            Some("ref=master"),
        )
        .unwrap();
        assert_eq!(req.project_id, "14");
        assert_eq!(req.file_path, "app/models/key.rb");
        assert_eq!(req.git_ref, "master");
        assert!(!req.head_only);
    }

    #[test]
    fn parse_skips_raw_and_write_methods() {
        assert!(parse_files_request(
            &Method::GET,
            "/api/v4/projects/14/repository/files/README.md/raw",
            Some("ref=main"),
        )
        .is_err());
        assert!(parse_files_request(
            &Method::POST,
            "/api/v4/projects/14/repository/files/README.md",
            Some("ref=main"),
        )
        .is_err());
    }

    #[test]
    fn json_fields_match_ce_19_3_1() {
        let body = build_file_body(
            "app/models/key.rb",
            "master",
            "79f7bbd25901e8334750839545a9bd021f0e4c83",
            "d5a3ff139356ce33e37e73add446f16869741b50",
            "570e7b2abdd848b95f2f578043fc23bd6f6fd24d",
            4,
            0o100644,
            b"test",
        );
        let value = serde_json::to_value(&body).unwrap();
        for key in [
            "file_name",
            "file_path",
            "size",
            "encoding",
            "content",
            "blob_id",
            "commit_id",
            "last_commit_id",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert_eq!(value["file_name"], "key.rb");
        assert_eq!(value["encoding"], "base64");
        assert_eq!(value["content"], "dGVzdA==");
        assert_eq!(value["size"], 4);
        assert_eq!(value["execute_filemode"], false);
    }

    #[test]
    fn oversize_blob_is_fallback() {
        assert!(blob_exceeds_limit(100, 50));
        assert!(!blob_exceeds_limit(50, 50));
        assert!(!blob_exceeds_limit(0, 50));
        assert!(blob_exceeds_limit(-1, 50));
    }

    #[test]
    fn acl_guest_private_is_denied() {
        assert!(!acl::can_read_code(
            acl::VISIBILITY_PRIVATE,
            Some(acl::ACCESS_GUEST)
        ));
    }

    #[test]
    fn parses_omnibus_database_yml() {
        let yml = r#"
production:
  adapter: postgresql
  encoding: unicode
  database: gitlabhq_production
  username: gitlab
  password: "s3cret"
  host: "/var/opt/gitlab/postgresql"
"#;
        let url = database_url_from_yml(yml).unwrap();
        assert!(url.contains("user=gitlab"));
        assert!(url.contains("dbname=gitlabhq_production"));
        assert!(url.contains("password=s3cret"));
        assert!(url.contains("host=/var/opt/gitlab/postgresql"));
    }
}
