//! Accelerate `GET /api/v4/projects/:id/repository/{tree,commits,archive,branches}` and commit show.

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{FixedOffset, TimeZone, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;
use url::form_urlencoded;

use super::acl;
use super::auth;
use super::{ARCHIVE_PATH_TEMPLATE, COMMITS_PATH_TEMPLATE, TREE_PATH_TEMPLATE};
use super::{BRANCHES_PATH_TEMPLATE, COMMIT_SHOW_PATH_TEMPLATE};
use crate::gitaly::{gitaly::GitCommit, GitalyClient, GitalyServer, RepoInfo};
use crate::state::AppState;

const NOT_FOUND_BODY: &str = "{\"message\":\"404 Project Not Found\"}";
const FILE_NOT_FOUND_BODY: &str = "{\"message\":\"404 File Not Found\"}";
const COMMIT_NOT_FOUND_BODY: &str = "{\"message\":\"404 Commit Not Found\"}";
const DEFAULT_PER_PAGE: u32 = 20;
const MAX_PER_PAGE: u32 = 100;
const MAX_TREE_ENTRIES: usize = 10_000;
const MAX_BRANCHES: usize = 10_000;
const ACCESS_DEVELOPER: i32 = 30;
const SKIP_ACCEL_HEADER: &str = "x-gitlab-rs-skip-accel";

#[derive(Debug)]
pub enum AccelResult {
    Hit(Response),
    Fallback { reason: String, error: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRequest {
    pub project_id: String,
    pub git_ref: Option<String>,
    pub path: String,
    pub recursive: bool,
    pub page: u32,
    pub per_page: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitsRequest {
    pub project_id: String,
    pub ref_name: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub page: u32,
    pub per_page: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRequest {
    pub project_id: String,
    pub sha: Option<String>,
    pub path: String,
    pub format: String,
    pub include_lfs_blobs: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitShowRequest {
    pub project_id: String,
    pub sha: String,
    pub with_stats: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchesRequest {
    pub project_id: String,
    pub sort: Option<String>,
    pub page: u32,
    pub per_page: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoRead {
    Tree(TreeRequest),
    Commits(CommitsRequest),
    Archive(ArchiveRequest),
    Commit(CommitShowRequest),
    Branches(BranchesRequest),
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct TreeEntryBody {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub path: String,
    pub mode: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct CommitListItem {
    pub id: String,
    pub short_id: String,
    pub title: String,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub authored_date: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committed_date: String,
    pub created_at: String,
    pub parent_ids: Vec<String>,
    pub trailers: BTreeMap<String, String>,
    pub extended_trailers: BTreeMap<String, Vec<String>>,
    pub web_url: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct CommitStatsBody {
    pub additions: i32,
    pub deletions: i32,
    pub total: i32,
}

#[derive(Debug, Serialize, Clone)]
pub struct CommitShowBody {
    #[serde(flatten)]
    pub commit: CommitListItem,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<CommitStatsBody>,
    pub status: Option<String>,
    pub project_id: i64,
    pub last_pipeline: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Clone)]
pub struct BranchBody {
    pub name: String,
    pub commit: CommitListItem,
    pub merged: bool,
    pub protected: bool,
    pub developers_can_push: bool,
    pub developers_can_merge: bool,
    pub can_push: bool,
    #[serde(rename = "default")]
    pub is_default: bool,
    pub web_url: String,
}

pub fn parse_repo_read(
    method: &Method,
    path: &str,
    query: Option<&str>,
) -> Result<RepoRead, String> {
    if method != Method::GET {
        return Err("method not accelerated".to_string());
    }
    let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    let prefix = "/api/v4/projects/";
    let rest = path
        .strip_prefix(prefix)
        .ok_or_else(|| "not a repository path".to_string())?;
    if let Some(project_id) = rest.strip_suffix("/repository/tree") {
        if project_id.is_empty() || project_id.contains("/repository/") {
            return Err("not a tree path".to_string());
        }
        return Ok(RepoRead::Tree(parse_tree_query(percent_decode(project_id), query)?));
    }
    if let Some((project_id, sha)) = split_commit_show(rest) {
        return Ok(RepoRead::Commit(parse_commit_show_query(project_id, sha, query)?));
    }
    if let Some(project_id) = rest.strip_suffix("/repository/commits") {
        if project_id.is_empty() || project_id.contains("/repository/") {
            return Err("not a commits list path".to_string());
        }
        return Ok(RepoRead::Commits(parse_commits_query(
            percent_decode(project_id),
            query,
        )?));
    }
    if let Some((project_id, path_format)) = split_archive_rest(rest) {
        return Ok(RepoRead::Archive(parse_archive_query(
            project_id,
            path_format,
            query,
        )?));
    }
    if let Some(project_id) = rest.strip_suffix("/repository/branches") {
        if project_id.is_empty() || project_id.contains("/repository/") {
            return Err("not a branches path".to_string());
        }
        return Ok(RepoRead::Branches(parse_branches_query(
            percent_decode(project_id),
            query,
        )?));
    }
    Err("not a tree/commits/archive/commit/branches path".to_string())
}

fn parse_tree_query(project_id: String, query: Option<&str>) -> Result<TreeRequest, String> {
    let mut parsed = TreeRequest {
        project_id,
        git_ref: None,
        path: String::new(),
        recursive: false,
        page: 1,
        per_page: DEFAULT_PER_PAGE,
    };
    for (key, value) in form_pairs(query) {
        match key.as_str() {
            "ref" => {
                if !value.is_empty() {
                    parsed.git_ref = Some(value);
                }
            }
            "path" => parsed.path = value,
            "recursive" => parsed.recursive = parse_flag(&value, "recursive")?,
            "page" => {
                if !value.is_empty() {
                    parsed.page = parse_page(&value)?;
                }
            }
            "per_page" => {
                if !value.is_empty() {
                    parsed.per_page = parse_per_page(&value)?;
                }
            }
            other => return Err(format!("unknown filter: {other}")),
        }
    }
    Ok(parsed)
}

fn parse_commits_query(project_id: String, query: Option<&str>) -> Result<CommitsRequest, String> {
    let mut parsed = CommitsRequest {
        project_id,
        ref_name: None,
        since: None,
        until: None,
        page: 1,
        per_page: DEFAULT_PER_PAGE,
    };
    for (key, value) in form_pairs(query) {
        match key.as_str() {
            "ref_name" => {
                if !value.is_empty() {
                    parsed.ref_name = Some(value);
                }
            }
            "since" => {
                if !value.is_empty() {
                    parsed.since = Some(value);
                }
            }
            "until" => {
                if !value.is_empty() {
                    parsed.until = Some(value);
                }
            }
            "page" => {
                if !value.is_empty() {
                    parsed.page = parse_page(&value)?;
                }
            }
            "per_page" => {
                if !value.is_empty() {
                    parsed.per_page = parse_per_page(&value)?;
                }
            }
            other => return Err(format!("unknown filter: {other}")),
        }
    }
    Ok(parsed)
}

fn parse_branches_query(project_id: String, query: Option<&str>) -> Result<BranchesRequest, String> {
    let mut parsed = BranchesRequest {
        project_id,
        sort: None,
        page: 1,
        per_page: DEFAULT_PER_PAGE,
    };
    for (key, value) in form_pairs(query) {
        match key.as_str() {
            "sort" => {
                if !value.is_empty() {
                    match value.as_str() {
                        "name_asc" | "updated_asc" | "updated_desc" => {
                            parsed.sort = Some(value);
                        }
                        other => return Err(format!("unknown sort: {other}")),
                    }
                }
            }
            "page" => {
                if !value.is_empty() {
                    parsed.page = parse_page(&value)?;
                }
            }
            "per_page" => {
                if !value.is_empty() {
                    parsed.per_page = parse_per_page(&value)?;
                }
            }
            "search" | "regex" | "page_token" => {
                if !value.is_empty() {
                    return Err(format!("unsupported filter: {key}"));
                }
            }
            other => return Err(format!("unknown filter: {other}")),
        }
    }
    Ok(parsed)
}

fn split_commit_show(rest: &str) -> Option<(String, String)> {
    let marker = "/repository/commits/";
    let idx = rest.find(marker)?;
    let project_id = &rest[..idx];
    if project_id.is_empty() || project_id.contains("/repository/") {
        return None;
    }
    let sha = &rest[idx + marker.len()..];
    if sha.is_empty() || sha.contains('/') {
        return None;
    }
    Some((percent_decode(project_id), percent_decode(sha)))
}

fn parse_commit_show_query(
    project_id: String,
    sha: String,
    query: Option<&str>,
) -> Result<CommitShowRequest, String> {
    if sha.is_empty() {
        return Err("empty commit sha".to_string());
    }
    let mut with_stats = true;
    for (key, value) in form_pairs(query) {
        match key.as_str() {
            "stats" => with_stats = parse_flag(&value, "stats")?,
            other => return Err(format!("unknown filter: {other}")),
        }
    }
    Ok(CommitShowRequest {
        project_id,
        sha,
        with_stats,
    })
}

fn split_archive_rest(rest: &str) -> Option<(String, Option<String>)> {
    let marker = "/repository/archive";
    let idx = rest.find(marker)?;
    let project_id = &rest[..idx];
    if project_id.is_empty() || project_id.contains("/repository/") {
        return None;
    }
    let after = &rest[idx + marker.len()..];
    if after.is_empty() {
        return Some((percent_decode(project_id), None));
    }
    let format = after.strip_prefix('.')?;
    if format.is_empty() || format.contains('/') {
        return None;
    }
    Some((percent_decode(project_id), Some(format.to_string())))
}

fn parse_archive_query(
    project_id: String,
    path_format: Option<String>,
    query: Option<&str>,
) -> Result<ArchiveRequest, String> {
    let mut sha = None;
    let mut path = String::new();
    let mut query_format = None;
    let mut include_lfs_blobs = true;
    for (key, value) in form_pairs(query) {
        match key.as_str() {
            "sha" => {
                if !value.is_empty() {
                    sha = Some(value);
                }
            }
            "path" => path = value,
            "format" => {
                if !value.is_empty() {
                    query_format = Some(value);
                }
            }
            "include_lfs_blobs" => {
                include_lfs_blobs = parse_flag(&value, "include_lfs_blobs")?;
            }
            other => return Err(format!("unknown filter: {other}")),
        }
    }
    let format = path_format
        .or(query_format)
        .unwrap_or_else(|| "tar.gz".to_string());
    if archive_gitaly_format(&format).is_none() {
        return Err(format!("unsupported archive format: {format}"));
    }
    Ok(ArchiveRequest {
        project_id,
        sha,
        path,
        format,
        include_lfs_blobs,
    })
}

fn form_pairs(query: Option<&str>) -> Vec<(String, String)> {
    let Some(query) = query else {
        return Vec::new();
    };
    form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

pub fn clamp_per_page(n: u32) -> u32 {
    if n == 0 {
        DEFAULT_PER_PAGE
    } else {
        n.min(MAX_PER_PAGE)
    }
}

fn parse_page(value: &str) -> Result<u32, String> {
    let n: u32 = value.parse().map_err(|_| format!("invalid page: {value}"))?;
    Ok(n.max(1))
}

fn parse_per_page(value: &str) -> Result<u32, String> {
    let n: u32 = value
        .parse()
        .map_err(|_| format!("invalid per_page: {value}"))?;
    Ok(clamp_per_page(n))
}

fn parse_flag(value: &str, name: &str) -> Result<bool, String> {
    if value.is_empty() {
        return Ok(true);
    }
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(format!("invalid {name}")),
    }
}

pub fn format_tree_mode(mode: i32) -> String {
    format!("{mode:06o}")
}

pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

pub fn tree_entry_from_gitaly(oid: &str, path: &[u8], entry_type: i32, mode: i32) -> TreeEntryBody {
    let path = String::from_utf8_lossy(path).into_owned();
    let name = path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path.as_str())
        .to_string();
    let entry_type = match entry_type {
        1 => "tree",
        3 => "commit",
        _ => "blob",
    };
    TreeEntryBody {
        id: oid.to_string(),
        name,
        entry_type: entry_type.to_string(),
        path,
        mode: format_tree_mode(mode),
    }
}

pub fn accelerate<'a>(
    state: &'a AppState,
    req: &Request<Body>,
) -> impl std::future::Future<Output = AccelResult> + Send + 'a {
    let req = super::CapturedRequest::from_http(req);
    async move {
        let started = Instant::now();
        let template = path_template_for(&req.path);
        match tokio::time::timeout(state.hotpath.timeout, accelerate_inner(state, &req)).await {
            Ok(AccelResult::Hit(resp)) => {
                let _ = state.metrics.record_hotpath(template, "hit", started.elapsed().as_secs_f64());
                AccelResult::Hit(resp)
            }
            Ok(other) => other,
            Err(_) => {
                let _ = state.metrics.record_hotpath(template, "error", started.elapsed().as_secs_f64());
                AccelResult::Fallback {
                    reason: "timeout".to_string(),
                    error: true,
                }
            }
        }
    }
}

fn path_template_for(path: &str) -> &'static str {
    if path.contains("/repository/tree") {
        TREE_PATH_TEMPLATE
    } else if path.contains("/repository/archive") {
        ARCHIVE_PATH_TEMPLATE
    } else if path.contains("/repository/commits/") {
        COMMIT_SHOW_PATH_TEMPLATE
    } else if path.contains("/repository/branches") {
        BRANCHES_PATH_TEMPLATE
    } else {
        COMMITS_PATH_TEMPLATE
    }
}

async fn accelerate_inner(state: &AppState, req: &super::CapturedRequest) -> AccelResult {
    if req.headers.get(SKIP_ACCEL_HEADER).is_some() {
        return AccelResult::Fallback {
            reason: "skip accel header".to_string(),
            error: false,
        };
    }
    let parsed = match parse_repo_read(&req.method, &req.path, req.query.as_deref()) {
        Ok(p) => p,
        Err(reason) => {
            return AccelResult::Fallback { reason, error: false };
        }
    };
    let template = match &parsed {
        RepoRead::Tree(_) => TREE_PATH_TEMPLATE,
        RepoRead::Commits(_) => COMMITS_PATH_TEMPLATE,
        RepoRead::Archive(_) => ARCHIVE_PATH_TEMPLATE,
        RepoRead::Commit(_) => COMMIT_SHOW_PATH_TEMPLATE,
        RepoRead::Branches(_) => BRANCHES_PATH_TEMPLATE,
    };

    let Some(database_url) = state.hotpath.database_url.as_deref() else {
        return record_error(state, template, "postgres not configured");
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
    let project_id = match &parsed {
        RepoRead::Tree(t) => t.project_id.as_str(),
        RepoRead::Commits(c) => c.project_id.as_str(),
        RepoRead::Archive(a) => a.project_id.as_str(),
        RepoRead::Commit(c) => c.project_id.as_str(),
        RepoRead::Branches(b) => b.project_id.as_str(),
    };
    let project = match load_project(database_url, project_id).await {
        Ok(Some(p)) => p,
        Ok(None) => return not_found_hit(),
        Err(e) => return record_error(state, template, &format!("postgres: {e}")),
    };
    if acl::repository_disabled(project.repository_access_level) {
        return not_found_hit();
    }
    let access = match identity.user_id {
        Some(_) if identity.admin => Some(acl::ACCESS_OWNER),
        Some(user_id) => match load_access_level(database_url, user_id, project.id).await {
            Ok(level) => level,
            Err(e) => return record_error(state, template, &format!("postgres acl: {e}")),
        },
        None => None,
    };
    if !acl::can_read_code_for(
        project.visibility_level,
        project.repository_access_level,
        access,
        identity.logged_in(),
        identity.external,
    ) {
        return not_found_hit();
    }

    match parsed {
        RepoRead::Tree(tree) => fetch_tree(state, &project, tree).await,
        RepoRead::Commits(commits) => fetch_commits(state, &project, commits).await,
        RepoRead::Archive(archive) => fetch_archive(state, &project, archive).await,
        RepoRead::Commit(commit) => fetch_commit_show(state, &project, commit).await,
        RepoRead::Branches(branches) => {
            fetch_branches(state, &project, branches, &identity, access).await
        }
    }
}

fn record_error(state: &AppState, template: &str, reason: &str) -> AccelResult {
    let _ = state.metrics.record_hotpath(template, "error", 0.0);
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

fn file_not_found_hit() -> AccelResult {
    AccelResult::Hit(
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            FILE_NOT_FOUND_BODY,
        )
            .into_response(),
    )
}

fn commit_not_found_hit() -> AccelResult {
    AccelResult::Hit(
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            COMMIT_NOT_FOUND_BODY,
        )
            .into_response(),
    )
}

struct ProjectRow {
    id: i64,
    visibility_level: i32,
    repository_storage: String,
    disk_path: String,
    full_path: String,
    default_branch: String,
    repository_access_level: Option<i32>,
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

fn pg_err(e: tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => db.message().to_string(),
        None => e.to_string(),
    }
}

async fn load_project(connstr: &str, project_id: &str) -> Result<Option<ProjectRow>, String> {
    let client = pg_client(connstr).await?;
    let row = if project_id.bytes().all(|b| b.is_ascii_digit()) {
        let id = project_id.parse::<i64>().map_err(|e| e.to_string())?;
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
                &[&id],
            )
            .await
            .map_err(pg_err)?
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
            .map_err(pg_err)?
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
    ProjectRow {
        id,
        visibility_level: pg_i32(row, 1),
        repository_storage: row
            .try_get::<_, String>(2)
            .unwrap_or_else(|_| "default".to_string()),
        disk_path: row
            .try_get::<_, Option<String>>(3)
            .ok()
            .flatten()
            .unwrap_or_else(|| acl::hashed_disk_path(id)),
        full_path: row
            .try_get::<_, Option<String>>(4)
            .ok()
            .flatten()
            .unwrap_or_else(|| format!("project/{id}")),
        default_branch: String::new(),
        repository_access_level: pg_opt_i32(row, 5),
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
            &[&user_id, &project_id],
        )
        .await
        .map_err(pg_err)?;
    Ok(row.map(|r| pg_i32(&r, 0)))
}

pub(super) fn gitaly_relative_path(disk_path: &str) -> String {
    if disk_path.ends_with(".git") {
        disk_path.to_string()
    } else {
        format!("{disk_path}.git")
    }
}

fn repo_info(project: &ProjectRow) -> RepoInfo {
    RepoInfo::new(
        &project.repository_storage,
        &gitaly_relative_path(&project.disk_path),
        &project.full_path,
        &format!("project-{}", project.id),
    )
}

pub(super) fn resolve_gitaly_endpoint(state: &AppState) -> Option<(String, String)> {
    if let Some(addr) = state.git.gitaly_address.as_deref().filter(|s| !s.is_empty()) {
        return Some((
            addr.to_string(),
            state.git.gitaly_token.clone().unwrap_or_default(),
        ));
    }
    for path in [
        "/var/opt/gitlab/gitlab-rails/etc/gitlab.yml",
        "/opt/gitlab/embedded/service/gitlab-rails/config/gitlab.yml",
    ] {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(addr) = gitaly_address_from_yml(&text) {
                return Some((addr, String::new()));
            }
        }
    }
    if std::path::Path::new("/var/opt/gitlab/gitaly/gitaly.socket").exists() {
        return Some((
            "unix:/var/opt/gitlab/gitaly/gitaly.socket".to_string(),
            String::new(),
        ));
    }
    None
}

pub fn gitaly_address_from_yml(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("gitaly_address:") {
            let v = rest.trim().trim_matches('"').trim_matches('\'').trim();
            if !v.is_empty() && !v.starts_with('{') {
                return Some(v.to_string());
            }
        }
        if let Some(idx) = trimmed.find("gitaly_address") {
            let rest = &trimmed[idx + "gitaly_address".len()..];
            let rest = rest.trim_start_matches(|c: char| c == '"' || c == '\'' || c == ' ' || c == ':');
            let val: String = rest
                .trim_start_matches('"')
                .chars()
                .take_while(|c| *c != '"' && *c != '\'' && *c != '}' && *c != ',')
                .collect();
            if val.starts_with("unix:") || val.contains(':') {
                return Some(val);
            }
        }
    }
    None
}

async fn connect_gitaly(state: &AppState) -> Result<GitalyClient, String> {
    let (gitaly_addr, gitaly_token) =
        resolve_gitaly_endpoint(state).ok_or_else(|| "gitaly not configured".to_string())?;
    let server = GitalyServer {
        address: gitaly_addr,
        token: gitaly_token,
        call_metadata: Default::default(),
    };
    GitalyClient::connect(&server)
        .await
        .map_err(|e| format!("gitaly connect: {e}"))
}

fn resolve_ref(explicit: Option<&str>, default_branch: &str) -> String {
    explicit
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            if default_branch.is_empty() {
                None
            } else {
                Some(default_branch.to_string())
            }
        })
        .unwrap_or_else(|| "HEAD".to_string())
}

async fn fetch_tree(state: &AppState, project: &ProjectRow, tree: TreeRequest) -> AccelResult {
    let mut client = match connect_gitaly(state).await {
        Ok(c) => c,
        Err(e) => return record_error(state, TREE_PATH_TEMPLATE, &e),
    };
    let repo = repo_info(project);
    let git_ref = resolve_ref(tree.git_ref.as_deref(), &project.default_branch);
    let gitaly_path = if tree.path.is_empty() {
        "."
    } else {
        tree.path.as_str()
    };
    let entries = match client
        .get_tree_entries(&repo, &git_ref, gitaly_path, tree.recursive)
        .await
    {
        Ok(entries) => entries,
        Err(status) if status.code() == tonic::Code::NotFound => return not_found_hit(),
        Err(e) => return record_error(state, TREE_PATH_TEMPLATE, &format!("gitaly tree: {e}")),
    };
    if entries.len() > MAX_TREE_ENTRIES {
        return AccelResult::Fallback {
            reason: "tree too large".to_string(),
            error: false,
        };
    }
    let bodies: Vec<TreeEntryBody> = entries
        .iter()
        .map(|e| tree_entry_from_gitaly(&e.oid, &e.path, e.r#type, e.mode))
        .collect();
    let page = paginate(&bodies, tree.page, tree.per_page);
    AccelResult::Hit(json_page(page.items, tree.page, tree.per_page, page.total))
}

async fn fetch_commits(state: &AppState, project: &ProjectRow, commits: CommitsRequest) -> AccelResult {
    let after = match commits.since.as_deref().map(parse_iso_timestamp).transpose() {
        Ok(v) => v,
        Err(reason) => {
            return AccelResult::Fallback { reason, error: false };
        }
    };
    let before = match commits.until.as_deref().map(parse_iso_timestamp).transpose() {
        Ok(v) => v,
        Err(reason) => {
            return AccelResult::Fallback { reason, error: false };
        }
    };
    let mut client = match connect_gitaly(state).await {
        Ok(c) => c,
        Err(e) => return record_error(state, COMMITS_PATH_TEMPLATE, &e),
    };
    let repo = repo_info(project);
    let git_ref = resolve_ref(commits.ref_name.as_deref(), &project.default_branch);
    let offset = ((commits.page - 1) * commits.per_page) as i32;
    let rows = match client
        .find_commits(
            &repo,
            &git_ref,
            commits.per_page as i32,
            offset,
            after,
            before,
        )
        .await
    {
        Ok(rows) => rows,
        Err(status) if status.code() == tonic::Code::NotFound => return not_found_hit(),
        Err(e) => {
            return record_error(state, COMMITS_PATH_TEMPLATE, &format!("gitaly commits: {e}"))
        }
    };
    let base = gitlab_base_url();
    let items: Vec<CommitListItem> = rows
        .iter()
        .map(|c| commit_list_item(c, &base, &project.full_path))
        .collect();
    let total_hint = if items.len() as u32 == commits.per_page {
        None
    } else {
        Some((offset as u32) + items.len() as u32)
    };
    AccelResult::Hit(json_commits(items, commits.page, commits.per_page, total_hint))
}

async fn fetch_commit_show(
    state: &AppState,
    project: &ProjectRow,
    req: CommitShowRequest,
) -> AccelResult {
    let mut client = match connect_gitaly(state).await {
        Ok(c) => c,
        Err(e) => return record_error(state, COMMIT_SHOW_PATH_TEMPLATE, &e),
    };
    let repo = repo_info(project);
    let commit = match client.find_commit_with_stats(&repo, &req.sha).await {
        Ok(Some(commit)) => commit,
        Ok(None) => return commit_not_found_hit(),
        Err(status)
            if status.code() == tonic::Code::NotFound
                || status.code() == tonic::Code::InvalidArgument =>
        {
            return commit_not_found_hit();
        }
        Err(e) => {
            return record_error(
                state,
                COMMIT_SHOW_PATH_TEMPLATE,
                &format!("gitaly commit: {e}"),
            )
        }
    };
    if let Some(database_url) = state.hotpath.database_url.as_deref() {
        match commit_has_pipeline(database_url, project.id, &commit.id).await {
            Ok(true) => {
                return AccelResult::Fallback {
                    reason: "commit has pipeline".to_string(),
                    error: false,
                };
            }
            Ok(false) => {}
            Err(e) => {
                return record_error(
                    state,
                    COMMIT_SHOW_PATH_TEMPLATE,
                    &format!("postgres pipeline: {e}"),
                )
            }
        }
    }
    let base = gitlab_base_url();
    let mut item = commit_list_item(&commit, &base, &project.full_path);
    let (trailers, extended_trailers) = trailers_maps(&commit);
    item.trailers = trailers;
    item.extended_trailers = extended_trailers;
    let stats = if req.with_stats {
        Some(commit_stats_body(&commit))
    } else {
        None
    };
    let body = CommitShowBody {
        commit: item,
        stats,
        status: None,
        project_id: project.id,
        last_pipeline: None,
    };
    AccelResult::Hit(json_commit_show(&body))
}

async fn commit_has_pipeline(
    connstr: &str,
    project_id: i64,
    sha: &str,
) -> Result<bool, String> {
    let client = pg_client(connstr).await?;
    let row = client
        .query_opt(
            "SELECT 1 FROM p_ci_pipelines WHERE project_id = $1 AND sha = $2 LIMIT 1",
            &[&project_id, &sha],
        )
        .await
        .map_err(pg_err)?;
    Ok(row.is_some())
}

pub fn archive_gitaly_format(format: &str) -> Option<i32> {
    match format {
        "zip" => Some(0),
        "tar" => Some(1),
        "tar.gz" | "tgz" => Some(2),
        "tar.bz2" | "tbz" | "tbz2" | "tb2" => Some(3),
        _ => None,
    }
}

pub fn archive_content_type(format: &str) -> &'static str {
    match format {
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "tar.gz" | "tgz" => "application/gzip",
        "tar.bz2" | "tbz" | "tbz2" | "tb2" => "application/x-bzip2",
        _ => "application/octet-stream",
    }
}

pub fn project_path_name(full_path: &str) -> &str {
    full_path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(full_path)
}

pub fn archive_prefix(project_name: &str, git_ref: &str, commit_sha: &str, path: &str) -> String {
    let formatted_ref = git_ref.replace('/', "-");
    let mut prefix = format!("{project_name}-{formatted_ref}");
    if formatted_ref != commit_sha {
        prefix.push('-');
        prefix.push_str(commit_sha);
    }
    if !path.is_empty() {
        prefix.push('-');
        prefix.push_str(&path.replace('/', "-"));
    }
    prefix
}

pub fn parse_git_head(text: &str) -> Option<String> {
    let text = text.trim();
    if let Some(branch) = text.strip_prefix("ref: refs/heads/") {
        let branch = branch.trim();
        if !branch.is_empty() {
            return Some(branch.to_string());
        }
    }
    if text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(text.to_string());
    }
    None
}

fn default_branch_from_disk(_storage: &str, disk_path: &str) -> Option<String> {
    let rel = gitaly_relative_path(disk_path);
    let path = format!("/var/opt/gitlab/git-data/repositories/{rel}/HEAD");
    let text = std::fs::read_to_string(path).ok()?;
    parse_git_head(&text)
}

fn content_disposition(filename: &str) -> HeaderValue {
    let safe: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_control() || matches!(c, '"' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();
    HeaderValue::from_str(&format!("attachment; filename=\"{safe}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

async fn fetch_archive(
    state: &AppState,
    project: &ProjectRow,
    archive: ArchiveRequest,
) -> AccelResult {
    let git_ref = match archive.sha.as_deref().filter(|s| !s.is_empty()) {
        Some(sha) => sha.to_string(),
        None => match default_branch_from_disk(&project.repository_storage, &project.disk_path) {
            Some(branch) => branch,
            None => {
                return AccelResult::Fallback {
                    reason: "default branch unknown".to_string(),
                    error: false,
                };
            }
        },
    };
    let mut client = match connect_gitaly(state).await {
        Ok(c) => c,
        Err(e) => return record_error(state, ARCHIVE_PATH_TEMPLATE, &e),
    };
    let repo = repo_info(project);
    let commit_id = match client.find_commit_id(&repo, &git_ref).await {
        Ok(Some(id)) => id,
        Ok(None) => return file_not_found_hit(),
        Err(status) if status.code() == tonic::Code::NotFound => return file_not_found_hit(),
        Err(e) => {
            return record_error(state, ARCHIVE_PATH_TEMPLATE, &format!("gitaly commit: {e}"))
        }
    };
    let Some(gitaly_format) = archive_gitaly_format(&archive.format) else {
        return AccelResult::Fallback {
            reason: format!("unsupported archive format: {}", archive.format),
            error: false,
        };
    };
    let prefix = archive_prefix(
        project_path_name(&project.full_path),
        &git_ref,
        &commit_id,
        &archive.path,
    );
    let path_bytes = if archive.path.is_empty() {
        Vec::new()
    } else {
        archive.path.as_bytes().to_vec()
    };
    let stream = match client
        .stream_archive(
            &repo,
            &commit_id,
            &prefix,
            gitaly_format,
            &path_bytes,
            archive.include_lfs_blobs,
        )
        .await
    {
        Ok(s) => s,
        Err(status) if status.code() == tonic::Code::NotFound => return file_not_found_hit(),
        Err(e) => {
            return record_error(state, ARCHIVE_PATH_TEMPLATE, &format!("gitaly archive: {e}"))
        }
    };
    let filename = format!("{prefix}.{}", archive.format);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(archive_content_type(&archive.format)),
    );
    headers.insert(header::CONTENT_DISPOSITION, content_disposition(&filename));
    headers.insert(
        "content-transfer-encoding",
        HeaderValue::from_static("binary"),
    );
    let body = Body::from_stream(stream);
    let mut resp = (StatusCode::OK, headers, body).into_response();
    resp.extensions_mut()
        .insert(crate::compression::SkipGitPackCompression);
    AccelResult::Hit(resp)
}

async fn fetch_branches(
    state: &AppState,
    project: &ProjectRow,
    req: BranchesRequest,
    identity: &auth::Identity,
    access: Option<i32>,
) -> AccelResult {
    let sort_by = match req.sort.as_deref() {
        None | Some("") | Some("name_asc") => 0,
        Some("updated_asc") => 1,
        Some("updated_desc") => 2,
        Some(other) => {
            return AccelResult::Fallback {
                reason: format!("unknown sort: {other}"),
                error: false,
            };
        }
    };
    let Some(database_url) = state.hotpath.database_url.as_deref() else {
        return record_error(state, BRANCHES_PATH_TEMPLATE, "postgres not configured");
    };
    let protected_rules = match load_protected_rules(database_url, project.id).await {
        Ok(rules) => rules,
        Err(e) => {
            return record_error(
                state,
                BRANCHES_PATH_TEMPLATE,
                &format!("postgres protected: {e}"),
            )
        }
    };
    let mut client = match connect_gitaly(state).await {
        Ok(c) => c,
        Err(e) => return record_error(state, BRANCHES_PATH_TEMPLATE, &e),
    };
    let repo = repo_info(project);
    let rows = match client.find_local_branches(&repo, sort_by).await {
        Ok(rows) => rows,
        Err(status) if status.code() == tonic::Code::NotFound => return not_found_hit(),
        Err(e) => {
            return record_error(
                state,
                BRANCHES_PATH_TEMPLATE,
                &format!("gitaly branches: {e}"),
            )
        }
    };
    if rows.len() > MAX_BRANCHES {
        return AccelResult::Fallback {
            reason: "too many branches".to_string(),
            error: false,
        };
    }
    let default_branch = match client.find_default_branch_name(&repo).await {
        Ok(name) => name,
        Err(e) => {
            return record_error(
                state,
                BRANCHES_PATH_TEMPLATE,
                &format!("gitaly default branch: {e}"),
            )
        }
    };
    let page = paginate(&rows, req.page, req.per_page);
    let page_names: Vec<String> = page.items.iter().map(|(name, _)| name.clone()).collect();
    let default_sha = rows
        .iter()
        .find(|(name, _)| name == &default_branch)
        .map(|(_, commit)| commit.id.clone());
    let merged_names = if page_names.iter().all(|name| name == &default_branch) {
        HashSet::new()
    } else {
        match client.find_merged_branches(&repo, &page_names).await {
            Ok(merged) => merged
                .into_iter()
                .filter(|(_, sha)| default_sha.as_ref() != Some(sha))
                .map(|(name, _)| name)
                .collect(),
            Err(e) => {
                return record_error(
                    state,
                    BRANCHES_PATH_TEMPLATE,
                    &format!("gitaly merged branches: {e}"),
                )
            }
        }
    };
    let base = gitlab_base_url();
    let items: Vec<BranchBody> = page
        .items
        .iter()
        .map(|(name, commit)| {
            let is_default = name == &default_branch;
            let matched = matching_protected(&protected_rules, name);
            let is_protected = !matched.is_empty();
            let developers_can_push = matched.iter().any(|rule| {
                rule.push_levels.iter().any(|level| *level == ACCESS_DEVELOPER)
            });
            let developers_can_merge = matched.iter().any(|rule| {
                rule.merge_levels.iter().any(|level| *level == ACCESS_DEVELOPER)
            });
            BranchBody {
                name: name.clone(),
                commit: commit_list_item(commit, &base, &project.full_path),
                merged: merged_names.contains(name),
                protected: is_protected,
                developers_can_push,
                developers_can_merge,
                can_push: can_push_to_branch(identity, access, is_protected, &matched),
                is_default,
                web_url: branch_web_url(&base, &project.full_path, name),
            }
        })
        .collect();
    AccelResult::Hit(json_branches(items, req.page, req.per_page, page.total))
}

struct ProtectedRule {
    name: String,
    push_levels: Vec<i32>,
    merge_levels: Vec<i32>,
}

async fn load_protected_rules(
    connstr: &str,
    project_id: i64,
) -> Result<Vec<ProtectedRule>, String> {
    let client = pg_client(connstr).await?;
    let rows = client
        .query(
            "SELECT pb.name, pal.access_level, mal.access_level
             FROM protected_branches pb
             LEFT JOIN protected_branch_push_access_levels pal
               ON pal.protected_branch_id = pb.id
             LEFT JOIN protected_branch_merge_access_levels mal
               ON mal.protected_branch_id = pb.id
             WHERE pb.project_id = $1
                OR pb.namespace_id = (SELECT namespace_id FROM projects WHERE id = $1)",
            &[&project_id],
        )
        .await
        .map_err(pg_err)?;
    let mut by_name: HashMap<String, ProtectedRule> = HashMap::new();
    for row in rows {
        let name: String = row.try_get(0).unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let push_level = pg_opt_i32(&row, 1);
        let merge_level = pg_opt_i32(&row, 2);
        let rule = by_name.entry(name.clone()).or_insert(ProtectedRule {
            name,
            push_levels: Vec::new(),
            merge_levels: Vec::new(),
        });
        if let Some(level) = push_level {
            if !rule.push_levels.contains(&level) {
                rule.push_levels.push(level);
            }
        }
        if let Some(level) = merge_level {
            if !rule.merge_levels.contains(&level) {
                rule.merge_levels.push(level);
            }
        }
    }
    Ok(by_name.into_values().collect())
}

fn matching_protected<'a>(rules: &'a [ProtectedRule], branch: &str) -> Vec<&'a ProtectedRule> {
    rules
        .iter()
        .filter(|rule| protected_ref_matches(&rule.name, branch))
        .collect()
}

pub fn protected_ref_matches(pattern: &str, name: &str) -> bool {
    if pattern == name {
        return true;
    }
    if !pattern.contains('*') {
        return false;
    }
    glob_match(pattern, name)
}

fn glob_match(pattern: &str, name: &str) -> bool {
    glob_match_bytes(pattern.as_bytes(), name.as_bytes())
}

fn glob_match_bytes(pattern: &[u8], name: &[u8]) -> bool {
    if pattern.is_empty() {
        return name.is_empty();
    }
    if pattern[0] == b'*' {
        return glob_match_bytes(&pattern[1..], name)
            || (!name.is_empty() && glob_match_bytes(pattern, &name[1..]));
    }
    !name.is_empty() && pattern[0] == name[0] && glob_match_bytes(&pattern[1..], &name[1..])
}

fn can_push_to_branch(
    identity: &auth::Identity,
    access: Option<i32>,
    is_protected: bool,
    matched: &[&ProtectedRule],
) -> bool {
    if identity.user_id.is_none() {
        return false;
    }
    if identity.admin {
        return true;
    }
    let access = access.unwrap_or(0);
    if !is_protected {
        return access >= ACCESS_DEVELOPER;
    }
    matched.iter().any(|rule| {
        rule.push_levels
            .iter()
            .any(|level| *level > 0 && access >= *level)
    })
}

struct PageSlice<'a, T> {
    items: &'a [T],
    total: usize,
}

fn paginate<T>(items: &[T], page: u32, per_page: u32) -> PageSlice<'_, T> {
    let start = ((page - 1) * per_page) as usize;
    let end = (start + per_page as usize).min(items.len());
    let slice = if start >= items.len() {
        &items[items.len()..items.len()]
    } else {
        &items[start..end]
    };
    PageSlice {
        items: slice,
        total: items.len(),
    }
}

fn json_page(items: &[TreeEntryBody], page: u32, per_page: u32, total: usize) -> Response {
    let json = serde_json::to_vec(items).unwrap_or_else(|_| b"[]".to_vec());
    let mut headers = pagination_headers(page, per_page, Some(total as u32), items.len());
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    (StatusCode::OK, headers, json).into_response()
}

fn json_commits(
    items: Vec<CommitListItem>,
    page: u32,
    per_page: u32,
    total: Option<u32>,
) -> Response {
    let json = serde_json::to_vec(&items).unwrap_or_else(|_| b"[]".to_vec());
    let mut headers = pagination_headers(page, per_page, total, items.len());
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    (StatusCode::OK, headers, json).into_response()
}

fn json_branches(
    items: Vec<BranchBody>,
    page: u32,
    per_page: u32,
    total: usize,
) -> Response {
    let json = serde_json::to_vec(&items).unwrap_or_else(|_| b"[]".to_vec());
    let mut headers = pagination_headers(page, per_page, Some(total as u32), items.len());
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    (StatusCode::OK, headers, json).into_response()
}

fn json_commit_show(body: &CommitShowBody) -> Response {
    let json = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    (StatusCode::OK, headers, json).into_response()
}

fn pagination_headers(page: u32, per_page: u32, total: Option<u32>, page_len: usize) -> HeaderMap {
    let mut headers = HeaderMap::new();
    insert_str(&mut headers, "x-page", &page.to_string());
    insert_str(&mut headers, "x-per-page", &per_page.to_string());
    if let Some(total) = total {
        let total_pages = if total == 0 {
            0
        } else {
            (total + per_page - 1) / per_page
        };
        insert_str(&mut headers, "x-total", &total.to_string());
        insert_str(&mut headers, "x-total-pages", &total_pages.to_string());
        if page > 1 {
            insert_str(&mut headers, "x-prev-page", &(page - 1).to_string());
        }
        if page < total_pages {
            insert_str(&mut headers, "x-next-page", &(page + 1).to_string());
        }
    } else if page_len as u32 == per_page {
        insert_str(&mut headers, "x-next-page", &(page + 1).to_string());
        if page > 1 {
            insert_str(&mut headers, "x-prev-page", &(page - 1).to_string());
        }
    }
    headers
}

fn insert_str(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        header::HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}

fn utf8_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn commit_list_item(commit: &GitCommit, web_base: &str, full_path: &str) -> CommitListItem {
    let title = utf8_lossy(&commit.subject).trim_end_matches('\n').to_string();
    let message = if commit.body.is_empty() {
        if title.is_empty() {
            String::new()
        } else {
            format!("{title}\n")
        }
    } else {
        utf8_lossy(&commit.body)
    };
    let (author_name, author_email, authored_date) = author_fields(commit.author.as_ref());
    let (committer_name, committer_email, committed_date) = author_fields(commit.committer.as_ref());
    let web_url = commit_web_url(web_base, full_path, &commit.id);
    CommitListItem {
        short_id: short_id(&commit.id),
        id: commit.id.clone(),
        title,
        message,
        author_name,
        author_email,
        authored_date,
        committer_name,
        committer_email,
        created_at: committed_date.clone(),
        committed_date,
        parent_ids: commit.parent_ids.clone(),
        trailers: BTreeMap::new(),
        extended_trailers: BTreeMap::new(),
        web_url,
    }
}

pub fn trailers_maps(
    commit: &GitCommit,
) -> (BTreeMap<String, String>, BTreeMap<String, Vec<String>>) {
    let mut trailers = BTreeMap::new();
    let mut extended: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for trailer in &commit.trailers {
        let key = utf8_lossy(&trailer.key);
        let value = utf8_lossy(&trailer.value);
        if key.is_empty() {
            continue;
        }
        trailers.insert(key.clone(), value.clone());
        extended.entry(key).or_default().push(value);
    }
    (trailers, extended)
}

pub fn commit_stats_body(commit: &GitCommit) -> CommitStatsBody {
    let (additions, deletions) = match commit.short_stats.as_ref() {
        Some(stats) => (stats.additions, stats.deletions),
        None => (0, 0),
    };
    CommitStatsBody {
        additions,
        deletions,
        total: additions + deletions,
    }
}

fn author_fields(author: Option<&crate::gitaly::gitaly::CommitAuthor>) -> (String, String, String) {
    let Some(author) = author else {
        return (String::new(), String::new(), String::new());
    };
    (
        utf8_lossy(&author.name),
        utf8_lossy(&author.email),
        format_commit_time(author.date.as_ref(), &author.timezone),
    )
}

pub fn format_commit_time(date: Option<&prost_types::Timestamp>, timezone: &[u8]) -> String {
    let Some(date) = date else {
        return String::new();
    };
    let Some(utc) = chrono::DateTime::<Utc>::from_timestamp(date.seconds, date.nanos.max(0) as u32) else {
        return String::new();
    };
    let offset = parse_tz_offset(timezone);
    offset
        .from_utc_datetime(&utc.naive_utc())
        .format("%Y-%m-%dT%H:%M:%S%.3f%:z")
        .to_string()
}

fn parse_tz_offset(timezone: &[u8]) -> FixedOffset {
    let text = utf8_lossy(timezone);
    let text = text.trim();
    if text.is_empty() || text == "Z" || text == "UTC" {
        return FixedOffset::east_opt(0).unwrap_or(FixedOffset::east_opt(0).unwrap());
    }
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    let (hh, mm) = if digits.len() >= 4 {
        (
            digits[0..2].parse::<i32>().unwrap_or(0),
            digits[2..4].parse::<i32>().unwrap_or(0),
        )
    } else if digits.len() >= 2 {
        (digits[0..2].parse::<i32>().unwrap_or(0), 0)
    } else {
        (0, 0)
    };
    FixedOffset::east_opt(sign * (hh * 3600 + mm * 60)).unwrap_or(FixedOffset::east_opt(0).unwrap())
}

pub fn commit_web_url(web_base: &str, full_path: &str, id: &str) -> String {
    let base = web_base.trim_end_matches('/');
    if base.is_empty() {
        format!("/{full_path}/-/commit/{id}")
    } else {
        format!("{base}/{full_path}/-/commit/{id}")
    }
}

fn branch_web_url(web_base: &str, full_path: &str, name: &str) -> String {
    let base = web_base.trim_end_matches('/');
    if base.is_empty() {
        format!("/{full_path}/-/tree/{name}")
    } else {
        format!("{base}/{full_path}/-/tree/{name}")
    }
}

fn parse_iso_timestamp(value: &str) -> Result<prost_types::Timestamp, String> {
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(prost_types::Timestamp {
            seconds: parsed.timestamp(),
            nanos: parsed.timestamp_subsec_nanos() as i32,
        });
    }
    let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("invalid timestamp: {value}"))?;
    let naive = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| format!("invalid timestamp: {value}"))?;
    let parsed = Utc.from_utc_datetime(&naive);
    Ok(prost_types::Timestamp {
        seconds: parsed.timestamp(),
        nanos: 0,
    })
}

pub fn gitlab_url_from_yml(text: &str) -> Option<String> {
    let mut in_prod = false;
    let mut in_gitlab = false;
    let mut host = String::new();
    let mut https = true;
    let mut port: Option<u16> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        if indent == 0 && !line.starts_with(' ') && !line.starts_with('\t') {
            in_prod = trimmed.starts_with("production:");
            in_gitlab = false;
            continue;
        }
        if !in_prod {
            continue;
        }
        if trimmed.starts_with("gitlab:") && indent <= 2 {
            in_gitlab = true;
            continue;
        }
        if in_gitlab && indent <= 2 && trimmed.ends_with(':') && !trimmed.starts_with("host") {
            in_gitlab = trimmed.starts_with("gitlab:");
            continue;
        }
        if !in_gitlab {
            continue;
        }
        let Some((k, v)) = trimmed.split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'');
        match k.trim() {
            "host" if !v.is_empty() => host = v.to_string(),
            "https" => https = v != "false",
            "port" => port = v.parse().ok(),
            _ => {}
        }
    }
    if host.is_empty() {
        return None;
    }
    let scheme = if https { "https" } else { "http" };
    match port {
        Some(p) if !(https && p == 443) && !(!https && p == 80) => Some(format!("{scheme}://{host}:{p}")),
        _ => Some(format!("{scheme}://{host}")),
    }
}

fn gitlab_base_url() -> String {
    for key in ["GITLAB_URL", "EXTERNAL_URL"] {
        if let Ok(url) = std::env::var(key) {
            if !url.is_empty() {
                return url.trim_end_matches('/').to_string();
            }
        }
    }
    for path in [
        "/var/opt/gitlab/gitlab-rails/etc/gitlab.yml",
        "/opt/gitlab/embedded/service/gitlab-rails/config/gitlab.yml",
    ] {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(url) = gitlab_url_from_yml(&text) {
                return url;
            }
        }
    }
    String::new()
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
    fn parse_tree_path_and_whitelist() {
        let parsed = parse_repo_read(
            &Method::GET,
            "/api/v4/projects/14/repository/tree",
            Some("ref=main&path=src&recursive=true&page=2&per_page=50"),
        )
        .unwrap();
        match parsed {
            RepoRead::Tree(t) => {
                assert_eq!(t.project_id, "14");
                assert_eq!(t.git_ref.as_deref(), Some("main"));
                assert_eq!(t.path, "src");
                assert!(t.recursive);
                assert_eq!(t.page, 2);
                assert_eq!(t.per_page, 50);
            }
            _ => panic!("expected tree"),
        }
    }

    #[test]
    fn tree_unknown_filter_falls_back() {
        assert!(parse_repo_read(
            &Method::GET,
            "/api/v4/projects/14/repository/tree",
            Some("ref=main&with_last_commit=true"),
        )
        .is_err());
    }

    #[test]
    fn parse_commits_clamps_per_page() {
        let parsed = parse_repo_read(
            &Method::GET,
            "/api/v4/projects/group%2Fproj/repository/commits",
            Some("ref_name=master&page=1&per_page=500"),
        )
        .unwrap();
        match parsed {
            RepoRead::Commits(c) => {
                assert_eq!(c.project_id, "group/proj");
                assert_eq!(c.ref_name.as_deref(), Some("master"));
                assert_eq!(c.per_page, 100);
            }
            _ => panic!("expected commits"),
        }
    }

    #[test]
    fn commits_unknown_filter_falls_back() {
        assert!(parse_repo_read(
            &Method::GET,
            "/api/v4/projects/1/repository/commits",
            Some("path=README.md"),
        )
        .is_err());
        assert!(parse_repo_read(
            &Method::POST,
            "/api/v4/projects/1/repository/commits",
            None,
        )
        .is_err());
    }

    #[test]
    fn tree_json_fields_match_ce() {
        let entry = tree_entry_from_gitaly(
            "a1e8f8d745cc87e3a9248358d9352bb7f9a0aeba",
            b"files/html",
            1,
            0o40000,
        );
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["id"], "a1e8f8d745cc87e3a9248358d9352bb7f9a0aeba");
        assert_eq!(value["name"], "html");
        assert_eq!(value["type"], "tree");
        assert_eq!(value["path"], "files/html");
        assert_eq!(value["mode"], "040000");
    }

    #[test]
    fn blob_mode_is_six_octal_digits() {
        assert_eq!(format_tree_mode(0o100644), "100644");
        assert_eq!(format_tree_mode(0o100755), "100755");
    }

    #[test]
    fn pagination_clamp_and_short_id() {
        assert_eq!(clamp_per_page(0), 20);
        assert_eq!(clamp_per_page(20), 20);
        assert_eq!(clamp_per_page(500), 100);
        assert_eq!(short_id("d5a3ff139356ce33e37e73add446f16869741b50"), "d5a3ff13");
    }

    #[test]
    fn commit_time_uses_original_offset() {
        let ts = prost_types::Timestamp {
            seconds: 0,
            nanos: 0,
        };
        let formatted = format_commit_time(Some(&ts), b"+0300");
        assert_eq!(formatted, "1970-01-01T03:00:00.000+03:00");
    }

    #[test]
    fn gitlab_yml_builds_external_url() {
        let yml = r#"
production:
  gitlab:
    host: bak.toarujs.com
    https: true
    port: 9071
"#;
        assert_eq!(
            gitlab_url_from_yml(yml).as_deref(),
            Some("https://bak.toarujs.com:9071")
        );
    }

    #[test]
    fn commit_web_url_joins_project_path() {
        assert_eq!(
            commit_web_url("https://gitlab.example.com", "root/demo", "abc"),
            "https://gitlab.example.com/root/demo/-/commit/abc"
        );
    }

    #[test]
    fn gitaly_yml_reads_unix_socket() {
        let yml = r#"
production:
  repositories:
    storages: {"default":{"gitaly_address":"unix:/var/opt/gitlab/gitaly/gitaly.socket"}}
"#;
        assert_eq!(
            gitaly_address_from_yml(yml).as_deref(),
            Some("unix:/var/opt/gitlab/gitaly/gitaly.socket")
        );
    }

    #[test]
    fn gitaly_relative_path_appends_git_once() {
        assert_eq!(
            gitaly_relative_path("@hashed/ab/cd/abcd"),
            "@hashed/ab/cd/abcd.git"
        );
        assert_eq!(
            gitaly_relative_path("@hashed/ab/cd/abcd.git"),
            "@hashed/ab/cd/abcd.git"
        );
    }

    #[test]
    fn parse_archive_defaults_to_tar_gz() {
        let parsed = parse_repo_read(
            &Method::GET,
            "/api/v4/projects/14/repository/archive",
            None,
        )
        .unwrap();
        match parsed {
            RepoRead::Archive(a) => {
                assert_eq!(a.project_id, "14");
                assert_eq!(a.format, "tar.gz");
                assert!(a.include_lfs_blobs);
                assert!(a.sha.is_none());
                assert!(a.path.is_empty());
            }
            _ => panic!("expected archive"),
        }
    }

    #[test]
    fn parse_archive_path_format_and_whitelist() {
        let parsed = parse_repo_read(
            &Method::GET,
            "/api/v4/projects/group%2Fproj/repository/archive.zip",
            Some("sha=main&path=src/lib&include_lfs_blobs=false"),
        )
        .unwrap();
        match parsed {
            RepoRead::Archive(a) => {
                assert_eq!(a.project_id, "group/proj");
                assert_eq!(a.format, "zip");
                assert_eq!(a.sha.as_deref(), Some("main"));
                assert_eq!(a.path, "src/lib");
                assert!(!a.include_lfs_blobs);
            }
            _ => panic!("expected archive"),
        }
    }

    #[test]
    fn archive_unknown_filter_or_format_falls_back() {
        assert!(parse_repo_read(
            &Method::GET,
            "/api/v4/projects/1/repository/archive",
            Some("ref_type=heads"),
        )
        .is_err());
        assert!(parse_repo_read(
            &Method::GET,
            "/api/v4/projects/1/repository/archive.tar.xz",
            None,
        )
        .is_err());
        assert!(parse_repo_read(
            &Method::GET,
            "/api/v4/projects/1/repository/archive",
            Some("format=7z"),
        )
        .is_err());
    }

    #[test]
    fn archive_prefix_appends_sha_unless_ref_is_sha() {
        let sha = "d5a3ff139356ce33e37e73add446f16869741b50";
        assert_eq!(
            archive_prefix("seed-vc-pro", "main", sha, ""),
            format!("seed-vc-pro-main-{sha}")
        );
        assert_eq!(
            archive_prefix("seed-vc-pro", sha, sha, "src/lib"),
            format!("seed-vc-pro-{sha}-src-lib")
        );
        assert_eq!(
            archive_prefix("demo", "feature/foo", sha, ""),
            format!("demo-feature-foo-{sha}")
        );
    }

    #[test]
    fn parse_git_head_branch_and_detached() {
        assert_eq!(
            parse_git_head("ref: refs/heads/main\n").as_deref(),
            Some("main")
        );
        assert_eq!(
            parse_git_head("d5a3ff139356ce33e37e73add446f16869741b50\n").as_deref(),
            Some("d5a3ff139356ce33e37e73add446f16869741b50")
        );
        assert!(parse_git_head("ref: refs/tags/v1.0").is_none());
    }

    #[test]
    fn archive_content_type_matches_send_data() {
        assert_eq!(archive_content_type("zip"), "application/zip");
        assert_eq!(archive_content_type("tar.gz"), "application/gzip");
        assert_eq!(archive_content_type("tgz"), "application/gzip");
        assert_eq!(archive_gitaly_format("tar.gz"), Some(2));
        assert_eq!(archive_gitaly_format("tbz2"), Some(3));
        assert_eq!(project_path_name("toaru/seed-vc-pro"), "seed-vc-pro");
    }

    #[test]
    fn parse_commit_show_path_and_stats_flag() {
        let parsed = parse_repo_read(
            &Method::GET,
            "/api/v4/projects/14/repository/commits/HEAD",
            Some("stats=false"),
        )
        .unwrap();
        match parsed {
            RepoRead::Commit(c) => {
                assert_eq!(c.project_id, "14");
                assert_eq!(c.sha, "HEAD");
                assert!(!c.with_stats);
            }
            _ => panic!("expected commit show"),
        }
    }

    #[test]
    fn parse_commit_show_decodes_sha() {
        let parsed = parse_repo_read(
            &Method::GET,
            "/api/v4/projects/group%2Fproj/repository/commits/feature%2Ffoo",
            None,
        )
        .unwrap();
        match parsed {
            RepoRead::Commit(c) => {
                assert_eq!(c.project_id, "group/proj");
                assert_eq!(c.sha, "feature/foo");
                assert!(c.with_stats);
            }
            _ => panic!("expected commit show"),
        }
    }

    #[test]
    fn commit_show_unknown_filter_or_nested_path_falls_back() {
        assert!(parse_repo_read(
            &Method::GET,
            "/api/v4/projects/1/repository/commits/abc/diff",
            None,
        )
        .is_err());
        assert!(parse_repo_read(
            &Method::GET,
            "/api/v4/projects/1/repository/commits/abc",
            Some("stats=true&with_stats=1"),
        )
        .is_err());
    }

    #[test]
    fn commit_show_json_includes_stats_and_project_id() {
        let item = CommitListItem {
            id: "d5a3ff139356ce33e37e73add446f16869741b50".to_string(),
            short_id: "d5a3ff13".to_string(),
            title: "fix".to_string(),
            message: "fix\n".to_string(),
            author_name: "a".to_string(),
            author_email: "a@b.c".to_string(),
            authored_date: "2026-01-01T00:00:00+00:00".to_string(),
            committer_name: "a".to_string(),
            committer_email: "a@b.c".to_string(),
            committed_date: "2026-01-01T00:00:00+00:00".to_string(),
            created_at: "2026-01-01T00:00:00+00:00".to_string(),
            parent_ids: vec![],
            trailers: BTreeMap::new(),
            extended_trailers: BTreeMap::new(),
            web_url: "https://example/x/-/commit/d5".to_string(),
        };
        let body = CommitShowBody {
            commit: item,
            stats: Some(CommitStatsBody {
                additions: 93,
                deletions: 0,
                total: 93,
            }),
            status: None,
            project_id: 2,
            last_pipeline: None,
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["id"], "d5a3ff139356ce33e37e73add446f16869741b50");
        assert_eq!(value["project_id"], 2);
        assert_eq!(value["stats"]["additions"], 93);
        assert_eq!(value["stats"]["total"], 93);
        assert!(value["status"].is_null());
        assert!(value["last_pipeline"].is_null());
    }

    #[test]
    fn trailers_last_value_wins_and_extended_keeps_all() {
        let commit = GitCommit {
            trailers: vec![
                crate::gitaly::gitaly::CommitTrailer {
                    key: b"Signed-off-by".to_vec(),
                    value: b"A <a@b>".to_vec(),
                },
                crate::gitaly::gitaly::CommitTrailer {
                    key: b"Signed-off-by".to_vec(),
                    value: b"C <c@d>".to_vec(),
                },
            ],
            ..Default::default()
        };
        let (trailers, extended) = trailers_maps(&commit);
        assert_eq!(trailers.get("Signed-off-by").map(String::as_str), Some("C <c@d>"));
        assert_eq!(
            extended.get("Signed-off-by").map(Vec::as_slice),
            Some(["A <a@b>".to_string(), "C <c@d>".to_string()].as_slice())
        );
    }

    #[test]
    fn parse_branches_path_and_sort() {
        let parsed = parse_repo_read(
            &Method::GET,
            "/api/v4/projects/13/repository/branches",
            Some("page=1&per_page=20&sort=name_asc"),
        )
        .unwrap();
        match parsed {
            RepoRead::Branches(b) => {
                assert_eq!(b.project_id, "13");
                assert_eq!(b.sort.as_deref(), Some("name_asc"));
                assert_eq!(b.page, 1);
                assert_eq!(b.per_page, 20);
            }
            _ => panic!("expected branches"),
        }
    }

    #[test]
    fn branches_unknown_or_nested_falls_back() {
        assert!(parse_repo_read(
            &Method::GET,
            "/api/v4/projects/13/repository/branches",
            Some("search=mas"),
        )
        .is_err());
        assert!(parse_repo_read(
            &Method::GET,
            "/api/v4/projects/13/repository/branches",
            Some("regex=^mas"),
        )
        .is_err());
        assert!(parse_repo_read(
            &Method::GET,
            "/api/v4/projects/13/repository/branches/master",
            None,
        )
        .is_err());
        assert!(parse_repo_read(
            &Method::POST,
            "/api/v4/projects/13/repository/branches",
            None,
        )
        .is_err());
    }

    #[test]
    fn protected_ref_glob_matches_wildcard() {
        assert!(protected_ref_matches("master", "master"));
        assert!(!protected_ref_matches("master", "main"));
        assert!(protected_ref_matches("*", "master"));
        assert!(protected_ref_matches("release/*", "release/1.0"));
        assert!(!protected_ref_matches("release/*", "master"));
        assert!(protected_ref_matches("*-stable", "foo-stable"));
        assert!(!protected_ref_matches("*-stable", "foo-main"));
    }

    #[test]
    fn branch_json_fields_match_ce() {
        let body = BranchBody {
            name: "master".to_string(),
            commit: CommitListItem {
                id: "abc".to_string(),
                short_id: "abc".to_string(),
                title: "t".to_string(),
                message: "t\n".to_string(),
                author_name: "a".to_string(),
                author_email: "a@b.c".to_string(),
                authored_date: "2026-01-01T00:00:00.000+00:00".to_string(),
                committer_name: "a".to_string(),
                committer_email: "a@b.c".to_string(),
                committed_date: "2026-01-01T00:00:00.000+00:00".to_string(),
                created_at: "2026-01-01T00:00:00.000+00:00".to_string(),
                parent_ids: vec![],
                trailers: BTreeMap::new(),
                extended_trailers: BTreeMap::new(),
                web_url: "https://example/x/-/commit/abc".to_string(),
            },
            merged: false,
            protected: true,
            developers_can_push: false,
            developers_can_merge: false,
            can_push: false,
            is_default: true,
            web_url: "https://example/x/-/tree/master".to_string(),
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["name"], "master");
        assert_eq!(value["merged"], false);
        assert_eq!(value["protected"], true);
        assert_eq!(value["developers_can_push"], false);
        assert_eq!(value["developers_can_merge"], false);
        assert_eq!(value["can_push"], false);
        assert_eq!(value["default"], true);
        assert_eq!(value["web_url"], "https://example/x/-/tree/master");
        assert_eq!(value["commit"]["id"], "abc");
    }
}
