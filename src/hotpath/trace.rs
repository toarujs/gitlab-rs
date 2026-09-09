//! Accelerate `GET/HEAD /api/v4/projects/:id/jobs/:job_id/trace`.

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
use super::session;
use super::TRACE_PATH_TEMPLATE;
use crate::redis::RedisClient;
use crate::state::AppState;

const NOT_FOUND_BODY: &str = "{\"message\":\"404 Not Found\"}";
const TRACE_FILE_TYPE: i32 = 3;
const LOCAL_STORE: i32 = 1;

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
    pub offset: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteSpan {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedRange {
    All,
    Span(ByteSpan),
    Unsatisfiable,
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
    Ok(TraceRequest {
        project_id: percent_decode(project_id),
        job_id,
        head_only: method == Method::HEAD,
        offset: query_u64(query, "offset"),
    })
}

/// Parse RFC 7233 `Range: bytes=` or GitLab poll `offset=` against `file_len`.
pub fn resolve_range(
    range_header: Option<&str>,
    offset: Option<u64>,
    file_len: u64,
) -> ParsedRange {
    if let Some(header) = range_header {
        if let Some(parsed) = parse_range_header(header, file_len) {
            return parsed;
        }
    }
    if let Some(off) = offset {
        if file_len == 0 {
            return if off == 0 {
                ParsedRange::All
            } else {
                ParsedRange::Unsatisfiable
            };
        }
        if off >= file_len {
            return ParsedRange::Unsatisfiable;
        }
        return ParsedRange::Span(ByteSpan {
            start: off,
            end: file_len - 1,
        });
    }
    ParsedRange::All
}

pub fn parse_range_header(value: &str, file_len: u64) -> Option<ParsedRange> {
    let spec = value.trim().strip_prefix("bytes=")?.trim();
    let spec = spec.split(',').next()?.trim();
    let (start, end) = spec.split_once('-')?;
    let start = start.trim();
    let end = end.trim();
    if start.is_empty() && end.is_empty() {
        return None;
    }
    if start.is_empty() {
        let n: u64 = end.parse().ok()?;
        if n == 0 || file_len == 0 {
            return Some(ParsedRange::Unsatisfiable);
        }
        let n = n.min(file_len);
        return Some(ParsedRange::Span(ByteSpan {
            start: file_len - n,
            end: file_len - 1,
        }));
    }
    let start: u64 = start.parse().ok()?;
    if file_len == 0 || start >= file_len {
        return Some(ParsedRange::Unsatisfiable);
    }
    let end = if end.is_empty() {
        file_len - 1
    } else {
        end.parse::<u64>().ok()?.min(file_len - 1)
    };
    if end < start {
        return Some(ParsedRange::Unsatisfiable);
    }
    Some(ParsedRange::Span(ByteSpan { start, end }))
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

pub fn job_token_from_headers(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    if let Some(v) = headers
        .get("JOB-TOKEN")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(v.to_string());
    }
    query_param(query, "job_token").filter(|s| !s.is_empty())
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
    let parsed = match parse_trace_request(&req.method, &req.path, req.query.as_deref()) {
        Ok(p) => p,
        Err(reason) => {
            return AccelResult::Fallback {
                reason,
                error: false,
            };
        }
    };

    let job_token = job_token_from_headers(&req.headers, req.query.as_deref());
    let cookie = req
        .headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session_id = session::session_id_from_cookie_header(cookie);

    if session_id.is_none() {
        if job_token.is_some() {
            return AccelResult::Fallback {
                reason: "job token not verified locally".to_string(),
                error: false,
            };
        }
        return AccelResult::Fallback {
            reason: "session cookie missing".to_string(),
            error: false,
        };
    }
    let session_id = session_id.unwrap();

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
        return record_error(state, "postgres not configured");
    };
    let project = match load_project(database_url, &parsed.project_id).await {
        Ok(Some(p)) => p,
        Ok(None) => return not_found_hit(),
        Err(e) => return record_error(state, &format!("postgres: {e}")),
    };
    let access = match load_access_level(database_url, user_id, project.id).await {
        Ok(level) => level,
        Err(e) => return record_error(state, &format!("postgres acl: {e}")),
    };
    if !acl::can_read_build(
        project.visibility_level,
        project.builds_access_level,
        project.public_builds,
        access,
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

    let range_header = req
        .headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());
    match serve_trace_file(&path, range_header, parsed.offset, parsed.head_only, &job.status)
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

struct ProjectRow {
    id: i64,
    visibility_level: i32,
    public_builds: bool,
    builds_access_level: Option<i32>,
}

struct JobRow {
    project_id: i64,
    job_id: i64,
    status: String,
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
    range_header: Option<&str>,
    offset: Option<u64>,
    head_only: bool,
    job_status: &str,
) -> Result<Response, String> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| e.to_string())?;
    let file_len = meta.len();
    let range = resolve_range(range_header, offset, file_len);
    Ok(trace_response(path, file_len, range, head_only, job_status).await?)
}

async fn trace_response(
    path: &Path,
    file_len: u64,
    range: ParsedRange,
    head_only: bool,
    job_status: &str,
) -> Result<Response, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::ACCEPT_RANGES,
        HeaderValue::from_static("bytes"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    if !job_status.is_empty() {
        if let Ok(v) = HeaderValue::from_str(job_status) {
            headers.insert("job-status", v);
        }
    }

    match range {
        ParsedRange::Unsatisfiable => {
            let cr = format!("bytes */{file_len}");
            if let Ok(v) = HeaderValue::from_str(&cr) {
                headers.insert(header::CONTENT_RANGE, v);
            }
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
            Ok((StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response())
        }
        ParsedRange::All => {
            headers.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&file_len.to_string()).unwrap_or(HeaderValue::from_static("0")),
            );
            if head_only || file_len == 0 {
                return Ok((StatusCode::OK, headers).into_response());
            }
            let body = file_body(path, 0, file_len).await?;
            Ok((StatusCode::OK, headers, body).into_response())
        }
        ParsedRange::Span(span) => {
            let len = span.end.saturating_sub(span.start).saturating_add(1);
            let cr = format!("bytes {}-{}/{}", span.start, span.end, file_len);
            if let Ok(v) = HeaderValue::from_str(&cr) {
                headers.insert(header::CONTENT_RANGE, v);
            }
            headers.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&len.to_string()).unwrap_or(HeaderValue::from_static("0")),
            );
            if head_only || len == 0 {
                return Ok((StatusCode::PARTIAL_CONTENT, headers).into_response());
            }
            let body = file_body(path, span.start, len).await?;
            Ok((StatusCode::PARTIAL_CONTENT, headers, body).into_response())
        }
    }
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

fn is_undefined_table(err: &str) -> bool {
    err.contains("does not exist") || err.contains("42P01")
}

async fn load_project(connstr: &str, project_id: &str) -> Result<Option<ProjectRow>, String> {
    let client = pg_client(connstr).await?;
    let row = if project_id.bytes().all(|b| b.is_ascii_digit()) {
        let id = project_id.parse::<i32>().map_err(|e| e.to_string())?;
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
            &[&(user_id as i32), &(project_id as i32)],
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
        "SELECT b.id, b.project_id, b.status, b.created_at,
                a.id, a.file, a.file_final_path, a.created_at, a.file_store
         FROM {builds} b
         LEFT JOIN {artifacts} a
           ON a.job_id = b.id AND a.project_id = b.project_id AND a.file_type = $3
         WHERE b.id = $1 AND b.project_id = $2
         ORDER BY a.id DESC NULLS LAST
         LIMIT 1"
    );
    let row = client
        .query_opt(&sql, &[&job_id, &(project_id as i32), &TRACE_FILE_TYPE])
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.map(|row| job_from_row(&row)))
}

fn job_from_row(row: &tokio_postgres::Row) -> JobRow {
    let filename = row
        .try_get::<_, Option<String>>(5)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "job.log".to_string());
    JobRow {
        job_id: pg_i64(row, 0),
        project_id: pg_i64(row, 1),
        status: pg_status(row, 2),
        job_created: pg_time(row, 3),
        artifact_id: pg_opt_i64(row, 4),
        filename: safe_filename(&filename).to_string(),
        file_final_path: row.try_get::<_, Option<String>>(6).ok().flatten(),
        artifact_created: pg_time(row, 7),
        file_store: pg_opt_i32(row, 8),
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

fn pg_status(row: &tokio_postgres::Row, idx: usize) -> String {
    if let Ok(s) = row.try_get::<_, String>(idx) {
        return s;
    }
    if let Ok(s) = row.try_get::<_, Option<String>>(idx) {
        return s.unwrap_or_default();
    }
    String::new()
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

fn query_u64(query: Option<&str>, key: &str) -> Option<u64> {
    query_param(query, key)?.parse().ok()
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
            Some("offset=10"),
        )
        .unwrap();
        assert_eq!(req.project_id, "14");
        assert_eq!(req.job_id, 19);
        assert_eq!(req.offset, Some(10));
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
    fn parse_range_open_end() {
        let r = resolve_range(Some("bytes=10-"), None, 100);
        assert_eq!(
            r,
            ParsedRange::Span(ByteSpan {
                start: 10,
                end: 99
            })
        );
    }

    #[test]
    fn parse_range_closed_and_suffix() {
        assert_eq!(
            resolve_range(Some("bytes=0-9"), None, 100),
            ParsedRange::Span(ByteSpan { start: 0, end: 9 })
        );
        assert_eq!(
            resolve_range(Some("bytes=-10"), None, 100),
            ParsedRange::Span(ByteSpan {
                start: 90,
                end: 99
            })
        );
    }

    #[test]
    fn parse_range_unsatisfiable() {
        assert_eq!(
            resolve_range(Some("bytes=100-"), None, 100),
            ParsedRange::Unsatisfiable
        );
        assert_eq!(
            resolve_range(Some("bytes=0-"), None, 0),
            ParsedRange::Unsatisfiable
        );
    }

    #[test]
    fn offset_query_is_poll_range() {
        assert_eq!(
            resolve_range(None, Some(50), 100),
            ParsedRange::Span(ByteSpan {
                start: 50,
                end: 99
            })
        );
        assert_eq!(
            resolve_range(None, Some(100), 100),
            ParsedRange::Unsatisfiable
        );
        assert_eq!(resolve_range(None, None, 100), ParsedRange::All);
    }

    #[test]
    fn range_header_wins_over_offset() {
        assert_eq!(
            resolve_range(Some("bytes=2-4"), Some(50), 100),
            ParsedRange::Span(ByteSpan { start: 2, end: 4 })
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
    fn job_token_from_header_and_query() {
        let mut headers = HeaderMap::new();
        headers.insert("JOB-TOKEN", HeaderValue::from_static("abc"));
        assert_eq!(
            job_token_from_headers(&headers, None).as_deref(),
            Some("abc")
        );
        assert_eq!(
            job_token_from_headers(&HeaderMap::new(), Some("job_token=xyz")).as_deref(),
            Some("xyz")
        );
    }

    #[test]
    fn parent_dir_final_path_rejected() {
        assert!(safe_join(Path::new("/art"), "../etc/passwd").is_none());
        assert!(safe_join(Path::new("/art"), "aa/bb/job.log").is_some());
    }
}
