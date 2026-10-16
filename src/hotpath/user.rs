//! Accelerate `GET /api/v4/user` for the authenticated user.
//!
//! The current-user payload is fetched on essentially every page load, yet the
//! Rails render (Grape entity plus a dozen preloads) costs ~130ms. Everything
//! the entity exposes is a `users` / `user_details` column plus a few small
//! dependencies, so it assembles from a handful of indexed queries.
//!
//! Parity is the hard requirement, so any attribute whose Rails value depends
//! on logic we do not reproduce (uploaded avatars, timezones, bot name
//! redaction, per-user OTP providers, private commit emails, locks) makes the
//! request fall back to Puma instead of guessing. The entity shape also differs
//! by viewer: admins get `Entities::UserWithAdmin` (42 fields), everyone else
//! `Entities::UserPublic` (38 fields).

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

use super::auth;
use super::projects::{gravatar_url, site_urls, AccelResult, SiteUrls};
use super::{CapturedRequest, CURRENT_USER_PATH_TEMPLATE};
use crate::state::AppState;

const SKIP_ACCEL_HEADER: &str = "x-gitlab-rs-skip-accel";
const PRIVATE_COMMIT_EMAIL_TOKEN: &str = "_private";
/// `Gitlab::Access::NO_ACCESS` / `GUEST`; `allow_user_to_create_group_and_project?`
/// tests `highest_role > GUEST`.
const ACCESS_GUEST: i32 = 10;

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct CurrentUser {
    pub id: i64,
    pub username: String,
    pub public_email: Option<String>,
    pub name: String,
    pub state: String,
    pub locked: bool,
    pub avatar_url: Option<String>,
    pub web_url: String,
    pub created_at: Option<String>,
    pub bio: Option<String>,
    pub location: Option<String>,
    pub linkedin: Option<String>,
    pub twitter: Option<String>,
    pub discord: Option<String>,
    pub website_url: Option<String>,
    pub github: Option<String>,
    pub job_title: Option<String>,
    pub pronouns: Option<String>,
    pub organization: Option<String>,
    pub bot: bool,
    pub work_information: Option<String>,
    pub local_time: Option<String>,
    pub last_sign_in_at: Option<String>,
    pub confirmed_at: Option<String>,
    pub last_activity_on: Option<String>,
    pub email: String,
    pub theme_id: i32,
    pub color_scheme_id: i64,
    pub projects_limit: i32,
    pub current_sign_in_at: Option<String>,
    pub identities: Vec<IdentityItem>,
    pub can_create_group: bool,
    pub can_create_project: bool,
    pub two_factor_enabled: bool,
    pub external: bool,
    pub private_profile: bool,
    pub commit_email: String,
    pub preferred_language: String,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub admin: Option<AdminExtras>,
}

/// `Entities::UserWithAdmin` adds four attributes on top of `UserPublic`.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct AdminExtras {
    pub is_admin: bool,
    pub note: Option<String>,
    pub namespace_id: Option<i64>,
    pub created_by: Option<UserBasicItem>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct IdentityItem {
    pub provider: String,
    pub extern_uid: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct UserBasicItem {
    pub id: i64,
    pub username: String,
    pub public_email: Option<String>,
    pub name: String,
    pub state: String,
    pub locked: bool,
    pub avatar_url: Option<String>,
    pub web_url: String,
}

/// Outcome of assembling an entity: either a ready payload or a deliberate
/// bail-out to Puma. `Err` is a genuine backend failure.
enum Load<T> {
    Ready(T),
    Fallback(String),
}

pub fn is_current_user_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path == "/api/v4/user" || path == "/api/v4/user/"
}

pub fn accelerate<'a>(
    state: &'a AppState,
    req: &Request<Body>,
) -> impl std::future::Future<Output = AccelResult> + Send + 'a {
    let req = CapturedRequest::from_http(req);
    async move {
        let started = Instant::now();
        match tokio::time::timeout(state.hotpath.timeout, accelerate_inner(state, &req)).await {
            Ok(AccelResult::Hit(resp)) => {
                let _ = state.metrics.record_hotpath(
                    CURRENT_USER_PATH_TEMPLATE,
                    "hit",
                    started.elapsed().as_secs_f64(),
                );
                AccelResult::Hit(resp)
            }
            Ok(other) => other,
            Err(_) => {
                let _ = state.metrics.record_hotpath(
                    CURRENT_USER_PATH_TEMPLATE,
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

async fn accelerate_inner(state: &AppState, req: &CapturedRequest) -> AccelResult {
    if req.headers.get(SKIP_ACCEL_HEADER).is_some() {
        return fallback("skip accel header");
    }
    if req.method != Method::GET || !is_current_user_path(&req.path) {
        return fallback("not current user");
    }
    if req.query.as_deref().filter(|s| !s.is_empty()).is_some() {
        return fallback("query not accelerated");
    }
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
    // `/user` requires authentication; Rails answers anonymous callers with 401.
    let Some(user_id) = identity.user_id else {
        return unauthorized_hit();
    };
    let Some(urls) = site_urls() else {
        return fallback("gitlab.yml missing");
    };

    match load_current_user(database_url, user_id, identity.admin, &urls).await {
        Ok(Load::Ready(item)) => AccelResult::Hit(current_user_response(&item)),
        Ok(Load::Fallback(reason)) => fallback(&reason),
        Err(e) => record_error(state, &format!("postgres: {e}")),
    }
}

async fn load_current_user(
    connstr: &str,
    user_id: i64,
    viewer_admin: bool,
    urls: &SiteUrls,
) -> Result<Load<CurrentUser>, String> {
    let client = super::pg::get(connstr).await?;

    let settings = client
        .query_opt(
            "SELECT allow_project_creation_for_guest_and_below, gravatar_enabled
             FROM application_settings
             ORDER BY id
             LIMIT 1",
            &[],
        )
        .await
        .map_err(|e| e.to_string())?;
    let Some(settings) = settings else {
        return Ok(Load::Fallback("application_settings missing".to_string()));
    };
    let allow_guest_projects: bool = settings.try_get(0).unwrap_or(true);
    let gravatar_enabled: bool = settings.try_get(1).unwrap_or(true);

    let row = client
        .query_opt(
            "SELECT
               u.id, u.username, u.public_email, u.name, u.state, u.locked_at,
               u.avatar, u.email, u.created_at, u.last_sign_in_at, u.confirmed_at,
               u.last_activity_on::text, u.theme_id, u.color_scheme_id,
               u.projects_limit, u.current_sign_in_at, u.external,
               u.private_profile, u.commit_email, u.preferred_language, u.admin,
               u.note, u.user_type, u.can_create_group, u.otp_required_for_login,
               u.created_by_id,
               ud.bio, ud.location, ud.linkedin, ud.twitter, ud.discord,
               ud.website_url, ud.github, ud.job_title, ud.pronouns, ud.company,
               up.timezone
             FROM users u
             LEFT JOIN user_details ud ON ud.user_id = u.id
             LEFT JOIN user_preferences up ON up.user_id = u.id
             WHERE u.id = $1",
            &[&user_id],
        )
        .await
        .map_err(|e| e.to_string())?;
    let Some(row) = row else {
        return Ok(Load::Fallback("current user not found".to_string()));
    };

    // --- deliberate fallbacks for values we do not reproduce ---

    // Bots use `redacted_name`, which depends on the viewer's access to their
    // groups/projects; the `bot` flag also depends on the type.
    if pg_i16(&row, 22) != 0 {
        return Ok(Load::Fallback("bot user type not accelerated".to_string()));
    }
    // `access_locked?` is `locked_at && !lock_expired?` with a configurable
    // unlock window.
    if pg_has_time(&row, 5) {
        return Ok(Load::Fallback("locked user not accelerated".to_string()));
    }
    // Uploaded avatars resolve through the uploader (asset host, signatures).
    if pg_opt_string_nonempty(&row, 6).is_some() {
        return Ok(Load::Fallback("uploaded avatar not accelerated".to_string()));
    }
    // `local_time` formats the wall clock in the user's timezone.
    if pg_opt_string_nonempty(&row, 36).is_some() {
        return Ok(Load::Fallback("timezone not accelerated".to_string()));
    }
    // `_private` commit emails expand to a configurable private domain.
    let commit_email_col = pg_opt_string_nonempty(&row, 18);
    if commit_email_col.as_deref() == Some(PRIVATE_COMMIT_EMAIL_TOKEN) {
        return Ok(Load::Fallback("private commit email not accelerated".to_string()));
    }
    // FortiAuthenticator / Duo make `two_factor_otp_enabled?` independent of the
    // `otp_required_for_login` column.
    if otp_providers_enabled() {
        return Ok(Load::Fallback("otp provider enabled".to_string()));
    }

    let namespace_id = client
        .query_opt(
            "SELECT id FROM namespaces WHERE owner_id = $1 AND type = 'User' LIMIT 1",
            &[&user_id],
        )
        .await
        .map_err(|e| e.to_string())?
        .map(|r| pg_i64(&r, 0));

    let two_factor_enabled =
        pg_bool(&row, 24) || second_factor_webauthn_count(&client, user_id).await? > 0;

    let highest_role = client
        .query_opt(
            "SELECT highest_access_level FROM user_highest_roles WHERE user_id = $1",
            &[&user_id],
        )
        .await
        .map_err(|e| e.to_string())?
        .map(|r| pg_i32(&r, 0))
        .unwrap_or(0);
    let personal_projects = match namespace_id {
        Some(ns) => client
            .query_one(
                "SELECT count(*) FROM projects WHERE namespace_id = $1",
                &[&ns],
            )
            .await
            .map_err(|e| e.to_string())?
            .try_get::<_, i64>(0)
            .unwrap_or(0),
        None => 0,
    };

    let admin = pg_bool(&row, 20);
    // `User#allow_user_to_create_group_and_project?`
    let allow_create = allow_guest_projects || admin || highest_role > ACCESS_GUEST;
    let can_create_group = pg_bool(&row, 23) && allow_create;
    let projects_limit = pg_i32(&row, 14);
    let can_create_project = allow_create && i64::from(projects_limit) > personal_projects;

    let identities = load_identities(&client, user_id).await?;
    let email: String = row.try_get(7).unwrap_or_default();
    let username: String = row.try_get(1).unwrap_or_default();
    let web_base = urls.web_base.trim_end_matches('/');
    let avatar_url = user_avatar_url(&email, &username, gravatar_enabled);

    let admin_extras = if viewer_admin {
        let created_by = match pg_opt_i64(&row, 25) {
            Some(creator_id) => match load_created_by(&client, creator_id, gravatar_enabled, urls).await? {
                Load::Ready(item) => item,
                Load::Fallback(reason) => return Ok(Load::Fallback(reason)),
            },
            None => None,
        };
        Some(AdminExtras {
            is_admin: admin,
            note: pg_opt_string(&row, 21),
            namespace_id,
            created_by,
        })
    } else {
        None
    };

    let item = CurrentUser {
        id: pg_i64(&row, 0),
        username: username.clone(),
        public_email: pg_opt_string(&row, 2),
        name: pg_opt_string(&row, 3).unwrap_or_default(),
        state: pg_opt_string(&row, 4).unwrap_or_default(),
        locked: false,
        avatar_url,
        web_url: format!("{web_base}/{username}"),
        created_at: pg_time(&row, 8),
        bio: pg_opt_string(&row, 26),
        location: pg_opt_string(&row, 27),
        linkedin: pg_opt_string(&row, 28),
        twitter: pg_opt_string(&row, 29),
        discord: pg_opt_string(&row, 30),
        website_url: pg_opt_string(&row, 31),
        github: pg_opt_string(&row, 32),
        job_title: pg_opt_string(&row, 33),
        pronouns: pg_opt_string(&row, 34),
        organization: pg_opt_string(&row, 35),
        bot: false,
        work_information: work_information(
            pg_opt_string_nonempty(&row, 33),
            pg_opt_string_nonempty(&row, 35),
        ),
        local_time: None,
        last_sign_in_at: pg_time(&row, 9),
        confirmed_at: pg_time(&row, 10),
        last_activity_on: pg_opt_string(&row, 11),
        email: email.clone(),
        theme_id: pg_i32(&row, 12),
        color_scheme_id: pg_i64(&row, 13),
        projects_limit,
        current_sign_in_at: pg_time(&row, 15),
        identities,
        can_create_group,
        can_create_project,
        two_factor_enabled,
        external: pg_bool(&row, 16),
        private_profile: pg_bool(&row, 17),
        commit_email: commit_email_col.unwrap_or_else(|| email.clone()),
        preferred_language: pg_opt_string_nonempty(&row, 19).unwrap_or_else(|| "en".to_string()),
        admin: admin_extras,
    };
    Ok(Load::Ready(item))
}

/// `GravatarService` / `AvatarUploader` resolution for the common case: no
/// uploaded avatar, so the value is the gravatar URL or nil.
fn user_avatar_url(email: &str, username: &str, gravatar_enabled: bool) -> Option<String> {
    if !gravatar_enabled {
        return None;
    }
    let identifier = if email.trim().is_empty() { username } else { email };
    if identifier.trim().is_empty() {
        None
    } else {
        Some(gravatar_url(identifier))
    }
}

async fn load_identities(
    client: &tokio_postgres::Client,
    user_id: i64,
) -> Result<Vec<IdentityItem>, String> {
    let rows = client
        .query(
            "SELECT provider, extern_uid FROM identities WHERE user_id = $1 ORDER BY id",
            &[&user_id],
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|r| IdentityItem {
            provider: r.try_get::<_, Option<String>>(0).ok().flatten().unwrap_or_default(),
            extern_uid: r.try_get::<_, Option<String>>(1).ok().flatten().unwrap_or_default(),
        })
        .collect())
}

async fn load_created_by(
    client: &tokio_postgres::Client,
    creator_id: i64,
    gravatar_enabled: bool,
    urls: &SiteUrls,
) -> Result<Load<Option<UserBasicItem>>, String> {
    let row = client
        .query_opt(
            "SELECT u.id, u.username, u.public_email, u.name, u.state, u.locked_at,
                    u.avatar, u.email, u.user_type
             FROM users u WHERE u.id = $1",
            &[&creator_id],
        )
        .await
        .map_err(|e| e.to_string())?;
    let Some(row) = row else {
        // `created_by` is `belongs_to`; a dangling id renders as nil in Rails.
        return Ok(Load::Ready(None));
    };
    // A bot creator needs `redacted_name` against the viewer's grants, and a
    // locked/avatar-bearing creator needs logic we do not model: fall back.
    if pg_i16(&row, 8) != 0 {
        return Ok(Load::Fallback("bot created_by not accelerated".to_string()));
    }
    if pg_has_time(&row, 5) {
        return Ok(Load::Fallback("locked created_by not accelerated".to_string()));
    }
    if pg_opt_string_nonempty(&row, 6).is_some() {
        return Ok(Load::Fallback("uploaded created_by avatar not accelerated".to_string()));
    }
    let email: String = row.try_get(7).unwrap_or_default();
    let username: String = row.try_get(1).unwrap_or_default();
    let web_base = urls.web_base.trim_end_matches('/');
    Ok(Load::Ready(Some(UserBasicItem {
        id: pg_i64(&row, 0),
        username: username.clone(),
        public_email: pg_opt_string(&row, 2),
        name: pg_opt_string(&row, 3).unwrap_or_default(),
        state: pg_opt_string(&row, 4).unwrap_or_default(),
        locked: false,
        avatar_url: user_avatar_url(&email, &username, gravatar_enabled),
        web_url: format!("{web_base}/{username}"),
    })))
}

async fn second_factor_webauthn_count(
    client: &tokio_postgres::Client,
    user_id: i64,
) -> Result<i64, String> {
    Ok(client
        .query_one(
            // `authentication_mode` enum: passwordless=1, second_factor=2.
            "SELECT count(*) FROM webauthn_registrations
             WHERE user_id = $1 AND authentication_mode = 2",
            &[&user_id],
        )
        .await
        .map_err(|e| e.to_string())?
        .try_get::<_, i64>(0)
        .unwrap_or(0))
}

/// `UsersHelper#work_information`: "job at org" when both are present, the one
/// present otherwise, nil when neither is.
fn work_information(job_title: Option<String>, organization: Option<String>) -> Option<String> {
    match (job_title, organization) {
        (Some(job), Some(org)) => Some(format!("{job} at {org}")),
        (Some(job), None) => Some(job),
        (None, Some(org)) => Some(org),
        (None, None) => None,
    }
}

/// Whether FortiAuthenticator / FortiToken Cloud / Duo are enabled in
/// `gitlab.yml`; any of them makes `two_factor_otp_enabled?` independent of the
/// OTP column, which we do not model.
fn otp_providers_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        for path in super::projects::GITLAB_YML_PATHS {
            if let Ok(text) = std::fs::read_to_string(path) {
                return ["forti_authenticator:", "forti_token_cloud:", "duo_auth:"]
                    .iter()
                    .any(|key| yml_section_enabled(&text, key));
            }
        }
        false
    })
}

fn yml_section_enabled(text: &str, key: &str) -> bool {
    let mut in_section = false;
    for line in text.lines() {
        if line.starts_with(key) {
            in_section = true;
            continue;
        }
        if in_section {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("enabled:") {
                return rest.trim().eq_ignore_ascii_case("true");
            }
            // Leave the section as soon as another top-level key appears.
            if !line.starts_with(' ') && !line.starts_with('\t') && !trimmed.is_empty() {
                in_section = false;
            }
        }
    }
    false
}

fn current_user_response(item: &CurrentUser) -> Response {
    let body = serde_json::to_string(item).unwrap_or_else(|_| "{}".to_string());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

fn fallback(reason: &str) -> AccelResult {
    AccelResult::Fallback {
        reason: reason.to_string(),
        error: false,
    }
}

fn record_error(state: &AppState, reason: &str) -> AccelResult {
    let _ = state
        .metrics
        .record_hotpath(CURRENT_USER_PATH_TEMPLATE, "error", 0.0);
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
    if let Ok(v) = row.try_get::<_, i16>(idx) {
        return i32::from(v);
    }
    0
}

fn pg_i16(row: &tokio_postgres::Row, idx: usize) -> i16 {
    if let Ok(v) = row.try_get::<_, i16>(idx) {
        return v;
    }
    if let Ok(v) = row.try_get::<_, i32>(idx) {
        return v as i16;
    }
    0
}

fn pg_bool(row: &tokio_postgres::Row, idx: usize) -> bool {
    row.try_get::<_, bool>(idx).unwrap_or(false)
}

fn pg_opt_string(row: &tokio_postgres::Row, idx: usize) -> Option<String> {
    row.try_get::<_, Option<String>>(idx).ok().flatten()
}

fn pg_opt_string_nonempty(row: &tokio_postgres::Row, idx: usize) -> Option<String> {
    pg_opt_string(row, idx).filter(|s| !s.trim().is_empty())
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

fn pg_has_time(row: &tokio_postgres::Row, idx: usize) -> bool {
    row.try_get::<_, Option<SystemTime>>(idx).ok().flatten().is_some()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_current_user_path() {
        assert!(is_current_user_path("/api/v4/user"));
        assert!(is_current_user_path("/api/v4/user/"));
        assert!(is_current_user_path("/api/v4/user?x=1"));
        assert!(!is_current_user_path("/api/v4/users"));
        assert!(!is_current_user_path("/api/v4/user/avatar"));
        assert!(!is_current_user_path("/api/v4/user/keys"));
    }

    #[test]
    fn work_information_combines_present_fields() {
        assert_eq!(
            work_information(Some("Job".into()), Some("Org".into())),
            Some("Job at Org".to_string())
        );
        assert_eq!(work_information(Some("Job".into()), None), Some("Job".to_string()));
        assert_eq!(work_information(None, Some("Org".into())), Some("Org".to_string()));
        assert_eq!(work_information(None, None), None);
    }

    #[test]
    fn yml_section_enabled_reads_flag() {
        let yml = "forti_authenticator:\n  # comment\n  enabled: false\nlogging:\n  enabled: true\n";
        assert!(!yml_section_enabled(yml, "forti_authenticator:"));
        let yml2 = "duo_auth:\n  enabled: true\n";
        assert!(yml_section_enabled(yml2, "duo_auth:"));
    }

    #[test]
    fn user_avatar_url_gravatar_gating() {
        assert!(user_avatar_url("a@b.com", "a", false).is_none());
        let url = user_avatar_url("A@B.com", "a", true).unwrap();
        assert!(url.starts_with("https://secure.gravatar.com/avatar/"));
        assert!(url.ends_with("?s=80&d=identicon"));
        // Empty email falls back to username; empty both yields nil.
        assert!(user_avatar_url("", "", true).is_none());
        assert!(user_avatar_url("", "user", true).is_some());
    }
}
