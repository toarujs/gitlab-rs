//! Resolve API identity: session cookie, PAT, or anonymous.
//!
//! JOB-TOKEN / deploy-token / unknown Bearer values fall back to Rails.

use axum::http::HeaderMap;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

use super::session;
use crate::redis::RedisClient;

pub const UNAUTHORIZED_JSON: &str = "{\"message\":\"401 Unauthorized\"}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub user_id: Option<i64>,
    pub external: bool,
    pub admin: bool,
}

impl Identity {
    pub fn anonymous() -> Self {
        Self {
            user_id: None,
            external: false,
            admin: false,
        }
    }

    pub fn logged_in(&self) -> bool {
        self.user_id.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Unauthorized,
    Fallback { reason: String, error: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentedCredential {
    None,
    Session { id: String },
    PersonalAccessToken(String),
    JobToken,
    DeployToken,
}

pub fn token_digest(token: &str) -> String {
    match db_key_base() {
        Some(salt) => gitlab_pat_digest(token, salt),
        None => hex::encode(Sha256::digest(token.as_bytes())),
    }
}

/// GitLab `Gitlab::CryptoHelper.sha256`: SHA256(token + db_key_base[0..31]) as Base64.
pub fn gitlab_pat_digest(token: &str, db_key_base: &str) -> String {
    let salt: String = db_key_base.chars().take(32).collect();
    let digest = Sha256::digest(format!("{token}{salt}").as_bytes());
    base64::engine::general_purpose::STANDARD.encode(digest)
}

pub fn db_key_base_from_yml(text: &str) -> Option<String> {
    let mut in_prod = false;
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
        if k.trim() != "db_key_base" {
            continue;
        }
        let v = v.trim().trim_matches('"').trim_matches('\'');
        if !v.is_empty() && !v.starts_with('|') {
            return Some(v.to_string());
        }
    }
    None
}

fn db_key_base() -> Option<&'static str> {
    static KEY: OnceLock<Option<String>> = OnceLock::new();
    KEY.get_or_init(load_db_key_base).as_deref()
}

fn load_db_key_base() -> Option<String> {
    if let Ok(v) = std::env::var("GITLAB_RS_DB_KEY_BASE") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    for path in [
        "/var/opt/gitlab/gitlab-rails/etc/secrets.yml",
        "/opt/gitlab/embedded/service/gitlab-rails/config/secrets.yml",
    ] {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(key) = db_key_base_from_yml(&text) {
                return Some(key);
            }
        }
    }
    None
}

pub fn token_has_api_scope(scopes: &str) -> bool {
    scopes
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|part| part == "api" || part == "read_api")
}

pub fn presented_credential(headers: &HeaderMap, query: Option<&str>) -> PresentedCredential {
    if header_nonempty(headers, "job-token").is_some() || query_nonempty(query, "job_token").is_some() {
        return PresentedCredential::JobToken;
    }
    if header_nonempty(headers, "deploy-token").is_some() {
        return PresentedCredential::DeployToken;
    }
    if let Some(token) = header_nonempty(headers, "private-token") {
        return PresentedCredential::PersonalAccessToken(token);
    }
    if let Some(token) = query_nonempty(query, "private_token") {
        return PresentedCredential::PersonalAccessToken(token);
    }
    if let Some(cred) = authorization_credential(headers) {
        return cred;
    }
    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Some(id) = session::session_id_from_cookie_header(cookie) {
        return PresentedCredential::Session { id };
    }
    PresentedCredential::None
}

pub async fn resolve_identity(
    headers: &HeaderMap,
    query: Option<&str>,
    redis_url: Option<&str>,
    database_url: &str,
) -> Result<Identity, AuthError> {
    match presented_credential(headers, query) {
        PresentedCredential::JobToken => Err(AuthError::Fallback {
            reason: "job token not verified locally".to_string(),
            error: false,
        }),
        PresentedCredential::DeployToken => Err(AuthError::Fallback {
            reason: "deploy token not verified locally".to_string(),
            error: false,
        }),
        PresentedCredential::PersonalAccessToken(token) => {
            lookup_personal_access_token(database_url, &token).await
        }
        PresentedCredential::Session { id } => {
            lookup_session_identity(redis_url, database_url, &id).await
        }
        PresentedCredential::None => Ok(Identity::anonymous()),
    }
}

async fn lookup_personal_access_token(
    database_url: &str,
    token: &str,
) -> Result<Identity, AuthError> {
    let digest = token_digest(token);
    let client = pg_client(database_url).await?;
    let row = client
        .query_opt(
            "SELECT u.id, u.admin, u.external, u.state, pat.scopes
             FROM personal_access_tokens pat
             INNER JOIN users u ON u.id = pat.user_id
             WHERE pat.token_digest = $1
               AND pat.revoked = FALSE
               AND (pat.expires_at IS NULL OR pat.expires_at >= CURRENT_DATE)
             LIMIT 1",
            &[&digest],
        )
        .await
        .map_err(|e| AuthError::Fallback {
            reason: format!("postgres pat: {e}"),
            error: true,
        })?;
    let Some(row) = row else {
        if db_key_base().is_none() {
            return Err(AuthError::Fallback {
                reason: "pat digest salt missing".to_string(),
                error: false,
            });
        }
        if token.starts_with("glpat-") {
            return Err(AuthError::Unauthorized);
        }
        return Err(AuthError::Fallback {
            reason: "access token not a pat".to_string(),
            error: false,
        });
    };
    let scopes = scopes_from_row(&row, 4);
    if !token_has_api_scope(&scopes) {
        return Err(AuthError::Fallback {
            reason: "pat missing api scope".to_string(),
            error: false,
        });
    }
    identity_from_user_row(&row, 0, 1, 2, 3)
}

async fn lookup_session_identity(
    redis_url: Option<&str>,
    database_url: &str,
    session_id: &str,
) -> Result<Identity, AuthError> {
    let Some(redis_url) = redis_url else {
        return Err(AuthError::Fallback {
            reason: "redis not configured".to_string(),
            error: false,
        });
    };
    let user_id = match load_session_user_id(redis_url, session_id).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return Err(AuthError::Fallback {
                reason: "session decode failed".to_string(),
                error: false,
            });
        }
        Err(e) => {
            return Err(AuthError::Fallback {
                reason: format!("session redis: {e}"),
                error: false,
            });
        }
    };
    let client = pg_client(database_url).await?;
    let row = client
        .query_opt(
            "SELECT id, admin, external, state FROM users WHERE id = $1",
            &[&user_id],
        )
        .await
        .map_err(|e| AuthError::Fallback {
            reason: format!("postgres user: {e}"),
            error: true,
        })?;
    let Some(row) = row else {
        return Err(AuthError::Fallback {
            reason: "session user missing".to_string(),
            error: false,
        });
    };
    identity_from_user_row(&row, 0, 1, 2, 3)
}

async fn load_session_user_id(redis_url: &str, session_id: &str) -> Result<Option<i64>, String> {
    let client = RedisClient::shared(redis_url).await?;
    for key in session::redis_session_keys(session_id) {
        if let Some(bytes) = client.get_bytes(&key).await? {
            if let Some(id) = session::user_id_from_session_bytes(&bytes) {
                return Ok(Some(id));
            }
        }
    }
    Ok(None)
}

fn identity_from_user_row(
    row: &tokio_postgres::Row,
    id_idx: usize,
    admin_idx: usize,
    external_idx: usize,
    state_idx: usize,
) -> Result<Identity, AuthError> {
    let state: String = row.try_get(state_idx).unwrap_or_default();
    if state != "active" {
        return Err(AuthError::Unauthorized);
    }
    Ok(Identity {
        user_id: Some(pg_i64(row, id_idx)),
        admin: pg_bool(row, admin_idx),
        external: pg_bool(row, external_idx),
    })
}

async fn pg_client(connstr: &str) -> Result<super::pg::PgConn, AuthError> {
    super::pg::get(connstr)
        .await
        .map_err(|reason| AuthError::Fallback {
            reason: format!("postgres: {reason}"),
            error: true,
        })
}

fn authorization_credential(headers: &HeaderMap) -> Option<PresentedCredential> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .trim();
    if let Some(token) = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
    {
        let token = token.trim();
        if token.is_empty() {
            return None;
        }
        return Some(PresentedCredential::PersonalAccessToken(token.to_string()));
    }
    let encoded = raw
        .strip_prefix("Basic ")
        .or_else(|| raw.strip_prefix("basic "))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim().as_bytes())
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    if user.eq_ignore_ascii_case("gitlab-ci-token") {
        return Some(PresentedCredential::JobToken);
    }
    let pass = pass.trim();
    if pass.is_empty() {
        return None;
    }
    Some(PresentedCredential::PersonalAccessToken(pass.to_string()))
}

fn header_nonempty(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn query_nonempty(query: Option<&str>, key: &str) -> Option<String> {
    let query = query?;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let k = parts.next()?;
        if k != key {
            continue;
        }
        let value = percent_decode(parts.next().unwrap_or(""));
        if value.is_empty() {
            return None;
        }
        return Some(value);
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
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn scopes_from_row(row: &tokio_postgres::Row, idx: usize) -> String {
    if let Ok(s) = row.try_get::<_, String>(idx) {
        return s;
    }
    if let Ok(v) = row.try_get::<_, Vec<String>>(idx) {
        return v.join(" ");
    }
    if let Ok(Some(s)) = row.try_get::<_, Option<String>>(idx) {
        return s;
    }
    String::new()
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

fn pg_bool(row: &tokio_postgres::Row, idx: usize) -> bool {
    if let Ok(v) = row.try_get::<_, bool>(idx) {
        return v;
    }
    if let Ok(v) = row.try_get::<_, Option<bool>>(idx) {
        return v.unwrap_or(false);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, HeaderValue};

    fn headers_from(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (k, v) in pairs {
            let name = axum::http::header::HeaderName::from_bytes(k.as_bytes()).unwrap();
            headers.append(name, HeaderValue::from_str(v).unwrap());
        }
        headers
    }

    #[test]
    fn private_token_header_wins() {
        let headers = headers_from(&[
            ("PRIVATE-TOKEN", "glpat-abc"),
            (header::COOKIE.as_str(), "_gitlab_session=sess"),
        ]);
        assert_eq!(
            presented_credential(&headers, None),
            PresentedCredential::PersonalAccessToken("glpat-abc".to_string())
        );
    }

    #[test]
    fn bearer_and_basic_pat() {
        let headers = headers_from(&[("authorization", "Bearer glpat-xyz")]);
        assert_eq!(
            presented_credential(&headers, None),
            PresentedCredential::PersonalAccessToken("glpat-xyz".to_string())
        );
        let basic = base64::engine::general_purpose::STANDARD.encode("oauth2:glpat-basic");
        let header_val = format!("Basic {basic}");
        let headers = headers_from(&[("authorization", header_val.as_str())]);
        assert_eq!(
            presented_credential(&headers, None),
            PresentedCredential::PersonalAccessToken("glpat-basic".to_string())
        );
    }

    #[test]
    fn job_token_and_ci_basic_fallback() {
        let headers = headers_from(&[("JOB-TOKEN", "ci-job")]);
        assert_eq!(
            presented_credential(&headers, Some("ref=main")),
            PresentedCredential::JobToken
        );
        let basic = base64::engine::general_purpose::STANDARD.encode("gitlab-ci-token:secret");
        let header_val = format!("Basic {basic}");
        let headers = headers_from(&[("authorization", header_val.as_str())]);
        assert_eq!(
            presented_credential(&headers, None),
            PresentedCredential::JobToken
        );
        let headers = HeaderMap::new();
        assert_eq!(
            presented_credential(&headers, Some("job_token=abc")),
            PresentedCredential::JobToken
        );
    }

    #[test]
    fn session_and_anonymous() {
        let headers = headers_from(&[(header::COOKIE.as_str(), "_gitlab_session=abc123")]);
        assert_eq!(
            presented_credential(&headers, None),
            PresentedCredential::Session {
                id: "abc123".to_string()
            }
        );
        assert_eq!(
            presented_credential(&HeaderMap::new(), None),
            PresentedCredential::None
        );
    }

    #[test]
    fn query_private_token() {
        let headers = HeaderMap::new();
        assert_eq!(
            presented_credential(&headers, Some("private_token=glpat-q&ref=main")),
            PresentedCredential::PersonalAccessToken("glpat-q".to_string())
        );
    }

    #[test]
    fn digest_and_scopes() {
        assert_eq!(
            gitlab_pat_digest("glpat-test", "abcdefghijklmnopqrstuvwxyz012345"),
            "Rn+60oKdukJs+FE603UZ2PvmxUDoOe8ei48mt64KOnU="
        );
        assert!(token_has_api_scope("---\n- api\n- read_repository\n"));
        assert!(token_has_api_scope("read_api read_user"));
        assert!(!token_has_api_scope("read_repository read_user"));
        assert!(!token_has_api_scope("read_registry"));
    }

    #[test]
    fn parses_omnibus_secrets_yml_db_key_base() {
        let yml = r#"
production:
  secret_key_base: ignore-me
  db_key_base: abcdefghijklmnopqrstuvwxyz0123456789extra
  otp_key_base: other
"#;
        assert_eq!(
            db_key_base_from_yml(yml).as_deref(),
            Some("abcdefghijklmnopqrstuvwxyz0123456789extra")
        );
        assert!(db_key_base_from_yml("development:\n  db_key_base: nope\n").is_none());
    }
}
