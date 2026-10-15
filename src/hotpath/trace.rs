//! Accelerate `GET/HEAD /api/v4/projects/:id/jobs/:job_id/trace`.
//!
//! Only the **API** trace endpoint is accelerated. The **web** route
//! `/-/jobs/:id/raw` is served by Rails (`Projects::JobsController#raw`) and is
//! deliberately left alone. The two endpoints differ and must not be conflated:
//!
//! | aspect        | API `/api/v4/projects/:id/jobs/:id/trace` | web `/-/jobs/:id/raw` |
//! |---------------|-------------------------------------------|-----------------------|
//! | `byte_offset` | supported (clamped to EOF)                | ignored (always full) |
//! | `byte_limit`  | supported (1..=512000, else 400)          | ignored (always full) |
//! | `Range`       | ignored, 200 full body                    | honoured by `send_file` (can 206) |
//! | auth          | session or PAT/job token                  | session (web) |
//!
//! Contract: `byte_limit=0` or `> 512000` is a client error (falls back to
//! Rails, which returns 400); `byte_offset` beyond EOF yields an empty 200.

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use super::acl;
use super::auth;
use super::TRACE_PATH_TEMPLATE;
use crate::state::AppState;

const NOT_FOUND_BODY: &str = "{\"message\":\"404 Not Found\"}";
const TRACE_FILE_TYPE: i32 = 3;
const LOCAL_STORE: i32 = 1;
const SKIP_ACCEL_HEADER: &str = "x-gitlab-rs-skip-accel";

#[derive(Debug)]
pub enum AccelResult {
    Hit(Response),
    Fallback { reason: String, error: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceRequest {
    pub project_id: String,
    pub job_id: i64,
    pub head_only: bool,
    pub byte_offset: Option<u64>,
    pub byte_limit: Option<u64>,
}

/// Upper bound the GitLab trace API accepts for `byte_limit`
/// (`Gitlab::Ci::Trace::Stream::LIMIT_SIZE`, 500 KiB).
pub const MAX_TRACE_BYTE_LIMIT: u64 = 512_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceRange {
    /// Full trace (no range params), matching `Ci::Trace#raw`.
    All,
    /// Byte window `start..start+len` (len may be 0), matching
    /// `Ci::Trace#raw_range(byte_offset:, byte_limit:)`.
    Span { start: u64, len: u64 },
}

pub fn parse_trace_request(
    method: &Method,
    path: &str,
    query: Option<&str>,
) -> Result<TraceRequest, String> {
    if method != Method::GET && method != Method::HEAD {
        return Err("method not accelerated".to_string());
    }
    let path = path.split('?').next().unwrap_or(path);
    let rest = path
        .strip_prefix("/api/v4/projects/")
        .ok_or_else(|| "not a trace path".to_string())?;
    let (project_id, after_jobs) = rest
        .split_once("/jobs/")
        .ok_or_else(|| "not a trace path".to_string())?;
    if project_id.is_empty() {
        return Err("missing project id".to_string());
    }
    let job_id_str = after_jobs
        .strip_suffix("/trace")
        .ok_or_else(|| "not a trace path".to_string())?;
    if job_id_str.is_empty() || !job_id_str.bytes().all(|b| b.is_ascii_digit()) {
        return Err("missing job id".to_string());
    }
    let job_id = job_id_str
        .parse::<i64>()
        .map_err(|_| "invalid job id".to_string())?;
    let (byte_offset, byte_limit) = parse_byte_range(query)?;
    Ok(TraceRequest {
        project_id: percent_decode(project_id),
        job_id,
        head_only: method == Method::HEAD,
        byte_offset,
        byte_limit,
    })
}

/// Parse the optional `byte_offset` / `byte_limit` params of the GitLab trace
/// API. Invalid values are rejected so the caller falls back to Rails, which
/// returns the same 400 the upstream API does.
fn parse_byte_range(query: Option<&str>) -> Result<(Option<u64>, Option<u64>), String> {
    let byte_offset = match query_param(query, "byte_offset") {
        Some(raw) => Some(
            raw.trim()
                .parse::<u64>()
                .map_err(|_| "invalid byte_offset".to_string())?,
        ),
        None => None,
    };
    let byte_limit = match query_param(query, "byte_limit") {
        Some(raw) => {
            let value = raw
                .trim()
                .parse::<u64>()
                .map_err(|_| "invalid byte_limit".to_string())?;
            if value == 0 || value > MAX_TRACE_BYTE_LIMIT {
                return Err("invalid byte_limit".to_string());
            }
            Some(value)
        }
        None => None,
    };
    Ok((byte_offset, byte_limit))
}

/// Resolve the window `raw_range` would read from a `file_len`-byte trace.
/// `byte_offset` is clamped to EOF and `byte_limit` is a maximum, so an
/// over-long limit simply reads to EOF.
pub fn resolve_trace_range(
    byte_offset: Option<u64>,
    byte_limit: Option<u64>,
    file_len: u64,
) -> TraceRange {
    if byte_offset.is_none() && byte_limit.is_none() {
        return TraceRange::All;
    }
    let start = byte_offset.unwrap_or(0).min(file_len);
    let remaining = file_len - start;
    let len = byte_limit.map_or(remaining, |limit| limit.min(remaining));
    TraceRange::Span { start, len }
}

pub fn artifacts_disk_hash(project_id: i64) -> String {
    hex::encode(Sha256::digest(project_id.to_string().as_bytes()))
}

pub fn artifact_trace_paths(
    artifacts_root: &Path,
    project_id: i64,
    job_id: i64,
    artifact_id: i64,
    ymd: &str,
    filename: &str,
    file_final_path: Option<&str>,
) -> Vec<PathBuf> {
    let hash = artifacts_disk_hash(project_id);
    let filename = safe_filename(filename);
    let job = job_id.to_string();
    let art = artifact_id.to_string();
    let mut out = vec![
        artifacts_root
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash)
            .join(ymd)
            .join(&job)
            .join(&art)
            .join(filename),
        artifacts_root
            .join(&hash)
            .join(ymd)
            .join(&job)
            .join(&art)
            .join(filename),
    ];
    if let Some(rel) = file_final_path {
        if let Some(p) = safe_join(artifacts_root, rel) {
            out.push(p);
        }
    }
    out
}

pub fn live_trace_paths(
    builds_root: &Path,
    project_id: i64,
    job_id: i64,
    ym: &str,
) -> Vec<PathBuf> {
    let hash = artifacts_disk_hash(project_id);
    let name = format!("{job_id}.log");
    vec![
        builds_root
            .join(ym)
            .join(project_id.to_string())
            .join(&name),
        builds_root.join(ym).join(&hash).join(&name),
    ]
}

pub fn select_local_trace(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
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
                    TRACE_PATH_TEMPLATE,
                    "hit",
                    started.elapsed().as_secs_f64(),
                );
                AccelResult::Hit(resp)
            }
            Ok(other) => other,
            Err(_) => {
                let _ = state.metrics.record_hotpath(
                    TRACE_PATH_TEMPLATE,
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
    if req.headers.get(SKIP_ACCEL_HEADER).is_some() {
        return AccelResult::Fallback {
            reason: "skip accel header".to_string(),
            error: false,
        };
    }
    let parsed = match parse_trace_request(&req.method, &req.path, req.query.as_deref()) {
        Ok(p) => p,
        Err(reason) => {
            return AccelResult::Fallback {
                reason,
                error: false,
            };
        }
    };

    let Some(database_url) = state.hotpath.database_url.as_deref() else {
        return record_error(state, "postgres not configured");
    };
    let identity = match auth::resolve_identity(
        &req.headers,
        req.query.as_deref(),
        state.hotpath.redis_url.as_deref(),
        database_url,
    )
    .await
    {
        Ok(identity) => identity,
        Err(auth::AuthError::Unauthorized) => return unauthorized_hit(),
        Err(auth::AuthError::Fallback { reason, error }) => {
            return AccelResult::Fallback { reason, error };
        }
    };
    if !identity.logged_in() {
        return unauthorized_hit();
    }
    let project = match load_project(database_url, &parsed.project_id).await {
        Ok(Some(p)) => p,
        Ok(None) => return not_found_hit(),
        Err(e) => return record_error(state, &format!("postgres: {e}")),
    };
    let access = match identity.user_id {
        Some(_) if identity.admin => Some(acl::ACCESS_OWNER),
        Some(user_id) => match load_access_level(database_url, user_id, project.id).await {
            Ok(level) => level,
            Err(e) => return record_error(state, &format!("postgres acl: {e}")),
        },
        None => None,
    };
    if !acl::can_read_build_for(
        project.visibility_level,
        project.builds_access_level,
        project.public_builds,
        access,
        identity.logged_in(),
        identity.external,
    ) {
        return not_found_hit();
    }

    let job = match load_job(database_url, project.id, parsed.job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => return not_found_hit(),
        Err(e) => return record_error(state, &format!("postgres job: {e}")),
    };
    if job.file_store.is_some_and(|s| s != LOCAL_STORE) {
        return AccelResult::Fallback {
            reason: "trace in object storage".to_string(),
            error: false,
        };
    }

    let candidates = trace_candidates(&job);
    let Some(path) = select_local_trace(&candidates) else {
        return AccelResult::Fallback {
            reason: "trace file missing".to_string(),
            error: false,
        };
    };

    match serve_trace_file(
        &path,
        parsed.byte_offset,
        parsed.byte_limit,
        parsed.head_only,
        job.job_id,
    )
    .await
    {
        Ok(resp) => AccelResult::Hit(resp),
        Err(e) => record_error(state, &format!("read trace: {e}")),
    }
}

fn record_error(state: &AppState, reason: &str) -> AccelResult {
    let _ = state.metrics.record_hotpath(TRACE_PATH_TEMPLATE, "error", 0.0);
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

fn unauthorized_hit() -> AccelResult {
    AccelResult::Hit(
        (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            auth::UNAUTHORIZED_JSON,
        )
            .into_response(),
    )
}

struct ProjectRow {
    id: i64,
    visibility_level: i32,
    public_builds: bool,
    builds_access_level: Option<i32>,
}

struct JobRow {
    project_id: i64,
    job_id: i64,
    job_created: Option<SystemTime>,
    artifact_id: Option<i64>,
    artifact_created: Option<SystemTime>,
    filename: String,
    file_final_path: Option<String>,
    file_store: Option<i32>,
}

fn artifacts_root() -> PathBuf {
    std::env::var("GITLAB_RS_ARTIFACTS_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/opt/gitlab/gitlab-rails/shared/artifacts"))
}

fn builds_root() -> PathBuf {
    std::env::var("GITLAB_RS_BUILDS_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/opt/gitlab/gitlab-ci/builds"))
}

fn trace_candidates(job: &JobRow) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let ymd = job
        .artifact_created
        .or(job.job_created)
        .map(format_ymd);
    if let (Some(artifact_id), Some(ymd)) = (job.artifact_id, ymd.as_deref()) {
        out.extend(artifact_trace_paths(
            &artifacts_root(),
            job.project_id,
            job.job_id,
            artifact_id,
            ymd,
            &job.filename,
            job.file_final_path.as_deref(),
        ));
    } else if let Some(rel) = job.file_final_path.as_deref() {
        if let Some(p) = safe_join(&artifacts_root(), rel) {
            out.push(p);
        }
    }
    if let Some(ts) = job.job_created {
        out.extend(live_trace_paths(
            &builds_root(),
            job.project_id,
            job.job_id,
            &format_ym(ts),
        ));
    } else {
        out.extend(live_trace_paths(
            &builds_root(),
            job.project_id,
            job.job_id,
            &format_ym(SystemTime::now()),
        ));
    }
    out
}

async fn serve_trace_file(
    path: &Path,
    byte_offset: Option<u64>,
    byte_limit: Option<u64>,
    head_only: bool,
    job_id: i64,
) -> Result<Response, String> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| e.to_string())?;
    let file_len = meta.len();
    let range = resolve_trace_range(byte_offset, byte_limit, file_len);
    trace_response(path, file_len, range, head_only, job_id).await
}

async fn trace_response(
    path: &Path,
    file_len: u64,
    range: TraceRange,
    head_only: bool,
    job_id: i64,
) -> Result<Response, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    // Mirrors the API's `Content-Disposition: infile; filename="<id>.log"`.
    if let Ok(v) = HeaderValue::from_str(&format!("infile; filename=\"{job_id}.log\"")) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }

    let (start, len) = match range {
        TraceRange::All => (0, file_len),
        TraceRange::Span { start, len } => (start, len),
    };
    // Upstream omits Content-Length on HEAD (Rack streams the body), so only
    // set it on GET to stay byte-for-byte aligned with the Rails response.
    if !head_only {
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&len.to_string()).unwrap_or(HeaderValue::from_static("0")),
        );
    }
    if head_only || len == 0 {
        return Ok((StatusCode::OK, headers).into_response());
    }
    let body = file_body(path, start, len).await?;
    Ok((StatusCode::OK, headers, body).into_response())
}

async fn file_body(path: &Path, start: u64, len: u64) -> Result<Body, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| e.to_string())?;
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| e.to_string())?;
    }
    let limited = file.take(len);
    let stream = ReaderStream::new(limited).map(|result| {
        result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    });
    Ok(Body::from_stream(stream))
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

fn is_undefined_table(err: &str) -> bool {
    err.contains("does not exist") || err.contains("42P01")
}

async fn load_project(connstr: &str, project_id: &str) -> Result<Option<ProjectRow>, String> {
    let client = pg_client(connstr).await?;
    let row = if project_id.bytes().all(|b| b.is_ascii_digit()) {
        let id = project_id.parse::<i64>().map_err(|e| e.to_string())?;
        client
            .query_opt(
                "SELECT p.id, p.visibility_level,
                        COALESCE(p.public_builds, true) AS public_builds,
                        pf.builds_access_level
                 FROM projects p
                 LEFT JOIN project_features pf ON pf.project_id = p.id
                 WHERE p.id = $1",
                &[&id],
            )
            .await
            .map_err(|e| e.to_string())?
    } else {
        client
            .query_opt(
                "SELECT p.id, p.visibility_level,
                        COALESCE(p.public_builds, true) AS public_builds,
                        pf.builds_access_level
                 FROM projects p
                 JOIN routes r ON r.source_id = p.id AND r.source_type = 'Project'
                 LEFT JOIN project_features pf ON pf.project_id = p.id
                 WHERE lower(r.path) = lower($1)",
                &[&project_id],
            )
            .await
            .map_err(|e| e.to_string())?
    };
    Ok(row.map(|row| ProjectRow {
        id: pg_i64(&row, 0),
        visibility_level: pg_i32(&row, 1),
        public_builds: pg_bool(&row, 2, true),
        builds_access_level: pg_opt_i32(&row, 3),
    }))
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
            &[&user_id, &project_id],
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.map(|r| pg_i32(&r, 0)))
}

async fn load_job(connstr: &str, project_id: i64, job_id: i64) -> Result<Option<JobRow>, String> {
    let client = pg_client(connstr).await?;
    match query_job(&client, project_id, job_id, true).await {
        Ok(row) => Ok(row),
        Err(e) if is_undefined_table(&e) => query_job(&client, project_id, job_id, false).await,
        Err(e) => Err(e),
    }
}

async fn query_job(
    client: &tokio_postgres::Client,
    project_id: i64,
    job_id: i64,
    partitioned: bool,
) -> Result<Option<JobRow>, String> {
    let (builds, artifacts) = if partitioned {
        ("p_ci_builds", "p_ci_job_artifacts")
    } else {
        ("ci_builds", "ci_job_artifacts")
    };
    let sql = format!(
        "SELECT b.id, b.project_id, b.created_at,
                a.id, a.file, a.file_final_path, a.created_at, a.file_store
         FROM {builds} b
         LEFT JOIN {artifacts} a
           ON a.job_id = b.id AND a.project_id = b.project_id AND a.file_type = $3
         WHERE b.id = $1 AND b.project_id = $2
         ORDER BY a.id DESC NULLS LAST
         LIMIT 1"
    );
    let row = client
        .query_opt(&sql, &[&job_id, &project_id, &TRACE_FILE_TYPE])
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.map(|row| job_from_row(&row)))
}

fn job_from_row(row: &tokio_postgres::Row) -> JobRow {
    let filename = row
        .try_get::<_, Option<String>>(4)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "job.log".to_string());
    JobRow {
        job_id: pg_i64(row, 0),
        project_id: pg_i64(row, 1),
        job_created: pg_time(row, 2),
        artifact_id: pg_opt_i64(row, 3),
        filename: safe_filename(&filename).to_string(),
        file_final_path: row.try_get::<_, Option<String>>(5).ok().flatten(),
        artifact_created: pg_time(row, 6),
        file_store: pg_opt_i32(row, 7),
    }
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

fn pg_opt_i64(row: &tokio_postgres::Row, idx: usize) -> Option<i64> {
    if let Ok(v) = row.try_get::<_, Option<i64>>(idx) {
        return v;
    }
    if let Ok(v) = row.try_get::<_, Option<i32>>(idx) {
        return v.map(i64::from);
    }
    None
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

fn pg_bool(row: &tokio_postgres::Row, idx: usize, default: bool) -> bool {
    if let Ok(v) = row.try_get::<_, bool>(idx) {
        return v;
    }
    if let Ok(v) = row.try_get::<_, Option<bool>>(idx) {
        return v.unwrap_or(default);
    }
    default
}

fn pg_time(row: &tokio_postgres::Row, idx: usize) -> Option<SystemTime> {
    row.try_get::<_, SystemTime>(idx)
        .ok()
        .or_else(|| row.try_get::<_, Option<SystemTime>>(idx).ok().flatten())
}

fn format_ymd(ts: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(ts)
        .format("%Y_%m_%d")
        .to_string()
}

fn format_ym(ts: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(ts)
        .format("%Y_%m")
        .to_string()
}

fn safe_filename(name: &str) -> &str {
    Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .unwrap_or("job.log")
}

fn safe_join(root: &Path, relative: &str) -> Option<PathBuf> {
    let rel = Path::new(relative);
    if rel.is_absolute() {
        return None;
    }
    if rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some(root.join(rel))
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
            if let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_numeric_project_and_job() {
        let req = parse_trace_request(
            &Method::GET,
            "/api/v4/projects/14/jobs/19/trace",
            Some("byte_offset=10&byte_limit=5"),
        )
        .unwrap();
        assert_eq!(req.project_id, "14");
        assert_eq!(req.job_id, 19);
        assert_eq!(req.byte_offset, Some(10));
        assert_eq!(req.byte_limit, Some(5));
        assert!(!req.head_only);
    }

    #[test]
    fn parse_skips_write_methods() {
        assert!(parse_trace_request(
            &Method::POST,
            "/api/v4/projects/14/jobs/19/trace",
            None
        )
        .is_err());
        assert!(parse_trace_request(
            &Method::PATCH,
            "/api/v4/projects/14/jobs/19/trace",
            None
        )
        .is_err());
    }

    #[test]
    fn reject_invalid_byte_params() {
        // Falls back to Rails, which returns the upstream 400 body.
        for query in [
            "byte_offset=-1",
            "byte_offset=abc",
            "byte_limit=0",
            "byte_limit=512001",
            "byte_limit=abc",
        ] {
            assert!(
                parse_trace_request(&Method::GET, "/api/v4/projects/14/jobs/19/trace", Some(query))
                    .is_err(),
                "{query} should be rejected"
            );
        }
    }

    #[test]
    fn byte_range_clamps_offset_and_limit() {
        // byte_offset alone reads to EOF.
        assert_eq!(
            resolve_trace_range(Some(50), None, 100),
            TraceRange::Span { start: 50, len: 50 }
        );
        // byte_limit alone starts at 0.
        assert_eq!(
            resolve_trace_range(None, Some(10), 100),
            TraceRange::Span { start: 0, len: 10 }
        );
        // over-long limit reads to EOF (limit is a maximum).
        assert_eq!(
            resolve_trace_range(Some(5), Some(99999), 100),
            TraceRange::Span { start: 5, len: 95 }
        );
        // offset beyond EOF yields an empty window, not an error.
        assert_eq!(
            resolve_trace_range(Some(100), None, 100),
            TraceRange::Span { start: 100, len: 0 }
        );
        assert_eq!(
            resolve_trace_range(Some(9999), Some(10), 100),
            TraceRange::Span { start: 100, len: 0 }
        );
    }

    #[test]
    fn no_byte_params_reads_all() {
        assert_eq!(resolve_trace_range(None, None, 100), TraceRange::All);
        assert_eq!(resolve_trace_range(None, None, 0), TraceRange::All);
    }

    // --- Contract regression tests (API trace endpoint) -------------------
    // These encode the behaviour that differs from the web `/-/jobs/:id/raw`
    // route, which is not accelerated. See the module docs.

    #[test]
    fn contract_byte_limit_zero_is_client_error() {
        // `byte_limit=0` must be rejected so the request falls back to Rails,
        // whose API returns 400 (`byte_limit does not have a valid value`).
        assert!(parse_trace_request(
            &Method::GET,
            "/api/v4/projects/14/jobs/19/trace",
            Some("byte_limit=0"),
        )
        .is_err());
    }

    #[test]
    fn contract_byte_offset_beyond_eof_is_empty() {
        // An offset past EOF must yield an empty 200 window, never an error.
        assert_eq!(
            resolve_trace_range(Some(999_999), Some(100), 3_821),
            TraceRange::Span {
                start: 3_821,
                len: 0
            }
        );
    }

    #[test]
    fn contract_range_and_legacy_offset_are_ignored() {
        // `Range` is an HTTP header the parser never reads, and the legacy
        // `offset` query param is not recognised: both must fall back to a full
        // read (200), matching the API route's behaviour.
        let req = parse_trace_request(
            &Method::GET,
            "/api/v4/projects/14/jobs/19/trace",
            Some("offset=10&range=bytes%3D0-119"),
        )
        .unwrap();
        assert_eq!(req.byte_offset, None);
        assert_eq!(req.byte_limit, None);
        assert_eq!(
            resolve_trace_range(req.byte_offset, req.byte_limit, 3_821),
            TraceRange::All
        );
    }

    #[test]
    fn missing_local_file_is_fallback() {
        let missing = PathBuf::from("/tmp/gitlab-rs-trace-missing-does-not-exist.log");
        assert!(select_local_trace(&[missing]).is_none());
    }

    #[test]
    fn existing_local_file_is_selected() {
        let dir = std::env::temp_dir().join("gitlab-rs-trace-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("job.log");
        std::fs::write(&path, b"hello-trace").unwrap();
        assert_eq!(select_local_trace(&[path.clone()]), Some(path));
    }

    #[test]
    fn permission_denied_for_non_member_private() {
        assert!(!acl::can_read_build(
            acl::VISIBILITY_PRIVATE,
            Some(acl::FEATURE_ENABLED),
            true,
            None
        ));
        assert!(acl::can_read_build(
            acl::VISIBILITY_PRIVATE,
            Some(acl::FEATURE_ENABLED),
            true,
            Some(acl::ACCESS_GUEST)
        ));
    }

    #[test]
    fn anonymous_can_read_public_builds_only() {
        assert!(acl::can_read_build_for(
            acl::VISIBILITY_PUBLIC,
            Some(acl::FEATURE_ENABLED),
            true,
            None,
            false,
            false
        ));
        assert!(!acl::can_read_build_for(
            acl::VISIBILITY_INTERNAL,
            Some(acl::FEATURE_ENABLED),
            true,
            None,
            false,
            false
        ));
    }

    #[test]
    fn hashed_artifact_path_uses_project_sha() {
        let hash = artifacts_disk_hash(1);
        assert_eq!(hash.len(), 64);
        let paths = artifact_trace_paths(
            Path::new("/art"),
            1,
            19,
            42,
            "2026_09_08",
            "job.log",
            None,
        );
        let hashed = paths[0].to_string_lossy();
        assert!(hashed.contains(&format!("/{}/{}/{}", &hash[..2], &hash[2..4], hash)));
        assert!(hashed.ends_with("/2026_09_08/19/42/job.log"));
    }

    #[test]
    fn parent_dir_final_path_rejected() {
        assert!(safe_join(Path::new("/art"), "../etc/passwd").is_none());
        assert!(safe_join(Path::new("/art"), "aa/bb/job.log").is_some());
    }
}
