//! Accelerate `GET /api/v4/projects` (index) and anonymous `GET /api/v4/projects/:id`.

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::time::{Instant, SystemTime};
use url::form_urlencoded;

use super::acl;
use super::auth;
use super::{PROJECTS_PATH_TEMPLATE, PROJECT_SHOW_PATH_TEMPLATE};
use crate::state::AppState;

const DEFAULT_PER_PAGE: u32 = 20;
const MAX_PER_PAGE: u32 = 100;
const SKIP_ACCEL_HEADER: &str = "x-gitlab-rs-skip-accel";
const PROJECT_NOT_FOUND_BODY: &str = "{\"message\":\"404 Project Not Found\"}";

#[derive(Debug)]
pub enum AccelResult {
    Hit(Response),
    Fallback { reason: String, error: bool },
}

#[derive(Debug)]
pub enum ProjectsPrecheck {
    Ready(ProjectsQuery),
    Fallback { reason: String, error: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectsQuery {
    pub page: u32,
    pub per_page: u32,
    pub membership: bool,
    pub owned: bool,
    pub archived: Option<bool>,
    pub visibility: Option<i32>,
    pub order_by: &'static str,
    pub sort_desc: bool,
    pub min_access_level: i32,
    pub min_access_explicit: bool,
    pub simple: bool,
}

impl ProjectsQuery {
    fn default_simple() -> Self {
        Self {
            page: 1,
            per_page: DEFAULT_PER_PAGE,
            membership: false,
            owned: false,
            archived: None,
            visibility: None,
            order_by: "created_at",
            sort_desc: true,
            min_access_level: acl::ACCESS_GUEST,
            min_access_explicit: false,
            simple: false,
        }
    }
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct ProjectListItem {
    pub id: i64,
    pub description: Option<String>,
    pub name: String,
    pub name_with_namespace: String,
    pub path: String,
    pub path_with_namespace: String,
    pub created_at: Option<String>,
    pub default_branch: Option<String>,
    pub tag_list: Vec<String>,
    pub topics: Vec<String>,
    pub ssh_url_to_repo: String,
    pub http_url_to_repo: String,
    pub web_url: String,
    pub readme_url: Option<String>,
    pub forks_count: i64,
    pub avatar_url: Option<String>,
    pub star_count: i64,
    pub visibility: String,
    pub last_activity_at: Option<String>,
    pub namespace: ProjectNamespace,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct ProjectNamespace {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub kind: String,
    pub full_path: String,
    pub parent_id: Option<i64>,
    pub avatar_url: Option<String>,
    pub web_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteUrls {
    pub web_base: String,
    pub ssh_prefix: String,
}

pub fn should_fallback_full_entity(logged_in: bool, simple: bool) -> bool {
    logged_in && !simple
}

pub fn is_projects_index_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path == "/api/v4/projects" || path == "/api/v4/projects/"
}

pub fn parse_project_show_id(path: &str) -> Option<String> {
    let path = path.split('?').next().unwrap_or(path);
    let rest = path.strip_prefix("/api/v4/projects/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    let lower = rest.to_ascii_lowercase();
    if lower == "import" || lower == "import-relation" {
        return None;
    }
    Some(percent_decode(rest))
}

#[derive(Debug)]
pub enum ShowPrecheck {
    Ready { project_id: String },
    Fallback { reason: String, error: bool },
}

pub fn precheck_show(method: &Method, path: &str, query: Option<&str>) -> ShowPrecheck {
    if method != Method::GET {
        return ShowPrecheck::Fallback {
            reason: "method not accelerated".to_string(),
            error: false,
        };
    }
    let Some(project_id) = parse_project_show_id(path) else {
        return ShowPrecheck::Fallback {
            reason: "not project show".to_string(),
            error: false,
        };
    };
    if query.filter(|s| !s.is_empty()).is_some() {
        return ShowPrecheck::Fallback {
            reason: "query not accelerated".to_string(),
            error: false,
        };
    }
    ShowPrecheck::Ready { project_id }
}

pub fn visibility_name(level: i32) -> &'static str {
    if level >= acl::VISIBILITY_PUBLIC {
        "public"
    } else if level >= acl::VISIBILITY_INTERNAL {
        "internal"
    } else {
        "private"
    }
}

/// Apply list filters on top of Rails-aligned visibility.
pub fn matches_list_filters(
    visibility_level: i32,
    access_level: Option<i32>,
    archived: bool,
    query: &ProjectsQuery,
    external_user: bool,
    logged_in: bool,
) -> bool {
    if let Some(want) = query.archived {
        if archived != want {
            return false;
        }
    }
    if let Some(vis) = query.visibility {
        if visibility_level != vis {
            return false;
        }
    }
    let min_access = if query.owned {
        query.min_access_level.max(acl::ACCESS_OWNER)
    } else {
        query.min_access_level
    };
    if query.owned || query.membership || query.min_access_explicit {
        return access_level.unwrap_or(0) >= min_access;
    }
    acl::can_see_project_for(visibility_level, access_level, logged_in, external_user)
}

pub fn parse_projects_query(query: Option<&str>) -> Result<ProjectsQuery, String> {
    let mut parsed = ProjectsQuery::default_simple();
    let Some(raw) = query.filter(|s| !s.is_empty()) else {
        return Ok(parsed);
    };
    for (key, value) in form_urlencoded::parse(raw.as_bytes()) {
        match key.as_ref() {
            "page" => {
                if value.is_empty() {
                    continue;
                }
                parsed.page = parse_page(&value)?;
            }
            "per_page" => {
                if value.is_empty() {
                    continue;
                }
                parsed.per_page = parse_per_page(&value)?;
            }
            "simple" => parsed.simple = parse_flag(&value, "simple")?,
            "membership" => parsed.membership = parse_flag(&value, "membership")?,
            "owned" => parsed.owned = parse_flag(&value, "owned")?,
            "archived" => parsed.archived = Some(parse_flag(&value, "archived")?),
            "visibility" => parsed.visibility = Some(parse_visibility(&value)?),
            "order_by" => parsed.order_by = parse_order_by(&value)?,
            "sort" => parsed.sort_desc = parse_sort(&value)?,
            "min_access_level" => {
                parsed.min_access_level = parse_min_access(&value)?;
                parsed.min_access_explicit = true;
            }
            "pagination" => {
                if value.is_empty() || value.eq_ignore_ascii_case("offset") {
                    continue;
                }
                return Err(format!("pagination not accelerated: {value}"));
            }
            other => return Err(format!("unknown filter: {other}")),
        }
    }
    if parsed.owned {
        parsed.min_access_level = parsed.min_access_level.max(acl::ACCESS_OWNER);
        parsed.min_access_explicit = true;
    }
    Ok(parsed)
}

pub fn precheck(
    method: &Method,
    path: &str,
    query: Option<&str>,
) -> ProjectsPrecheck {
    if method != Method::GET {
        return ProjectsPrecheck::Fallback {
            reason: "method not accelerated".to_string(),
            error: false,
        };
    }
    if !is_projects_index_path(path) {
        return ProjectsPrecheck::Fallback {
            reason: "not projects index".to_string(),
            error: false,
        };
    }
    let parsed = match parse_projects_query(query) {
        Ok(q) => q,
        Err(reason) => {
            return ProjectsPrecheck::Fallback {
                reason,
                error: false,
            };
        }
    };
    ProjectsPrecheck::Ready(parsed)
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
                    PROJECTS_PATH_TEMPLATE,
                    "hit",
                    started.elapsed().as_secs_f64(),
                );
                AccelResult::Hit(resp)
            }
            Ok(other) => other,
            Err(_) => {
                let _ = state.metrics.record_hotpath(
                    PROJECTS_PATH_TEMPLATE,
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

pub fn accelerate_show<'a>(
    state: &'a AppState,
    req: &Request<Body>,
) -> impl std::future::Future<Output = AccelResult> + Send + 'a {
    let req = super::CapturedRequest::from_http(req);
    async move {
        let started = Instant::now();
        match tokio::time::timeout(state.hotpath.timeout, accelerate_show_inner(state, &req)).await {
            Ok(AccelResult::Hit(resp)) => {
                let _ = state.metrics.record_hotpath(
                    PROJECT_SHOW_PATH_TEMPLATE,
                    "hit",
                    started.elapsed().as_secs_f64(),
                );
                AccelResult::Hit(resp)
            }
            Ok(other) => other,
            Err(_) => {
                let _ = state.metrics.record_hotpath(
                    PROJECT_SHOW_PATH_TEMPLATE,
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
    let query = match precheck(&req.method, &req.path, req.query.as_deref()) {
        ProjectsPrecheck::Ready(q) => q,
        ProjectsPrecheck::Fallback { reason, error } => {
            return AccelResult::Fallback { reason, error };
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
    if identity.admin {
        return AccelResult::Fallback {
            reason: "admin listing not accelerated".to_string(),
            error: false,
        };
    }

    if should_fallback_full_entity(identity.user_id.is_some(), query.simple) {
        return AccelResult::Fallback {
            reason: "full project entity not accelerated".to_string(),
            error: false,
        };
    }
    let Some(urls) = site_urls() else {
        return AccelResult::Fallback {
            reason: "gitlab.yml missing".to_string(),
            error: false,
        };
    };

    match load_visible_projects(
        database_url,
        identity.user_id,
        identity.external,
        &query,
        &urls,
    )
    .await
    {
        Ok((items, total)) => AccelResult::Hit(list_response(&items, &query, total)),
        Err(e) => record_error(state, &format!("postgres: {e}")),
    }
}

async fn accelerate_show_inner(state: &AppState, req: &super::CapturedRequest) -> AccelResult {
    if req.headers.get(SKIP_ACCEL_HEADER).is_some() {
        return AccelResult::Fallback {
            reason: "skip accel header".to_string(),
            error: false,
        };
    }
    let project_id = match precheck_show(&req.method, &req.path, req.query.as_deref()) {
        ShowPrecheck::Ready { project_id } => project_id,
        ShowPrecheck::Fallback { reason, error } => {
            return AccelResult::Fallback { reason, error };
        }
    };
    let Some(database_url) = state.hotpath.database_url.as_deref() else {
        return record_show_error(state, "postgres not configured");
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
    if should_fallback_full_entity(identity.logged_in(), false) {
        return AccelResult::Fallback {
            reason: "full project entity not accelerated".to_string(),
            error: false,
        };
    }
    let Some(urls) = site_urls() else {
        return AccelResult::Fallback {
            reason: "gitlab.yml missing".to_string(),
            error: false,
        };
    };
    match load_one_project(database_url, &project_id, &urls).await {
        Ok(Some((item, visibility_level))) => {
            if !acl::can_see_project_for(
                visibility_level,
                None,
                identity.logged_in(),
                identity.external,
            ) {
                return not_found_hit();
            }
            AccelResult::Hit(show_response(&item))
        }
        Ok(None) => not_found_hit(),
        Err(e) => record_show_error(state, &format!("postgres: {e}")),
    }
}

fn record_error(state: &AppState, reason: &str) -> AccelResult {
    let _ = state.metrics.record_hotpath(PROJECTS_PATH_TEMPLATE, "error", 0.0);
    AccelResult::Fallback {
        reason: reason.to_string(),
        error: true,
    }
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

fn record_show_error(state: &AppState, reason: &str) -> AccelResult {
    let _ = state.metrics.record_hotpath(PROJECT_SHOW_PATH_TEMPLATE, "error", 0.0);
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
            PROJECT_NOT_FOUND_BODY,
        )
            .into_response(),
    )
}

async fn pg_client(connstr: &str) -> Result<super::pg::PgConn, String> {
    super::pg::get(connstr).await
}

fn visibility_predicate(
    query: &ProjectsQuery,
    logged_in: bool,
    include_internal: bool,
) -> &'static str {
    if query.owned || query.membership || query.min_access_explicit {
        if logged_in {
            "p.id IN (SELECT project_id FROM project_authorizations WHERE user_id = $1 AND access_level >= $2)"
        } else {
            "FALSE"
        }
    } else if !logged_in {
        "p.visibility_level >= 20"
    } else if include_internal {
        "(p.visibility_level >= 10 OR p.id IN (SELECT project_id FROM project_authorizations WHERE user_id = $1 AND access_level >= $2))"
    } else {
        "(p.visibility_level >= 20 OR p.id IN (SELECT project_id FROM project_authorizations WHERE user_id = $1 AND access_level >= $2))"
    }
}

fn order_sql(order_by: &str, desc: bool) -> String {
    let col = match order_by {
        "id" => "p.id",
        "name" => "p.name",
        "path" => "p.path",
        "updated_at" => "p.updated_at",
        "last_activity_at" => "p.last_activity_at",
        _ => "p.created_at",
    };
    let dir = if desc { "DESC" } else { "ASC" };
    format!("{col} {dir} NULLS LAST, p.id DESC")
}

async fn load_visible_projects(
    connstr: &str,
    user_id: Option<i64>,
    external_user: bool,
    query: &ProjectsQuery,
    urls: &SiteUrls,
) -> Result<(Vec<ProjectListItem>, u64), String> {
    let client = pg_client(connstr).await?;
    let logged_in = user_id.is_some();
    let pred = visibility_predicate(query, logged_in, logged_in && !external_user);
    let filters = if logged_in {
        "AND ($3::boolean IS NULL OR p.archived = $3)
          AND ($4::integer IS NULL OR p.visibility_level = $4)"
    } else {
        "AND ($1::boolean IS NULL OR p.archived = $1)
          AND ($2::integer IS NULL OR p.visibility_level = $2)"
    };
    let where_sql = format!(
        "NOT COALESCE(p.pending_delete, false)
          AND NOT COALESCE(p.hidden, false)
          AND {pred}
          {filters}"
    );
    let min_access = query.min_access_level;
    let archived = query.archived;
    let vis = query.visibility;
    let limit = i64::from(query.per_page);
    let offset = i64::from(query.page.saturating_sub(1).saturating_mul(query.per_page));

    let count_sql = format!(
        "SELECT COUNT(*) FROM projects p WHERE {where_sql}"
    );
    let order = order_sql(query.order_by, query.sort_desc);
    let page_sql = if logged_in {
        "LIMIT $5 OFFSET $6"
    } else {
        "LIMIT $3 OFFSET $4"
    };
    let list_sql = format!(
        "SELECT p.id, p.name, p.path,
                COALESCE(
                  (SELECT r.path FROM routes r
                    WHERE r.source_id = p.id AND r.source_type = 'Project'
                    LIMIT 1),
                  NULLIF(n.path, '') || '/' || p.path,
                  p.path
                ) AS path_with_namespace,
                COALESCE(NULLIF(n.name, '') || ' / ' || p.name, p.name) AS name_with_namespace,
                p.visibility_level, p.last_activity_at,
                p.description, p.created_at, p.star_count, p.avatar,
                pr.disk_path,
                n.id, n.name, n.path, n.type, n.parent_id, n.avatar, n.owner_id,
                u.email, u.avatar,
                COALESCE((
                  SELECT COUNT(*) FROM fork_network_members f
                  WHERE f.forked_from_project_id = p.id
                ), 0) AS forks_count,
                COALESCE((
                  SELECT ARRAY_AGG(t.name ORDER BY t.name)
                  FROM project_topics pt
                  JOIN topics t ON t.id = pt.topic_id
                  WHERE pt.project_id = p.id
                ), ARRAY[]::text[]) AS topics
         FROM projects p
         LEFT JOIN namespaces n ON n.id = p.namespace_id
         LEFT JOIN users u ON u.id = n.owner_id
         LEFT JOIN project_repositories pr ON pr.project_id = p.id
         WHERE {where_sql}
         ORDER BY {order}
          {page_sql}"
    );
    let (count_row, rows) = if let Some(uid) = user_id {
        let count_row = client
            .query_one(&count_sql, &[&uid, &min_access, &archived, &vis])
            .await
            .map_err(|e| e.to_string())?;
        let rows = client
            .query(
                &list_sql,
                &[&uid, &min_access, &archived, &vis, &limit, &offset],
            )
            .await
            .map_err(|e| e.to_string())?;
        (count_row, rows)
    } else {
        let count_row = client
            .query_one(&count_sql, &[&archived, &vis])
            .await
            .map_err(|e| e.to_string())?;
        let rows = client
            .query(&list_sql, &[&archived, &vis, &limit, &offset])
            .await
            .map_err(|e| e.to_string())?;
        (count_row, rows)
    };
    let total = pg_u64_count(&count_row);
    let items = rows.iter().map(|row| item_from_row(row, urls)).collect();
    Ok((items, total))
}

const PROJECT_ITEM_SELECT: &str = "SELECT p.id, p.name, p.path,
                COALESCE(
                  (SELECT r.path FROM routes r
                    WHERE r.source_id = p.id AND r.source_type = 'Project'
                    LIMIT 1),
                  NULLIF(n.path, '') || '/' || p.path,
                  p.path
                ) AS path_with_namespace,
                COALESCE(NULLIF(n.name, '') || ' / ' || p.name, p.name) AS name_with_namespace,
                p.visibility_level, p.last_activity_at,
                p.description, p.created_at, p.star_count, p.avatar,
                pr.disk_path,
                n.id, n.name, n.path, n.type, n.parent_id, n.avatar, n.owner_id,
                u.email, u.avatar,
                COALESCE((
                  SELECT COUNT(*) FROM fork_network_members f
                  WHERE f.forked_from_project_id = p.id
                ), 0) AS forks_count,
                COALESCE((
                  SELECT ARRAY_AGG(t.name ORDER BY t.name)
                  FROM project_topics pt
                  JOIN topics t ON t.id = pt.topic_id
                  WHERE pt.project_id = p.id
                ), ARRAY[]::text[]) AS topics
         FROM projects p
         LEFT JOIN namespaces n ON n.id = p.namespace_id
         LEFT JOIN users u ON u.id = n.owner_id
         LEFT JOIN project_repositories pr ON pr.project_id = p.id";

async fn load_one_project(
    connstr: &str,
    project_id: &str,
    urls: &SiteUrls,
) -> Result<Option<(ProjectListItem, i32)>, String> {
    let client = pg_client(connstr).await?;
    let hidden = "NOT COALESCE(p.pending_delete, false) AND NOT COALESCE(p.hidden, false)";
    let row = if project_id.bytes().all(|b| b.is_ascii_digit()) {
        let id = project_id
            .parse::<i64>()
            .map_err(|e| e.to_string())?;
        let sql = format!("{PROJECT_ITEM_SELECT} WHERE {hidden} AND p.id = $1");
        client
            .query_opt(&sql, &[&id])
            .await
            .map_err(|e| e.to_string())?
    } else {
        let sql = format!(
            "{PROJECT_ITEM_SELECT}
             WHERE {hidden}
               AND lower(COALESCE(
                     (SELECT r.path FROM routes r
                       WHERE r.source_id = p.id AND r.source_type = 'Project'
                       LIMIT 1),
                     NULLIF(n.path, '') || '/' || p.path,
                     p.path
                   )) = lower($1)"
        );
        client
            .query_opt(&sql, &[&project_id])
            .await
            .map_err(|e| e.to_string())?
    };
    Ok(row.map(|row| {
        let visibility_level = pg_i32(&row, 5);
        (item_from_row(&row, urls), visibility_level)
    }))
}

fn item_from_row(row: &tokio_postgres::Row, urls: &SiteUrls) -> ProjectListItem {
    let id = pg_i64(row, 0);
    let name: String = row.try_get::<_, String>(1).unwrap_or_default();
    let path: String = row.try_get::<_, String>(2).unwrap_or_default();
    let path_with_namespace: String = row
        .try_get::<_, Option<String>>(3)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.clone());
    let name_with_namespace: String = row
        .try_get::<_, Option<String>>(4)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| name.clone());
    let visibility_level = pg_i32(row, 5);
    let last_activity_at = pg_time(row, 6);
    let description = pg_opt_string(row, 7);
    let created_at = pg_time(row, 8);
    let star_count = i64::from(pg_i32(row, 9));
    let project_avatar = pg_opt_string_nonempty(row, 10);
    let disk_path = row
        .try_get::<_, Option<String>>(11)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| acl::hashed_disk_path(id));
    let ns_id = pg_i64(row, 12);
    let ns_name: String = row.try_get::<_, Option<String>>(13).ok().flatten().unwrap_or_default();
    let ns_path: String = row.try_get::<_, Option<String>>(14).ok().flatten().unwrap_or_default();
    let ns_type = row.try_get::<_, Option<String>>(15).ok().flatten();
    let ns_parent_id = pg_opt_i64(row, 16);
    let ns_avatar = pg_opt_string_nonempty(row, 17);
    let owner_id = pg_opt_i64(row, 18);
    let user_email = pg_opt_string_nonempty(row, 19);
    let user_avatar = pg_opt_string_nonempty(row, 20);
    let forks_count = pg_i64(row, 21);
    let topics = pg_text_array(row, 22);
    let kind = namespace_kind(ns_type.as_deref()).to_string();
    let default_branch = default_branch_from_disk(&disk_path);
    let web_base = urls.web_base.trim_end_matches('/');
    let web_url = format!("{web_base}/{path_with_namespace}");
    let http_url_to_repo = format!("{web_url}.git");
    let ssh_url_to_repo = format!("{}/{}.git", urls.ssh_prefix, path_with_namespace);
    let readme_url = default_branch.as_deref().map(|branch| {
        format!("{web_base}/{path_with_namespace}/-/blob/{branch}/README.md")
    });
    let ns_web_url = format!("{web_base}/{ns_path}");
    ProjectListItem {
        id,
        description,
        name,
        path,
        path_with_namespace,
        name_with_namespace,
        created_at,
        default_branch,
        tag_list: topics.clone(),
        topics,
        ssh_url_to_repo,
        http_url_to_repo,
        web_url,
        readme_url,
        forks_count,
        avatar_url: project_avatar_url(id, project_avatar.as_deref()),
        star_count,
        visibility: visibility_name(visibility_level).to_string(),
        last_activity_at,
        namespace: ProjectNamespace {
            id: ns_id,
            name: ns_name,
            path: ns_path.clone(),
            kind,
            full_path: ns_path,
            parent_id: ns_parent_id,
            avatar_url: namespace_avatar_url(
                ns_id,
                namespace_kind(ns_type.as_deref()),
                ns_avatar.as_deref(),
                owner_id,
                user_avatar.as_deref(),
                user_email.as_deref(),
            ),
            web_url: ns_web_url,
        },
    }
}

fn list_response(items: &[ProjectListItem], query: &ProjectsQuery, total: u64) -> Response {
    let body = serde_json::to_string(items).unwrap_or_else(|_| "[]".to_string());
    let per_page = u64::from(query.per_page.max(1));
    let page = u64::from(query.page.max(1));
    let total_pages = if total == 0 {
        0
    } else {
        (total + per_page - 1) / per_page
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    insert_num(&mut headers, "x-page", page);
    insert_num(&mut headers, "x-per-page", per_page);
    insert_num(&mut headers, "x-total", total);
    insert_num(&mut headers, "x-total-pages", total_pages);
    if page < total_pages {
        insert_num(&mut headers, "x-next-page", page + 1);
    }
    if page > 1 {
        insert_num(&mut headers, "x-prev-page", page - 1);
    }
    (StatusCode::OK, headers, body).into_response()
}

fn show_response(item: &ProjectListItem) -> Response {
    let body = serde_json::to_string(item).unwrap_or_else(|_| "{}".to_string());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

fn insert_num(headers: &mut HeaderMap, name: &'static str, value: u64) {
    if let Ok(val) = HeaderValue::from_str(&value.to_string()) {
        headers.insert(axum::http::HeaderName::from_static(name), val);
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

fn pg_i32(row: &tokio_postgres::Row, idx: usize) -> i32 {
    if let Ok(v) = row.try_get::<_, i32>(idx) {
        return v;
    }
    if let Ok(v) = row.try_get::<_, i64>(idx) {
        return v as i32;
    }
    0
}

fn pg_u64_count(row: &tokio_postgres::Row) -> u64 {
    if let Ok(v) = row.try_get::<_, i64>(0) {
        return v.max(0) as u64;
    }
    if let Ok(v) = row.try_get::<_, i32>(0) {
        return v.max(0) as u64;
    }
    0
}

fn pg_time(row: &tokio_postgres::Row, idx: usize) -> Option<String> {
    let ts = if let Ok(v) = row.try_get::<_, SystemTime>(idx) {
        Some(v)
    } else {
        row.try_get::<_, Option<SystemTime>>(idx).ok().flatten()
    }?;
    Some(
        chrono::DateTime::<chrono::Utc>::from(ts)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
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

fn parse_visibility(value: &str) -> Result<i32, String> {
    match value {
        "private" => Ok(acl::VISIBILITY_PRIVATE),
        "internal" => Ok(acl::VISIBILITY_INTERNAL),
        "public" => Ok(acl::VISIBILITY_PUBLIC),
        _ => Err(format!("unknown visibility: {value}")),
    }
}

fn parse_order_by(value: &str) -> Result<&'static str, String> {
    match value {
        "id" => Ok("id"),
        "name" => Ok("name"),
        "path" => Ok("path"),
        "created_at" => Ok("created_at"),
        "updated_at" => Ok("updated_at"),
        "last_activity_at" => Ok("last_activity_at"),
        _ => Err(format!("order_by not accelerated: {value}")),
    }
}

fn parse_sort(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "desc" => Ok(true),
        "asc" => Ok(false),
        _ => Err(format!("invalid sort: {value}")),
    }
}

fn parse_min_access(value: &str) -> Result<i32, String> {
    match value {
        "10" => Ok(10),
        "20" => Ok(20),
        "30" => Ok(30),
        "40" => Ok(40),
        "50" => Ok(50),
        _ => Err(format!("min_access_level not accelerated: {value}")),
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
    if n == 0 {
        return Ok(DEFAULT_PER_PAGE);
    }
    Ok(n.min(MAX_PER_PAGE))
}

pub const GITLAB_YML_PATHS: [&str; 2] = [
    "/var/opt/gitlab/gitlab-rails/etc/gitlab.yml",
    "/opt/gitlab/embedded/service/gitlab-rails/config/gitlab.yml",
];

pub fn site_urls() -> Option<SiteUrls> {
    for path in GITLAB_YML_PATHS {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(urls) = SiteUrls::from_yml(&text) {
                return Some(urls);
            }
        }
    }
    None
}

impl SiteUrls {
    pub fn from_yml(text: &str) -> Option<Self> {
        let web_base = super::repo::gitlab_url_from_yml(text)?;
        let web_base = web_base.trim_end_matches('/').to_string();
        let host = url::Url::parse(&web_base)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))?;
        let ssh_host = ssh_host_override(text).unwrap_or(host);
        let port = ssh_port_from_yml(text);
        let ssh_prefix = if port == 22 {
            format!("ssh://git@{ssh_host}")
        } else {
            format!("ssh://git@{ssh_host}:{port}")
        };
        Some(Self {
            web_base,
            ssh_prefix,
        })
    }
}

fn ssh_port_from_yml(text: &str) -> u16 {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("ssh_port:") {
            if let Ok(port) = rest.trim().trim_matches('"').trim_matches('\'').parse() {
                return port;
            }
        }
    }
    22
}

fn ssh_host_override(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("ssh_host:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub fn namespace_kind(ns_type: Option<&str>) -> &'static str {
    match ns_type.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("group") => "group",
        _ => "user",
    }
}

pub fn gravatar_url(email: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(email.trim().to_ascii_lowercase().as_bytes());
    format!("https://secure.gravatar.com/avatar/{digest:x}?s=80&d=identicon")
}

fn project_avatar_url(project_id: i64, avatar: Option<&str>) -> Option<String> {
    let file = avatar.filter(|s| !s.is_empty())?;
    Some(format!(
        "/uploads/-/system/project/avatar/{project_id}/{file}"
    ))
}

fn namespace_avatar_url(
    ns_id: i64,
    kind: &str,
    ns_avatar: Option<&str>,
    owner_id: Option<i64>,
    user_avatar: Option<&str>,
    email: Option<&str>,
) -> Option<String> {
    if kind == "group" {
        let file = ns_avatar.filter(|s| !s.is_empty())?;
        return Some(format!(
            "/uploads/-/system/namespace/avatar/{ns_id}/{file}"
        ));
    }
    let uid = owner_id.unwrap_or(ns_id);
    if let Some(file) = user_avatar.filter(|s| !s.is_empty()) {
        return Some(format!("/uploads/-/system/user/avatar/{uid}/{file}"));
    }
    email.filter(|s| !s.is_empty()).map(gravatar_url)
}

fn default_branch_from_disk(disk_path: &str) -> Option<String> {
    let rel = super::repo::gitaly_relative_path(disk_path);
    let path = format!("/var/opt/gitlab/git-data/repositories/{rel}/HEAD");
    let text = std::fs::read_to_string(path).ok()?;
    super::repo::parse_git_head(&text)
}

fn pg_opt_string(row: &tokio_postgres::Row, idx: usize) -> Option<String> {
    row.try_get::<_, Option<String>>(idx).ok().flatten()
}

fn pg_opt_string_nonempty(row: &tokio_postgres::Row, idx: usize) -> Option<String> {
    pg_opt_string(row, idx).filter(|s| !s.is_empty())
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

fn pg_text_array(row: &tokio_postgres::Row, idx: usize) -> Vec<String> {
    row.try_get::<_, Vec<String>>(idx).unwrap_or_default()
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
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

    fn fallback_reason(pre: ProjectsPrecheck) -> String {
        match pre {
            ProjectsPrecheck::Fallback { reason, .. } => reason,
            ProjectsPrecheck::Ready(_) => panic!("expected fallback"),
        }
    }

    #[test]
    fn anonymous_query_is_ready() {
        let pre = precheck(&Method::GET, "/api/v4/projects", None);
        match pre {
            ProjectsPrecheck::Ready(q) => {
                assert_eq!(q.page, 1);
                assert!(!q.membership);
            }
            ProjectsPrecheck::Fallback { reason, .. } => panic!("unexpected fallback: {reason}"),
        }
    }

    #[test]
    fn unknown_query_is_fallback() {
        let pre = precheck(
            &Method::GET,
            "/api/v4/projects",
            Some("search=secret&membership=true"),
        );
        let reason = fallback_reason(pre);
        assert!(reason.contains("unknown filter"), "{reason}");
    }

    #[test]
    fn starred_and_statistics_are_fallback() {
        for q in ["starred=true", "statistics=true", "order_by=star_count"] {
            let pre = precheck(&Method::GET, "/api/v4/projects", Some(q));
            assert!(
                matches!(pre, ProjectsPrecheck::Fallback { .. }),
                "{q} should fallback"
            );
        }
    }

    #[test]
    fn subpath_is_not_accelerated() {
        let pre = precheck(&Method::GET, "/api/v4/projects/14", None);
        let reason = fallback_reason(pre);
        assert!(reason.contains("not projects index"), "{reason}");
        assert!(!is_projects_index_path("/api/v4/projects/14/repository/files/a"));
        assert!(is_projects_index_path("/api/v4/projects"));
    }

    #[test]
    fn simple_membership_query_is_ready() {
        let pre = precheck(
            &Method::GET,
            "/api/v4/projects",
            Some("membership=true&simple=true&order_by=last_activity_at&sort=desc&per_page=20"),
        );
        match pre {
            ProjectsPrecheck::Ready(q) => {
                assert!(q.membership);
                assert!(q.simple);
                assert_eq!(q.order_by, "last_activity_at");
                assert!(q.sort_desc);
                assert_eq!(q.per_page, 20);
            }
            ProjectsPrecheck::Fallback { reason, .. } => panic!("unexpected fallback: {reason}"),
        }
    }

    #[test]
    fn visibility_hides_private_from_non_members() {
        let q = ProjectsQuery::default_simple();
        assert!(!matches_list_filters(
            acl::VISIBILITY_PRIVATE,
            None,
            false,
            &q,
            false,
            true
        ));
        assert!(matches_list_filters(
            acl::VISIBILITY_PRIVATE,
            Some(acl::ACCESS_GUEST),
            false,
            &q,
            false,
            true
        ));
        assert!(matches_list_filters(
            acl::VISIBILITY_INTERNAL,
            None,
            false,
            &q,
            false,
            true
        ));
        assert!(!matches_list_filters(
            acl::VISIBILITY_INTERNAL,
            None,
            false,
            &q,
            true,
            true
        ));
        assert!(matches_list_filters(
            acl::VISIBILITY_PUBLIC,
            None,
            false,
            &q,
            true,
            true
        ));
    }

    #[test]
    fn membership_filter_excludes_non_member_public() {
        let mut q = ProjectsQuery::default_simple();
        q.membership = true;
        assert!(!matches_list_filters(
            acl::VISIBILITY_PUBLIC,
            None,
            false,
            &q,
            false,
            true
        ));
        assert!(matches_list_filters(
            acl::VISIBILITY_PRIVATE,
            Some(acl::ACCESS_GUEST),
            false,
            &q,
            false,
            true
        ));
    }

    #[test]
    fn anonymous_list_shows_public_only() {
        let q = ProjectsQuery::default_simple();
        assert!(matches_list_filters(
            acl::VISIBILITY_PUBLIC,
            None,
            false,
            &q,
            false,
            false
        ));
        assert!(!matches_list_filters(
            acl::VISIBILITY_INTERNAL,
            None,
            false,
            &q,
            false,
            false
        ));
        assert!(!matches_list_filters(
            acl::VISIBILITY_PRIVATE,
            None,
            false,
            &q,
            false,
            false
        ));
    }

    #[test]
    fn list_item_json_has_required_fields() {
        let item = ProjectListItem {
            id: 14,
            description: None,
            name: "hello".to_string(),
            path: "hello".to_string(),
            path_with_namespace: "root/hello".to_string(),
            name_with_namespace: "root / hello".to_string(),
            created_at: Some("2026-09-08T00:00:00.000Z".to_string()),
            default_branch: Some("master".to_string()),
            tag_list: vec![],
            topics: vec![],
            ssh_url_to_repo: "ssh://git@bak.toarujs.com:9078/root/hello.git".to_string(),
            http_url_to_repo: "https://bak.toarujs.com:9071/root/hello.git".to_string(),
            web_url: "https://bak.toarujs.com:9071/root/hello".to_string(),
            readme_url: Some(
                "https://bak.toarujs.com:9071/root/hello/-/blob/master/README.md".to_string(),
            ),
            forks_count: 0,
            avatar_url: None,
            star_count: 0,
            visibility: "private".to_string(),
            last_activity_at: Some("2026-09-08T00:00:00.000Z".to_string()),
            namespace: ProjectNamespace {
                id: 1,
                name: "root".to_string(),
                path: "root".to_string(),
                kind: "user".to_string(),
                full_path: "root".to_string(),
                parent_id: None,
                avatar_url: None,
                web_url: "https://bak.toarujs.com:9071/root".to_string(),
            },
        };
        let value = serde_json::to_value(&item).unwrap();
        for key in [
            "id",
            "name",
            "path_with_namespace",
            "visibility",
            "last_activity_at",
            "created_at",
            "default_branch",
            "web_url",
            "ssh_url_to_repo",
            "http_url_to_repo",
            "readme_url",
            "namespace",
            "tag_list",
            "topics",
            "star_count",
            "forks_count",
            "avatar_url",
            "description",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert_eq!(value["visibility"], "private");
        assert_eq!(value["namespace"]["kind"], "user");
        assert_eq!(value["topics"], serde_json::json!([]));
        assert_eq!(visibility_name(0), "private");
        assert_eq!(visibility_name(10), "internal");
        assert_eq!(visibility_name(20), "public");
    }

    #[test]
    fn logged_in_full_list_falls_back() {
        assert!(should_fallback_full_entity(true, false));
        assert!(!should_fallback_full_entity(true, true));
        assert!(!should_fallback_full_entity(false, false));
        assert!(!should_fallback_full_entity(false, true));
    }

    #[test]
    fn site_urls_from_yml_include_ssh_port() {
        let yml = r#"
production:
  gitlab:
    host: bak.toarujs.com
    https: true
    port: 9071
  gitlab_shell:
    ssh_port: 9078
"#;
        let urls = SiteUrls::from_yml(yml).expect("urls");
        assert_eq!(urls.web_base, "https://bak.toarujs.com:9071");
        assert_eq!(urls.ssh_prefix, "ssh://git@bak.toarujs.com:9078");
    }

    #[test]
    fn gravatar_matches_gitlab_sha256() {
        assert_eq!(
            gravatar_url("gitlab_admin_fea2ef@example.com"),
            "https://secure.gravatar.com/avatar/65dedbe8ac5f7b6795f231875cafc1b5e53c53feee425a95290d7e278cdf93a1?s=80&d=identicon"
        );
        assert_eq!(namespace_kind(Some("Group")), "group");
        assert_eq!(namespace_kind(Some("User")), "user");
        assert_eq!(namespace_kind(None), "user");
    }

    fn show_fallback_reason(pre: ShowPrecheck) -> String {
        match pre {
            ShowPrecheck::Fallback { reason, .. } => reason,
            ShowPrecheck::Ready { project_id } => panic!("expected fallback, got {project_id}"),
        }
    }

    #[test]
    fn project_show_numeric_and_encoded_path_are_ready() {
        assert_eq!(
            parse_project_show_id("/api/v4/projects/13"),
            Some("13".to_string())
        );
        assert_eq!(
            parse_project_show_id("/api/v4/projects/toaru%2Findextts2.5-byond"),
            Some("toaru/indextts2.5-byond".to_string())
        );
        match precheck_show(&Method::GET, "/api/v4/projects/13", None) {
            ShowPrecheck::Ready { project_id } => assert_eq!(project_id, "13"),
            ShowPrecheck::Fallback { reason, .. } => panic!("unexpected fallback: {reason}"),
        }
    }

    #[test]
    fn project_show_rejects_index_subpath_and_query() {
        assert!(parse_project_show_id("/api/v4/projects").is_none());
        assert!(parse_project_show_id("/api/v4/projects/13/").is_none());
        assert!(parse_project_show_id("/api/v4/projects/13/repository/tree").is_none());
        assert!(parse_project_show_id("/api/v4/projects/import").is_none());
        let reason = show_fallback_reason(precheck_show(
            &Method::GET,
            "/api/v4/projects/13",
            Some("statistics=true"),
        ));
        assert!(reason.contains("query not accelerated"), "{reason}");
    }
}
