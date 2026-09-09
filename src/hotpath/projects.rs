//! Accelerate `GET /api/v4/projects` (index only, no subpaths).

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::time::{Instant, SystemTime};
use url::form_urlencoded;

use super::acl;
use super::session;
use super::PROJECTS_PATH_TEMPLATE;
use crate::redis::RedisClient;
use crate::state::AppState;

const DEFAULT_PER_PAGE: u32 = 20;
const MAX_PER_PAGE: u32 = 100;

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
    pub name: String,
    pub path: String,
    pub path_with_namespace: String,
    pub name_with_namespace: String,
    pub visibility: String,
    pub last_activity_at: Option<String>,
}

pub fn is_projects_index_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path == "/api/v4/projects" || path == "/api/v4/projects/"
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
    acl::can_see_project(visibility_level, access_level, external_user)
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
    session_id: Option<&str>,
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
    if session_id.is_none() {
        return ProjectsPrecheck::Fallback {
            reason: "session cookie missing".to_string(),
            error: false,
        };
    }
    ProjectsPrecheck::Ready(parsed)
}

pub async fn accelerate(state: &AppState, req: &Request<Body>) -> AccelResult {
    let started = Instant::now();
    match tokio::time::timeout(state.hotpath.timeout, accelerate_inner(state, req)).await {
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

async fn accelerate_inner(state: &AppState, req: &Request<Body>) -> AccelResult {
    let cookie = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session_id = session::session_id_from_cookie_header(cookie);
    let query = match precheck(
        req.method(),
        req.uri().path(),
        req.uri().query(),
        session_id.as_deref(),
    ) {
        ProjectsPrecheck::Ready(q) => q,
        ProjectsPrecheck::Fallback { reason, error } => {
            return AccelResult::Fallback { reason, error };
        }
    };
    let session_id = session_id.expect("precheck requires session");

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
    let user = match load_user(database_url, user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return AccelResult::Fallback {
                reason: "user not found".to_string(),
                error: false,
            };
        }
        Err(e) => return record_error(state, &format!("postgres user: {e}")),
    };
    if user.admin {
        return AccelResult::Fallback {
            reason: "admin listing not accelerated".to_string(),
            error: false,
        };
    }
    if user.state != "active" {
        return AccelResult::Fallback {
            reason: "user not active".to_string(),
            error: false,
        };
    }

    match load_visible_projects(database_url, user_id, user.external, &query).await {
        Ok((items, total)) => AccelResult::Hit(list_response(&items, &query, total)),
        Err(e) => record_error(state, &format!("postgres: {e}")),
    }
}

fn record_error(state: &AppState, reason: &str) -> AccelResult {
    let _ = state.metrics.record_hotpath(PROJECTS_PATH_TEMPLATE, "error", 0.0);
    AccelResult::Fallback {
        reason: reason.to_string(),
        error: true,
    }
}

struct UserRow {
    admin: bool,
    external: bool,
    state: String,
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

async fn load_user(connstr: &str, user_id: i64) -> Result<Option<UserRow>, String> {
    let client = pg_client(connstr).await?;
    let row = client
        .query_opt(
            "SELECT admin, external, state FROM users WHERE id = $1",
            &[&(user_id as i32)],
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.map(|row| UserRow {
        admin: pg_bool(&row, 0),
        external: pg_bool(&row, 1),
        state: row.try_get::<_, String>(2).unwrap_or_default(),
    }))
}

fn visibility_predicate(query: &ProjectsQuery, include_internal: bool) -> &'static str {
    if query.owned || query.membership || query.min_access_explicit {
        "p.id IN (SELECT project_id FROM project_authorizations WHERE user_id = $1 AND access_level >= $2)"
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
    user_id: i64,
    external_user: bool,
    query: &ProjectsQuery,
) -> Result<(Vec<ProjectListItem>, u64), String> {
    let client = pg_client(connstr).await?;
    let pred = visibility_predicate(query, !external_user);
    let where_sql = format!(
        "NOT COALESCE(p.pending_delete, false)
          AND NOT COALESCE(p.hidden, false)
          AND {pred}
          AND ($3::boolean IS NULL OR p.archived = $3)
          AND ($4::integer IS NULL OR p.visibility_level = $4)"
    );
    let uid = user_id as i32;
    let min_access = query.min_access_level;
    let archived = query.archived;
    let vis = query.visibility;
    let limit = i64::from(query.per_page);
    let offset = i64::from(query.page.saturating_sub(1).saturating_mul(query.per_page));

    let count_sql = format!(
        "SELECT COUNT(*) FROM projects p WHERE {where_sql}"
    );
    let count_row = client
        .query_one(&count_sql, &[&uid, &min_access, &archived, &vis])
        .await
        .map_err(|e| e.to_string())?;
    let total = pg_u64_count(&count_row);

    let order = order_sql(query.order_by, query.sort_desc);
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
                p.visibility_level, p.last_activity_at
         FROM projects p
         LEFT JOIN namespaces n ON n.id = p.namespace_id
         WHERE {where_sql}
         ORDER BY {order}
         LIMIT $5 OFFSET $6"
    );
    let rows = client
        .query(
            &list_sql,
            &[&uid, &min_access, &archived, &vis, &limit, &offset],
        )
        .await
        .map_err(|e| e.to_string())?;
    let items = rows.iter().map(item_from_row).collect();
    Ok((items, total))
}

fn item_from_row(row: &tokio_postgres::Row) -> ProjectListItem {
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
    ProjectListItem {
        id,
        name,
        path,
        path_with_namespace,
        name_with_namespace,
        visibility: visibility_name(visibility_level).to_string(),
        last_activity_at: pg_time(row, 6),
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

fn pg_bool(row: &tokio_postgres::Row, idx: usize) -> bool {
    if let Ok(v) = row.try_get::<_, bool>(idx) {
        return v;
    }
    if let Ok(v) = row.try_get::<_, Option<bool>>(idx) {
        return v.unwrap_or(false);
    }
    false
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
    fn missing_session_is_fallback() {
        let pre = precheck(&Method::GET, "/api/v4/projects", None, None);
        let reason = fallback_reason(pre);
        assert!(reason.contains("session"), "{reason}");
    }

    #[test]
    fn unknown_query_is_fallback() {
        let pre = precheck(
            &Method::GET,
            "/api/v4/projects",
            Some("search=secret&membership=true"),
            Some("sid"),
        );
        let reason = fallback_reason(pre);
        assert!(reason.contains("unknown filter"), "{reason}");
    }

    #[test]
    fn starred_and_statistics_are_fallback() {
        for q in ["starred=true", "statistics=true", "order_by=star_count"] {
            let pre = precheck(&Method::GET, "/api/v4/projects", Some(q), Some("sid"));
            assert!(
                matches!(pre, ProjectsPrecheck::Fallback { .. }),
                "{q} should fallback"
            );
        }
    }

    #[test]
    fn subpath_is_not_accelerated() {
        let pre = precheck(&Method::GET, "/api/v4/projects/14", None, Some("sid"));
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
            Some("sid"),
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
            false
        ));
        assert!(matches_list_filters(
            acl::VISIBILITY_PRIVATE,
            Some(acl::ACCESS_GUEST),
            false,
            &q,
            false
        ));
        assert!(matches_list_filters(
            acl::VISIBILITY_INTERNAL,
            None,
            false,
            &q,
            false
        ));
        assert!(!matches_list_filters(
            acl::VISIBILITY_INTERNAL,
            None,
            false,
            &q,
            true
        ));
        assert!(matches_list_filters(
            acl::VISIBILITY_PUBLIC,
            None,
            false,
            &q,
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
            false
        ));
        assert!(matches_list_filters(
            acl::VISIBILITY_PRIVATE,
            Some(acl::ACCESS_GUEST),
            false,
            &q,
            false
        ));
    }

    #[test]
    fn list_item_json_has_required_fields() {
        let item = ProjectListItem {
            id: 14,
            name: "hello".to_string(),
            path: "hello".to_string(),
            path_with_namespace: "root/hello".to_string(),
            name_with_namespace: "root / hello".to_string(),
            visibility: "private".to_string(),
            last_activity_at: Some("2026-09-08T00:00:00.000Z".to_string()),
        };
        let value = serde_json::to_value(&item).unwrap();
        for key in [
            "id",
            "name",
            "path_with_namespace",
            "visibility",
            "last_activity_at",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert_eq!(value["visibility"], "private");
        assert_eq!(visibility_name(0), "private");
        assert_eq!(visibility_name(10), "internal");
        assert_eq!(visibility_name(20), "public");
    }
}
