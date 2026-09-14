#![allow(dead_code)]
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use super::forwardheaders;
use super::gitaly;
use super::secret;
use super::senddata;
use super::state::AppState;
use super::transport;

/// Generate optimized cache key - strip cache-busting query params for static assets
fn generate_cache_key(method: &Method, uri: &Uri) -> String {
    let path = uri.path();
    let pq = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(path);

    if is_static_asset_path(path) {
        if let Some(query) = uri.query() {
            // Strip common cache-busting params (v, timestamp, hash)
            let params: Vec<&str> = query.split('&')
                .filter(|p| {
                    let key = p.split('=').next().unwrap_or("");
                    !matches!(key, "v" | "_" | "timestamp" | "hash" | "cb")
                })
                .collect();
            if params.is_empty() {
                format!("{}:{}", method.as_str(), path)
            } else {
                format!("{}:{}?{}", method.as_str(), path, params.join("&"))
            }
        } else {
            format!("{}:{}", method.as_str(), path)
        }
    } else {
        format!("{}:{}", method.as_str(), pq)
    }
}

fn should_cache_response(method: &Method, path: &str) -> bool {
    *method == Method::GET
        && is_static_asset_path(path)
        && !path.starts_with("/api/")
        && !crate::webide_nls::is_nls_messages_path(path)
}

fn is_static_asset_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".js")
        || lower.ends_with(".css")
        || lower.ends_with(".map")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".svg")
        || lower.ends_with(".ico")
        || lower.ends_with(".webp")
        || lower.ends_with(".woff")
        || lower.ends_with(".woff2")
        || lower.ends_with(".ttf")
        || lower.ends_with(".eot")
        || lower.ends_with(".wasm")
}

fn response_allows_shared_cache(headers: &HeaderMap) -> bool {
    if headers.get("set-cookie").is_some() {
        return false;
    }
    if let Some(cc) = headers.get("cache-control").and_then(|v| v.to_str().ok()) {
        let cc = cc.to_ascii_lowercase();
        if cc.contains("no-store") || cc.contains("private") || cc.contains("no-cache") {
            return false;
        }
    }
    true
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name,
        "host" | "content-length" | "transfer-encoding" | "connection"
            | "keep-alive" | "te" | "trailer" | "upgrade" | "proxy-connection"
    )
}

pub type UnixSocketClient = hyper_util::client::legacy::Client<hyperlocal::UnixConnector, http_body_util::Full<bytes::Bytes>>;

#[derive(Debug, Clone)]
pub struct ProxyState {
    pub backend_url: Url,
    pub client: Client,
    pub auth_socket: Option<String>,
    pub git_backend_url: Option<String>,
    pub circuit_breaker: Option<CircuitBreaker>,
    pub connection_pool: ConnectionPool,
}

#[derive(Debug, Clone)]
pub struct ConnectionPool {
    pub max_connections: usize,
    pub max_idle_per_host: usize,
    pub idle_timeout: Duration,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self {
            max_connections: 100,
            max_idle_per_host: 10,
            idle_timeout: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
        }
    }
}

impl ConnectionPool {
    pub fn build_client(&self) -> Result<Client, reqwest::Error> {
        Client::builder()
            .pool_max_idle_per_host(self.max_idle_per_host)
            .pool_idle_timeout(self.idle_timeout)
            .connect_timeout(self.connect_timeout)
            .timeout(self.request_timeout)
            .no_proxy()
            .build()
    }
}

#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    pub failures: Arc<RwLock<u32>>,
    pub max_failures: u32,
    pub timeout: Duration,
    pub last_failure: Arc<RwLock<Option<std::time::Instant>>>,
}

impl CircuitBreaker {
    pub fn new(max_failures: u32, timeout: Duration) -> Self {
        Self {
            failures: Arc::new(RwLock::new(0)),
            max_failures,
            timeout,
            last_failure: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn is_open(&self) -> bool {
        let failures = self.failures.read().await;
        let last_failure = self.last_failure.read().await;

        if *failures >= self.max_failures {
            if let Some(last) = *last_failure {
                if last.elapsed() < self.timeout {
                    return true;
                }
            }
        }
        false
    }

    pub async fn record_failure(&self) {
        let mut failures = self.failures.write().await;
        let mut last_failure = self.last_failure.write().await;
        *failures += 1;
        *last_failure = Some(std::time::Instant::now());
    }

    pub async fn record_success(&self) {
        let mut failures = self.failures.write().await;
        *failures = 0;
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProxyQuery {
    pub path: String,
}

/// GraphQL entry: allow POST and websocket GET; block GET mutations (CVE-2026-19650).
pub async fn graphql_handler(
    State(state): State<AppState>,
    req: axum::http::Request<Body>,
) -> Result<Response, StatusCode> {
    if is_blocked_graphql_get_mutation(req.method(), req.uri(), req.headers()) {
        tracing::warn!("Blocked GraphQL mutation via GET");
        return Err(StatusCode::METHOD_NOT_ALLOWED);
    }
    proxy_handler(State(state), req).await
}

fn is_blocked_graphql_get_mutation(method: &Method, uri: &Uri, headers: &HeaderMap) -> bool {
    if method != Method::GET {
        return false;
    }
    let is_ws = headers
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    if is_ws {
        return false;
    }
    uri.query()
        .unwrap_or("")
        .to_ascii_lowercase()
        .contains("mutation")
}

/// Main proxy handler - streams request body to backend without buffering
pub async fn proxy_handler(
    State(state): State<AppState>,
    req: axum::http::Request<Body>,
) -> Result<Response, StatusCode> {
    let device = req.extensions().get::<crate::device_detection::DeviceClass>().copied();
    let is_mobile = device.map(|d| d.is_mobile_or_tablet()).unwrap_or(false);

    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
    let uri = parts.uri.clone();
    let method_str = method.to_string();
    let path_str = uri.path().to_string();
    let client_accepts_webp = crate::imageresizer::WebPConverter::supports_webp(&parts.headers);
    let best_format = crate::imageresizer::WebPConverter::best_supported_format(&parts.headers);
    let headers = parts.headers.clone();

    let result = proxy_request_streaming(State(state.clone()), method.clone(), uri, headers, body).await;

    match result {
        Ok(mut response) => {
            let original_status = response.status();

            // Invalidate avatar cache when profile is updated via web form
            if method_str == "POST" && path_str == "/-/user_settings/profile" {
                let status = original_status.as_u16();
                if status == 200 || status == 302 {
                    if let Some(cache) = &state.cache {
                        let cache_key = "/uploads/-/system/user/avatar";
                        let removed = cache.remove_by_prefix(cache_key).await;
                        tracing::info!("Invalidated {} avatar cache entries after profile update", removed);
                    }
                }
            }

            // Prevent browser caching of avatar images to fix stale avatar display
            if path_str.starts_with("/uploads/-/system/user/avatar/") {
                response.headers_mut().insert(
                    "cache-control",
                    HeaderValue::from_static("no-cache, no-store, must-revalidate"),
                );
                response.headers_mut().insert(
                    "pragma",
                    HeaderValue::from_static("no-cache"),
                );
            }

            // Fix: post-login redirect to asset URLs → redirect to / instead
            if method_str == "POST" && path_str == "/users/sign_in" && original_status.as_u16() == 302 {
                let loc_to_fix = response.headers().get("location").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
                if let Some(ref loc_str) = loc_to_fix {
                    if loc_str.contains("/-/pwa-icons") || loc_str.contains("/-/manifest.json") || loc_str.contains("/-/collect_events") {
                        response.headers_mut().insert("location", HeaderValue::from_static("/"));
                        tracing::info!("Rewrote post-login redirect from {} to /", loc_str);
                    }
                }
            }

            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");

            if content_type.contains("text/html") {
                let new_response =
                    crate::html_injection::inject_into_response(response).await;
                tracing::info!(
                    method = %method_str,
                    path = %path_str,
                    original_status = original_status.as_u16(),
                    new_status = new_response.status().as_u16(),
                    is_mobile = is_mobile,
                    "HTML injection applied"
                );
                Ok(new_response)
            } else if is_mobile && client_accepts_webp
                && (content_type.contains("image/png") || content_type.contains("image/jpeg"))
            {
                Ok(crate::imageresizer::resize_and_convert_best_response(
                    response,
                    &state.webp_converter,
                    800,
                    best_format,
                ).await)
            } else if !is_mobile && client_accepts_webp
                && (content_type.contains("image/png") || content_type.contains("image/jpeg"))
            {
                Ok(crate::imageresizer::convert_response_to_best_format(
                    response,
                    &state.webp_converter,
                    best_format,
                ).await)
            } else if crate::webide_nls::is_nls_messages_path(&path_str) {
                Ok(crate::webide_nls::translate_nls_response(
                    response,
                    &parts.headers,
                )
                .await)
            } else {
                Ok(response)
            }
        }
        other => other,
    }
}

/// Legacy proxy handler that accepts a string body (for backward compatibility)
pub async fn proxy_request(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Result<Response, StatusCode> {
    let body = Body::from(body);
    proxy_request_streaming(State(state), method, uri, headers, body).await
}

/// Convenience wrapper that converts Bytes body to streaming
#[allow(dead_code)]
pub async fn proxy_request_with_bytes(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let body = Body::from(body);
    proxy_request_streaming(State(state), method, uri, headers, body).await
}

/// Core proxy function that streams body to backend
pub async fn proxy_request_streaming(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, StatusCode> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let path_display = uri.path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| uri.path().to_string());
    let ctx = super::logging::RequestContext::new(
        request_id.clone(),
        method.as_str().to_string(),
        path_display.clone(),
    );
    ctx.log_request();

    state.metrics.record_request();
    let timer = super::logging::RequestTimer::new(
        request_id,
        method.as_str().to_string(),
        path_display,
    );

    if let Some(cb) = &state.proxy.circuit_breaker {
        if cb.is_open().await {
            state.metrics.record_request_duration(timer.elapsed_ms() as f64 / 1000.0);
            timer.finish(503);
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    // SSRF protection: validate backend URL
    if let Some(host) = state.proxy.backend_url.host_str() {
        if let Err(e) = transport::validate_address(host, true, &[]) {
            tracing::warn!("SSRF protection blocked backend URL {}: {}", host, e);
            state.metrics.record_request_duration(timer.elapsed_ms() as f64 / 1000.0);
            timer.finish(403);
            return Err(StatusCode::FORBIDDEN);
        }
    }

    // Only generate cache key for GET requests (memoizable)
    if is_git_smart_http(uri.path()) {
        let method_str = method.as_str().to_string();
        let path_str = uri.path().to_string();
        let result = handle_git_smart_http(&state, method, uri, headers, body).await;
        let duration = timer.elapsed_ms();
        state.metrics.record_request_duration(duration as f64 / 1000.0);
        let response_status = match &result {
            Ok(resp) => resp.status().as_u16(),
            Err(s) => s.as_u16(),
        };
        timer.finish(response_status);
        state.metrics.record_git_operation();
        let template = crate::hotpath::path_template(&path_str);
        let result_label = if response_status >= 500 { "error" } else { "hit" };
        let _ = state.metrics.record_hotpath(
            &template,
            result_label,
            duration as f64 / 1000.0,
        );
        tracing::info!(
            method = %method_str,
            path = %path_str,
            path_template = %template,
            status = response_status,
            result = result_label,
            duration_ms = duration,
            "Git smart HTTP completed"
        );
        return result;
    }

    let cache_key = if should_cache_response(&method, uri.path()) {
        Some(generate_cache_key(&method, &uri))
    } else {
        None
    };

    if let Some(ref cache) = state.cache {
        if let Some(ref cache_key) = cache_key {
            if let Some(entry) = cache.get(cache_key).await {
                state.metrics.record_request_duration(timer.elapsed_ms() as f64 / 1000.0);
                timer.finish(200);
                let mut response_headers = HeaderMap::new();
                response_headers.insert("content-type", entry.content_type.parse().unwrap());
                response_headers.insert("x-cache", "HIT".parse().unwrap());
                // Restore cached headers (cache-control, etag, last-modified, etc.)
                for (key, value) in &entry.headers {
                    if let (Ok(name), Ok(val)) = (key.parse::<HeaderName>(), value.parse::<HeaderValue>()) {
                        // Add stale-while-revalidate to cache-control headers
                        if name.as_str().eq_ignore_ascii_case("cache-control") {
                            let enhanced = format!("{}, stale-while-revalidate=86400", val.to_str().unwrap_or(""));
                            response_headers.insert(name, enhanced.parse().unwrap());
                        } else {
                            response_headers.insert(name, val);
                        }
                    }
                }
                // Generate ETag based on content hash for conditional requests
                let etag = format!("\"{:x}\"", md5::compute(&entry.data));
                response_headers.insert("etag", etag.parse().unwrap());
                // Check If-None-Match header for conditional request
                if let Some(if_none_match) = headers.get("if-none-match") {
                    if let Ok(client_etag) = if_none_match.to_str() {
                        if client_etag == etag || client_etag == "*" {
                            return Ok(StatusCode::NOT_MODIFIED.into_response());
                        }
                    }
                }
                return Ok((StatusCode::OK, response_headers, entry.data).into_response());
            }
        }
    }

    let metrics = state.metrics.clone();
    let method_str = method.as_str().to_string();
    let path_str = uri.path().to_string();
    let used_gitaly = is_git_url(&path_str)
        && state.git.gitaly_address.is_some()
        && state.git.gitaly_token.is_some();
    let used_git_backend = is_git_url(&path_str)
        && !used_gitaly
        && state.proxy.git_backend_url.is_some();
    let via_puma = !used_gitaly && !used_git_backend;
    let backend_started = std::time::Instant::now();
    // Check for git URLs FIRST — route to Gitaly or Go sidecar
    let result = if is_git_url(&path_str) {
        if let (Some(gitaly_addr), Some(gitaly_token)) = (&state.git.gitaly_address, &state.git.gitaly_token) {
            proxy_via_gitaly(&state, method, uri, headers, body, gitaly_addr, gitaly_token).await
        } else if let Some(ref git_backend) = state.proxy.git_backend_url {
            proxy_via_git_backend(&state, method, uri, headers, body, git_backend).await
        } else if let Some(ref socket_path) = state.proxy.auth_socket {
            let socket_path = socket_path.clone();
            proxy_via_unix_socket(&state, method, uri, headers, body, &socket_path).await
        } else {
            proxy_via_tcp(&state, method, uri, headers, body).await
        }
    } else if let Some(ref socket_path) = state.proxy.auth_socket {
        let socket_path = socket_path.clone();
        tracing::debug!(method = %method_str, path = %path_str, socket = %socket_path, "Routing to unix socket");
        proxy_via_unix_socket(&state, method, uri, headers, body, &socket_path).await
    } else {
        tracing::debug!(method = %method_str, path = %path_str, "Routing to TCP");
        proxy_via_tcp(&state, method, uri, headers, body).await
    };

    let duration = timer.elapsed_ms();
    metrics.record_request_duration(duration as f64 / 1000.0);

    let response_status = match &result {
        Ok(resp) => resp.status().as_u16(),
        Err(s) => s.as_u16(),
    };
    let puma_ms = if via_puma {
        backend_started.elapsed().as_millis() as u64
    } else {
        0
    };
    let template = crate::hotpath::path_template(&path_str);
    if via_puma {
        let result_label = if response_status >= 500 {
            "error"
        } else {
            "fallback"
        };
        let snap = metrics.record_hotpath(&template, result_label, puma_ms as f64 / 1000.0);
        if snap.log_aggregate {
            tracing::info!(
                path_template = %template,
                count = snap.count,
                p50_ms = snap.p50_ms,
                p95_ms = snap.p95_ms,
                "Hot path aggregated"
            );
        }
    }

    if let Ok(resp) = result.as_ref() {
        let resp_status = resp.status().as_u16();
        if resp_status >= 500 {
            let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
            tracing::warn!(
                method = %method_str,
                path = %path_str,
                status = resp_status,
                content_type = content_type,
                duration_ms = duration,
                "Backend returned 5xx response"
            );
        }
    }
    timer.finish(response_status);

    if let Ok(ref _response) = result {
        tracing::info!(
            method = %method_str,
            path = %path_str,
            path_template = %template,
            status = response_status,
            duration_ms = duration,
            puma_ms = puma_ms,
            "Proxy request completed"
        );
    }

    result
}

/// Shared response body processing pipeline: handles X-Sendfile, detect-content-type,
/// send-data injection, cache storage, ETag generation, and conditional requests.
#[allow(clippy::too_many_arguments)]
async fn process_response_pipeline(
    state: &AppState,
    method: &Method,
    uri: &Uri,
    request_headers: &HeaderMap,
    status: StatusCode,
    response_headers: &HeaderMap,
    body_bytes: Bytes,
) -> Result<Response, StatusCode> {
    let mut filtered = forwardheaders::forward_response_headers(response_headers, None, &[]);

    let body_bytes = if let Some(sendfile_path) = response_headers.get(crate::headers::X_SENDFILE_HEADER) {
        if let Ok(path) = sendfile_path.to_str() {
            match tokio::fs::canonicalize(path).await {
                Ok(canonical) => {
                    match tokio::fs::read(&canonical).await {
                        Ok(file_data) => {
                            tracing::info!("X-Sendfile served: {} ({} bytes)", canonical.display(), file_data.len());
                            Bytes::from(file_data)
                        }
                        Err(e) => {
                            tracing::error!("X-Sendfile read failed: {}: {}", canonical.display(), e);
                            body_bytes
                        }
                    }
                }
                Err(_) => {
                    tracing::warn!("X-Sendfile path not found: {}", path);
                    body_bytes
                }
            }
        } else {
            body_bytes
        }
    } else if body_bytes.is_empty()
        && crate::headers::is_detect_content_type_header_present(response_headers)
    {
        let file_path = format!("/var/opt/gitlab/gitlab-rails{}", uri.path());
        match tokio::fs::canonicalize(&file_path).await {
            Ok(canonical) => {
                if canonical.starts_with("/var/opt/gitlab/gitlab-rails") {
                    match tokio::fs::read(&canonical).await {
                        Ok(file_data) => {
                            tracing::info!("Served file from disk: {} ({} bytes)", canonical.display(), file_data.len());
                            Bytes::from(file_data)
                        }
                        Err(e) => {
                            tracing::error!("Failed to read file from disk {}: {}", canonical.display(), e);
                            body_bytes
                        }
                    }
                } else {
                    tracing::warn!("Detect-content-type path traversal attempt: {}", file_path);
                    body_bytes
                }
            }
            Err(_) => {
                body_bytes
            }
        }
    } else {
        body_bytes
    };

    if let Ok(body_text) = std::str::from_utf8(&body_bytes) {
        if let Some(inject_response) = senddata::intercept_send_data(
            status,
            &filtered,
            body_text,
            &state.injecters,
        ).await {
            return Ok(inject_response);
        }
    }

    if is_internal_workhorse_json(&filtered) {
        tracing::error!(
            "Refusing to leak workhorse JSON to client for {}",
            uri.path()
        );
        return Err(StatusCode::BAD_GATEWAY);
    }

    if let Some(ref cache) = state.cache {
        if should_cache_response(method, uri.path())
            && status.is_success()
            && response_allows_shared_cache(&filtered)
        {
            let content_type = filtered
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_string();
            let cache_key = generate_cache_key(method, uri);
            let cache_headers: Vec<(String, String)> = filtered
                .iter()
                .filter_map(|(k, v)| {
                    let key = k.as_str().to_lowercase();
                    match key.as_str() {
                        "cache-control" | "etag" | "last-modified" | "content-type"
                        | "x-content-type-options" | "x-frame-options" | "x-xss-protection"
                        | "x-permitted-cross-domain-policies" | "referrer-policy"
                        | "permissions-policy" | "x-ua-compatible" | "x-gitlab-meta"
                        | "x-request-id" | "x-download-options" => {
                            v.to_str().ok().map(|s| (k.as_str().to_string(), s.to_string()))
                        }
                        _ => None,
                    }
                })
                .collect();
            cache.set(cache_key.clone(), body_bytes.clone(), content_type, cache_headers, None).await;
            tracing::info!(cache_key = %cache_key, size = body_bytes.len(), "Cache STORE");
        }
    }

    if *method == Method::GET && status.is_success() && !filtered.contains_key("etag") {
        let etag = format!("\"{:x}\"", md5::compute(&body_bytes));
        filtered.insert("etag", etag.parse().unwrap());
    }

    if *method == Method::GET && status.is_success() && !crate::webide_nls::is_nls_messages_path(uri.path()) {
        if let Some(if_none_match) = request_headers.get("if-none-match") {
            if let Some(etag) = filtered.get("etag") {
                if let (Ok(client_etag), Ok(server_etag)) = (if_none_match.to_str(), etag.to_str()) {
                    if client_etag == server_etag || client_etag == "*" {
                        return Ok(StatusCode::NOT_MODIFIED.into_response());
                    }
                }
            }
        }
    }

    Ok((status, filtered, body_bytes).into_response())
}

async fn proxy_via_tcp(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, StatusCode> {
    // Use path_and_query to preserve query parameters
    let path_and_query = uri.path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());
    let backend_url = format!("{}{}", state.proxy.backend_url, path_and_query);

    // Validate the backend URL for SSRF protection
    if let Ok(parsed_url) = Url::parse(&backend_url) {
        if let Some(host) = parsed_url.host_str() {
            if let Err(e) = transport::validate_address(host, true, &[]) {
                tracing::warn!("SSRF protection blocked request to {}: {}", host, e);
                return Err(StatusCode::FORBIDDEN);
            }
        }
    }

    let mut request = state.proxy.client.request(method.clone(), &backend_url);

    // Forward headers (filter out hop-by-hop headers)
    let mut filtered_headers = forwardheaders::forward_request_headers(&headers, &state.proxy.backend_url);
    filtered_headers.insert("X-Forwarded-Proto", HeaderValue::from_static("https"));
    for (key, value) in filtered_headers.iter() {
        request = request.header(key.as_str(), value.to_str().unwrap_or(""));
    }

    // Add workhorse identification headers
    let mut auth_headers = HeaderMap::new();
    if secret::add_workhorse_headers(&mut auth_headers, &state.secret).is_ok() {
        for (key, value) in auth_headers.iter() {
            request = request.header(key.as_str(), value.to_str().unwrap_or(""));
        }
    }

    // Stream body for methods that support it
    if matches!(method, Method::POST | Method::PUT | Method::PATCH) {
        let byte_stream = body.into_data_stream()
            .map(|r| r.map_err(|e| -> std::io::Error { std::io::Error::new(std::io::ErrorKind::Other, e.to_string()) }));
        request = request.body(reqwest::Body::wrap_stream(byte_stream));
    }

    match request.send().await {
        Ok(response) => {
            if let Some(cb) = &state.proxy.circuit_breaker {
                cb.record_success().await;
            }

            let status = StatusCode::from_u16(response.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

            // Build response headers, filtering through forwardheaders
            let mut response_headers = HeaderMap::new();
            for (key, value) in response.headers() {
                if let Ok(v) = value.to_str() {
                    if let Ok(name) = key.as_str().parse::<HeaderName>() {
                        if let Ok(val) = v.parse::<HeaderValue>() {
                            response_headers.append(name, val);
                        }
                    }
                }
            }

            // Read response body as bytes (preserves binary data)
            let resp_body = response.bytes().await.unwrap_or_default();

            return process_response_pipeline(state, &method, &uri, &headers, status, &response_headers, resp_body).await;
        }
        Err(e) => {
            tracing::error!("Proxy error: {}", e);

            if let Some(cb) = &state.proxy.circuit_breaker {
                cb.record_failure().await;
            }

            // Classify the error type
            if e.is_timeout() {
                Err(StatusCode::GATEWAY_TIMEOUT)
            } else if e.is_connect() {
                Err(StatusCode::BAD_GATEWAY)
            } else {
                Err(StatusCode::BAD_GATEWAY)
            }
        }
    }
}

const UNIX_PROXY_TIMEOUT: Duration = Duration::from_secs(30);

async fn unix_client_request(
    client: &UnixSocketClient,
    req: hyper::Request<http_body_util::Full<Bytes>>,
) -> Result<hyper::Response<hyper::body::Incoming>, StatusCode> {
    match tokio::time::timeout(UNIX_PROXY_TIMEOUT, client.request(req.into())).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => {
            tracing::error!("Unix socket proxy error: {}", e);
            Err(StatusCode::BAD_GATEWAY)
        }
        Err(_) => {
            tracing::error!("Unix socket proxy timeout after {:?}", UNIX_PROXY_TIMEOUT);
            Err(StatusCode::GATEWAY_TIMEOUT)
        }
    }
}

async fn proxy_via_unix_socket(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
    socket_path: &str,
) -> Result<Response, StatusCode> {
    let path = uri.path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let hex_path = hex::encode(socket_path);
    let unix_uri = format!("unix://{}:0{}", hex_path, path);

    let mut request = hyper::Request::builder()
        .method(method.as_str())
        .uri(&unix_uri);

    for (key, value) in headers.iter() {
        if is_hop_by_hop_header(key.as_str()) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            request = request.header(key.as_str(), v);
        }
    }

    let original_host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    request = request.header("host", original_host);
    request = request.header("X-Forwarded-Proto", "https");

    // Add workhorse identification headers (JWT auth)
    let mut auth_headers = HeaderMap::new();
    if secret::add_workhorse_headers(&mut auth_headers, &state.secret).is_ok() {
        for (key, value) in auth_headers.iter() {
            if let Ok(v) = value.to_str() {
                request = request.header(key.as_str(), v);
            }
        }
    }

    // Buffer the entire request body before sending to avoid streaming timing issues
    let body_data = body.collect().await.map_err(|e| {
        tracing::error!("Failed to read request body: {}", e);
        StatusCode::BAD_REQUEST
    })?;
    let body_bytes = body_data.to_bytes();

    let req_body = http_body_util::Full::new(body_bytes);
    let req = request
        .body(req_body)
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let resp = unix_client_request(&state.unix_client, req).await?;

    let resp_status = resp.status().as_u16();
    tracing::info!(
        method = %method.as_str(),
        path = %path,
        resp_status = resp_status,
        resp_headers = ?resp.headers(),
        "Unix socket: got response from backend"
    );

    if let Some(cb) = &state.proxy.circuit_breaker {
        cb.record_success().await;
    }

    let status = StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut response_headers = HeaderMap::new();
    for (key, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            if let Ok(name) = key.as_str().parse::<HeaderName>() {
                if let Ok(val) = v.parse::<HeaderValue>() {
                    response_headers.append(name, val);
                }
            }
        }
    }

    let collected = resp.into_body().collect().await.map_err(|e| {
        tracing::error!("Unix socket body read error: {}", e);
        StatusCode::BAD_GATEWAY
    })?;
    let body_bytes = collected.to_bytes();

    if resp_status >= 500 {
        let body_preview = std::str::from_utf8(&body_bytes).unwrap_or("<binary>");
        tracing::error!(
            method = %method.as_str(),
            path = %path,
            status = resp_status,
            body = %body_preview,
            "Backend 5xx response body"
        );
    }

    process_response_pipeline(state, &method, &uri, &headers, status, &response_headers, body_bytes).await
}

fn strip_secure_from_set_cookie(headers: &mut HeaderMap) {
    let new_cookies: Vec<HeaderValue> = headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| {
            let value = v.to_str().unwrap_or("");
            let stripped = value
                .split(';')
                .map(|part| part.trim())
                .filter(|part| !part.eq_ignore_ascii_case("secure"))
                .collect::<Vec<_>>()
                .join("; ");
            HeaderValue::from_str(&stripped).ok()
        })
        .collect();

    headers.remove("set-cookie");
    for cookie in new_cookies {
        headers.append("set-cookie", cookie);
    }
}

pub async fn proxy_websocket(
    State(state): State<AppState>,
    uri: Uri,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let ws_host = state.proxy.backend_url.host_str().unwrap_or("localhost").to_string();
    let ws_port = state.proxy.backend_url.port().unwrap_or(80);
    let ws_scheme = if state.proxy.backend_url.scheme() == "https" { "wss" } else { "ws" };
    let ws_url = format!("{}://{}:{}{}", ws_scheme, ws_host, ws_port, uri.path());

    let (ws_stream, _) = connect_async(&ws_url).await.map_err(|e| {
        tracing::error!("WebSocket connection failed: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    let (ws_sender, ws_receiver) = ws_stream.split();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(100);

    let tx_clone = tx.clone();
    tokio::spawn(async move {
        let mut ws_receiver = ws_receiver;
        while let Some(msg) = ws_receiver.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if tx_clone.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Binary(bin)) => {
                    if tx_clone.send(Message::Binary(bin)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Ping(ping)) => {
                    if tx_clone.send(Message::Ping(ping)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Pong(pong)) => {
                    if tx_clone.send(Message::Pong(pong)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) => {
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    break;
                }
            }
        }
    });

    tokio::spawn(async move {
        let mut ws_sender = ws_sender;
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut response_headers = HeaderMap::new();
    response_headers.insert("connection", "upgrade".parse().unwrap());
    response_headers.insert("upgrade", "websocket".parse().unwrap());

    Ok((StatusCode::SWITCHING_PROTOCOLS, response_headers, "").into_response())
}

fn is_git_url(path: &str) -> bool {
    path.contains(".git/") || path.ends_with(".git")
}

fn is_git_smart_http(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    is_git_url(path)
        && (path.ends_with("/info/refs")
        || path.ends_with("/git-upload-pack")
        || path.ends_with("/git-receive-pack"))
}

const GIT_UPLOAD_PACK_REQUEST_MAX: usize = 16 * 1024 * 1024;

fn git_status_error(
    status: StatusCode,
    class: &'static str,
    path: &str,
    err: impl std::fmt::Display,
) -> StatusCode {
    tracing::error!(
        error_class = class,
        path = %path,
        status = status.as_u16(),
        error = %err,
        "Git smart HTTP failed"
    );
    status
}

fn body_to_io_stream(
    body: Body,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    body.into_data_stream().map(|chunk| {
        chunk.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    })
}

fn looks_like_git_pktline(body: &[u8]) -> bool {
    body.len() >= 4
        && body[..4].iter().all(|b| b.is_ascii_hexdigit())
        && (body.starts_with(b"001e# service=")
            || body.starts_with(b"001f# service=")
            || body
                .windows(b"# service=git-upload-pack".len())
                .any(|w| w == b"# service=git-upload-pack")
            || body
                .windows(b"# service=git-receive-pack".len())
                .any(|w| w == b"# service=git-receive-pack"))
}

fn workhorse_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.starts_with(crate::api::RESPONSE_CONTENT_TYPE) || v.contains("application/json")
        })
        .unwrap_or(false)
}

fn is_internal_workhorse_json(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with(crate::api::RESPONSE_CONTENT_TYPE))
        .unwrap_or(false)
}

fn repo_from_auth(auth: &crate::api::Response) -> Option<(gitaly::GitalyServer, gitaly::RepoInfo)> {
    let gs = auth.gitaly_server.as_ref()?;
    let repo = auth.repository.as_ref()?;
    if gs.address.is_empty() || repo.storage_name.is_empty() || repo.relative_path.is_empty() {
        return None;
    }
    Some((
        gitaly::GitalyServer {
            address: gs.address.clone(),
            token: gs.token.clone(),
            call_metadata: gs.call_metadata.clone(),
        },
        gitaly::RepoInfo::new(
            &repo.storage_name,
            &repo.relative_path,
            &repo.gl_project_path,
            &repo.gl_repository,
        ),
    ))
}

async fn handle_git_smart_http(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, StatusCode> {
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or("").to_string();
    let receive_pack = path.ends_with("/git-receive-pack");
    let (pack_body, receive_body) = if receive_pack {
        (Bytes::new(), Some(body))
    } else if matches!(method, Method::POST | Method::PUT | Method::PATCH) {
        let bytes = body
            .collect()
            .await
            .map_err(|e| git_status_error(StatusCode::BAD_REQUEST, "body_read", &path, e))?
            .to_bytes();
        if bytes.len() > GIT_UPLOAD_PACK_REQUEST_MAX {
            return Err(git_status_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "upload_pack_request_too_large",
                &path,
                bytes.len(),
            ));
        }
        (bytes, None)
    } else {
        (Bytes::new(), None)
    };

    let (status, auth_headers, auth_body) =
        git_pre_authorize(state, &method, &uri, &headers).await?;

    if !status.is_success() {
        return Ok((status, auth_headers, auth_body).into_response());
    }

    if looks_like_git_pktline(&auth_body) {
        return git_protocol_response(&path, &query, auth_body);
    }

    if !workhorse_json_content_type(&auth_headers) {
        return Err(git_status_error(
            StatusCode::BAD_GATEWAY,
            "authorize_non_json",
            &path,
            "success response was not JSON",
        ));
    }

    let auth: crate::api::Response = serde_json::from_slice(&auth_body).map_err(|e| {
        git_status_error(StatusCode::BAD_GATEWAY, "json_decode", &path, e)
    })?;

    let Some((server, repo)) = repo_from_auth(&auth) else {
        let gs = auth.gitaly_server.as_ref();
        let repo = auth.repository.as_ref();
        return Err(git_status_error(
            StatusCode::BAD_GATEWAY,
            "missing_repo_fields",
            &path,
            format!(
                "addr={} token={} storage={} relpath={} glrepo={}",
                gs.map(|s| s.address.len()).unwrap_or(0),
                gs.map(|s| s.token.len()).unwrap_or(0),
                repo.map(|r| r.storage_name.len()).unwrap_or(0),
                repo.map(|r| r.relative_path.len()).unwrap_or(0),
                auth.gl_repository
            ),
        ));
    };

    serve_git_from_gitaly(
        &path,
        &query,
        pack_body.to_vec(),
        receive_body,
        server,
        repo,
        &auth.gl_id,
        &auth.gl_username,
    )
    .await
}

async fn git_pre_authorize(
    state: &AppState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<(StatusCode, HeaderMap, Bytes), StatusCode> {
    if let Some(ref socket_path) = state.proxy.auth_socket {
        git_pre_authorize_unix(state, method, uri, headers, socket_path).await
    } else {
        git_pre_authorize_tcp(state, method, uri, headers).await
    }
}

async fn git_pre_authorize_unix(
    state: &AppState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    socket_path: &str,
) -> Result<(StatusCode, HeaderMap, Bytes), StatusCode> {
    let path = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());
    let hex_path = hex::encode(socket_path);
    let unix_uri = format!("unix://{}:0{}", hex_path, path);

    let mut request = hyper::Request::builder()
        .method(method.as_str())
        .uri(&unix_uri);

    for (key, value) in headers.iter() {
        let name = key.as_str();
        if matches!(
            name,
            "host" | "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            request = request.header(name, v);
        }
    }

    let original_host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    request = request.header("host", original_host);
    request = request.header("X-Forwarded-Proto", "https");

    let mut auth_headers = HeaderMap::new();
    if secret::add_workhorse_headers(&mut auth_headers, &state.secret).is_ok() {
        for (key, value) in auth_headers.iter() {
            if let Ok(v) = value.to_str() {
                request = request.header(key.as_str(), v);
            }
        }
    }

    let req = request
        .body(http_body_util::Full::new(Bytes::new()))
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let resp = match tokio::time::timeout(
        UNIX_PROXY_TIMEOUT,
        state.unix_client.request(req.into()),
    )
    .await
    {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            return Err(git_status_error(
                StatusCode::BAD_GATEWAY,
                "authorize_unix",
                uri.path(),
                e,
            ));
        }
        Err(_) => {
            return Err(git_status_error(
                StatusCode::GATEWAY_TIMEOUT,
                "authorize_unix_timeout",
                uri.path(),
                "timeout",
            ));
        }
    };

    let status = StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response_headers = HeaderMap::new();
    for (key, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            if let Ok(name) = key.as_str().parse::<HeaderName>() {
                if let Ok(val) = v.parse::<HeaderValue>() {
                    response_headers.append(name, val);
                }
            }
        }
    }
    let body = resp.into_body().collect().await.map_err(|e| {
        git_status_error(StatusCode::BAD_GATEWAY, "authorize_unix_body", uri.path(), e)
    })?.to_bytes();
    Ok((status, response_headers, body))
}

async fn git_pre_authorize_tcp(
    state: &AppState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<(StatusCode, HeaderMap, Bytes), StatusCode> {
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());
    let backend_url = format!("{}{}", state.proxy.backend_url, path_and_query);
    let mut request = state.proxy.client.request(method.clone(), &backend_url);

    let mut filtered = forwardheaders::forward_request_headers(headers, &state.proxy.backend_url);
    filtered.insert("X-Forwarded-Proto", HeaderValue::from_static("https"));
    filtered.remove("content-length");
    for (key, value) in filtered.iter() {
        request = request.header(key.as_str(), value.to_str().unwrap_or(""));
    }
    let mut auth_headers = HeaderMap::new();
    if secret::add_workhorse_headers(&mut auth_headers, &state.secret).is_ok() {
        for (key, value) in auth_headers.iter() {
            request = request.header(key.as_str(), value.to_str().unwrap_or(""));
        }
    }
    request = request.body(Vec::<u8>::new());

    let response = request.send().await.map_err(|e| {
        git_status_error(StatusCode::BAD_GATEWAY, "authorize_tcp", uri.path(), e)
    })?;
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response_headers = HeaderMap::new();
    for (key, value) in response.headers() {
        if let Ok(v) = value.to_str() {
            if let Ok(name) = key.as_str().parse::<HeaderName>() {
                if let Ok(val) = v.parse::<HeaderValue>() {
                    response_headers.append(name, val);
                }
            }
        }
    }
    let body = response.bytes().await.unwrap_or_default();
    Ok((status, response_headers, body))
}

fn git_protocol_response(path: &str, query: &str, body: Bytes) -> Result<Response, StatusCode> {
    let content_type = git_content_type(path, query);
    let mut headers = HeaderMap::new();
    headers.insert("content-type", content_type.parse().unwrap());
    headers.insert("cache-control", "no-cache".parse().unwrap());
    Ok((StatusCode::OK, headers, body).into_response())
}

fn git_content_type(path: &str, query: &str) -> &'static str {
    if path.ends_with("/info/refs") {
        if query.contains("git-receive-pack") {
            "application/x-git-receive-pack-advertisement"
        } else {
            "application/x-git-upload-pack-advertisement"
        }
    } else if path.ends_with("/git-receive-pack") {
        "application/x-git-receive-pack-result"
    } else {
        "application/x-git-upload-pack-result"
    }
}

async fn serve_git_from_gitaly(
    path: &str,
    query: &str,
    pack_body: Vec<u8>,
    receive_body: Option<Body>,
    server: gitaly::GitalyServer,
    repo: gitaly::RepoInfo,
    gl_id: &str,
    gl_username: &str,
) -> Result<Response, StatusCode> {
    let mut client = gitaly::GitalyClient::connect(&server).await.map_err(|e| {
        git_status_error(StatusCode::BAD_GATEWAY, "gitaly_connect", path, e)
    })?;

    if path.ends_with("/info/refs") {
        let result = if query.contains("git-receive-pack") {
            client.info_refs_receive_pack(&repo).await
        } else {
            client.info_refs_upload_pack(&repo).await
        };
        match result {
            Ok(refs_data) => git_protocol_response(path, query, Bytes::from(refs_data)),
            Err(e) => {
                Err(git_status_error(StatusCode::BAD_GATEWAY, "gitaly_info_refs", path, e))
            }
        }
    } else if path.ends_with("/git-upload-pack") {
        match client
            .post_upload_pack_with_sidechannel(&repo, pack_body)
            .await
        {
            Ok(pack_stream) => {
                let mut headers = HeaderMap::new();
                headers.insert("content-type", git_content_type(path, query).parse().unwrap());
                headers.insert("cache-control", "no-cache".parse().unwrap());
                Ok((StatusCode::OK, headers, Body::from_stream(pack_stream)).into_response())
            }
            Err(e) => {
                Err(git_status_error(StatusCode::BAD_GATEWAY, "gitaly_upload_pack", path, e))
            }
        }
    } else if path.ends_with("/git-receive-pack") {
        let stream = body_to_io_stream(receive_body.unwrap_or_else(|| Body::from(pack_body)));
        match client.post_receive_pack(&repo, stream, gl_id, gl_username).await {
            Ok(pack_stream) => {
                let mut headers = HeaderMap::new();
                headers.insert("content-type", git_content_type(path, query).parse().unwrap());
                headers.insert("cache-control", "no-cache".parse().unwrap());
                Ok((StatusCode::OK, headers, Body::from_stream(pack_stream)).into_response())
            }
            Err(e) => Err(git_status_error(
                StatusCode::BAD_GATEWAY,
                "gitaly_receive_pack",
                path,
                e,
            )),
        }
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

/// Proxy to Go workhorse sidecar for Gitaly-backed git operations
async fn proxy_via_git_backend(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
    git_backend_url: &str,
) -> Result<Response, StatusCode> {
    let path_and_query = uri.path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());
    let backend_url = format!("{}{}", git_backend_url.trim_end_matches('/'), path_and_query);

    tracing::info!("Routing git request to Go sidecar: {}", backend_url);

    let mut request = state.proxy.client.request(method.clone(), &backend_url);

    let mut filtered_headers = forwardheaders::forward_request_headers(&headers, &state.proxy.backend_url);
    filtered_headers.insert("X-Forwarded-Proto", HeaderValue::from_static("https"));
    for (key, value) in filtered_headers.iter() {
        request = request.header(key.as_str(), value.to_str().unwrap_or(""));
    }

    // Add workhorse headers (Go workhorse will verify and add its own JWT)
    let mut auth_headers = HeaderMap::new();
    if secret::add_workhorse_headers(&mut auth_headers, &state.secret).is_ok() {
        for (key, value) in auth_headers.iter() {
            request = request.header(key.as_str(), value.to_str().unwrap_or(""));
        }
    }

    // Stream body
    if matches!(method, Method::POST | Method::PUT | Method::PATCH) {
        let byte_stream = body.into_data_stream()
            .map(|r| r.map_err(|e| -> std::io::Error { std::io::Error::new(std::io::ErrorKind::Other, e.to_string()) }));
        request = request.body(reqwest::Body::wrap_stream(byte_stream));
    }

    match request.send().await {
        Ok(response) => {
            let status = StatusCode::from_u16(response.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

            let mut response_headers = HeaderMap::new();
            for (key, value) in response.headers() {
                if let Ok(v) = value.to_str() {
                    if let Ok(name) = key.as_str().parse::<HeaderName>() {
                        if let Ok(val) = v.parse::<HeaderValue>() {
                            response_headers.append(name, val);
                        }
                    }
                }
            }

            let filtered_response_headers = forwardheaders::forward_response_headers(
                &response_headers,
                None,
                &[],
            );

            let resp_body = response.bytes().await.unwrap_or_default();

            tracing::info!(
                status = status.as_u16(),
                size = resp_body.len(),
                "Git sidecar response"
            );

            Ok((status, filtered_response_headers, resp_body).into_response())
        }
        Err(e) => {
            tracing::error!("Git backend proxy error: {}", e);
            if e.is_timeout() {
                Err(StatusCode::GATEWAY_TIMEOUT)
            } else {
                Err(StatusCode::BAD_GATEWAY)
            }
        }
    }
}

/// Proxy git request via direct Gitaly gRPC with sidechannel support
async fn proxy_via_gitaly(
    _state: &AppState,
    _method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
    gitaly_addr: &str,
    gitaly_token: &str,
) -> Result<Response, StatusCode> {
    let path = uri.path();
    let query = uri.query().unwrap_or("");

    tracing::info!("Routing git request to Gitaly: {}?{}", path, query);

    let (storage_name, relative_path) = match extract_repo_path(path) {
        Some(v) => {
            tracing::info!("Parsed repo: storage={}, relative={}", v.0, v.1);
            // TODO: Implement proper auth flow to resolve hashed path
            // For now, look up actual storage path
            let actual_relative = resolve_actual_repo_path(&v.1);
            (v.0, actual_relative)
        }
        None => {
            tracing::error!("Cannot parse repo path from: {}", path);
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    let repo = gitaly::RepoInfo::new(
        &storage_name,
        &relative_path,
        &extract_gl_project_path(path).unwrap_or_else(|| "unknown".to_string()),
        &extract_gl_repository(path).unwrap_or_else(|| "unknown".to_string()),
    );

    tracing::info!("Connecting to Gitaly at {} with token len={}", gitaly_addr, gitaly_token.len());

    let server = gitaly::GitalyServer {
        address: gitaly_addr.to_string(),
        token: gitaly_token.to_string(),
        call_metadata: std::collections::HashMap::new(),
    };

    let mut client = match gitaly::GitalyClient::connect(&server).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to connect to Gitaly: {}", e);
            return Err(StatusCode::BAD_GATEWAY);
        }
    };

    if path.ends_with("/info/refs") {
        let service = if query.contains("git-upload-pack") {
            "git-upload-pack"
        } else if query.contains("git-receive-pack") {
            "git-receive-pack"
        } else {
            tracing::warn!("Unknown git service in info/refs query: {}", query);
            return Err(StatusCode::BAD_REQUEST);
        };

        let result = if service == "git-upload-pack" {
            client.info_refs_upload_pack(&repo).await
        } else {
            client.info_refs_receive_pack(&repo).await
        };

        match result {
            Ok(refs_data) => {
                let advertisement = format_pkt_line_advertisement(service, &refs_data);
                let content_type = if service == "git-upload-pack" {
                    "application/x-git-upload-pack-advertisement"
                } else {
                    "application/x-git-receive-pack-advertisement"
                };
                let mut response_headers = HeaderMap::new();
                response_headers.insert("content-type", content_type.parse().unwrap());
                response_headers.insert("cache-control", "no-cache".parse().unwrap());
                Ok((StatusCode::OK, response_headers, advertisement).into_response())
            }
            Err(e) => {
                tracing::error!("Gitaly info_refs failed: {}", e);
                Err(StatusCode::BAD_GATEWAY)
            }
        }
    } else if path.ends_with("/git-upload-pack") {
        let body_bytes = body.into_data_stream()
            .fold(Vec::new(), |mut acc, chunk| async move {
                if let Ok(data) = chunk {
                    acc.extend_from_slice(&data);
                }
                acc
            })
            .await;

        tracing::info!("git-upload-pack: body_bytes len={}", body_bytes.len());

        match client.post_upload_pack_with_sidechannel(&repo, body_bytes).await {
            Ok(pack_stream) => {
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    "content-type",
                    "application/x-git-upload-pack-result".parse().unwrap(),
                );
                response_headers.insert("cache-control", "no-cache".parse().unwrap());
                Ok((StatusCode::OK, response_headers, Body::from_stream(pack_stream)).into_response())
            }
            Err(e) => {
                tracing::error!("Gitaly post_upload_pack failed: {}", e);
                Err(StatusCode::BAD_GATEWAY)
            }
        }
    } else if path.ends_with("/git-receive-pack") {
        let gl_id = headers
            .get("gitlab-workhorse-gl-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("user-1")
            .to_string();
        let gl_username = headers
            .get("gitlab-workhorse-gl-username")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("root")
            .to_string();
        match client
            .post_receive_pack(&repo, body_to_io_stream(body), &gl_id, &gl_username)
            .await
        {
            Ok(pack_stream) => {
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    "content-type",
                    "application/x-git-receive-pack-result".parse().unwrap(),
                );
                response_headers.insert("cache-control", "no-cache".parse().unwrap());
                Ok((
                    StatusCode::OK,
                    response_headers,
                    Body::from_stream(pack_stream),
                )
                    .into_response())
            }
            Err(e) => {
                tracing::error!("Gitaly post_receive_pack failed: {}", e);
                Err(StatusCode::BAD_GATEWAY)
            }
        }
    } else {
        tracing::warn!("Unknown git endpoint: {}", path);
        Err(StatusCode::NOT_FOUND)
    }
}

/// Extract storage_name and relative_path from a git URL path
/// e.g., "/root/test-repo.git/info/refs?service=git-upload-pack" -> Some(("default", "root/test-repo.git"))
fn extract_repo_path(path: &str) -> Option<(String, String)> {
    let clean = path.trim_start_matches('/');

    let clean = if let Some(pos) = clean.find('?') {
        &clean[..pos]
    } else {
        clean
    };

    let clean = clean
        .trim_end_matches("/info/refs")
        .trim_end_matches("/git-upload-pack")
        .trim_end_matches("/git-receive-pack");

    if clean.is_empty() {
        return None;
    }

    if !clean.ends_with(".git") {
        return None;
    }

    let storage_name = "default".to_string();
    Some((storage_name, clean.to_string()))
}

fn extract_gl_project_path(path: &str) -> Option<String> {
    let clean = path.trim_start_matches('/')
        .trim_end_matches("/info/refs")
        .trim_end_matches("/git-upload-pack")
        .trim_end_matches("/git-receive-pack")
        .trim_end_matches(".git");
    if clean.is_empty() {
        return None;
    }
    Some(format!("/{}", clean))
}

fn extract_gl_repository(path: &str) -> Option<String> {
    let clean = path.trim_start_matches('/');
    if let Some(last_slash) = clean.rfind('/') {
        let project_part = &clean[last_slash + 1..];
        let project_part = project_part.trim_end_matches(".git");
        if !project_part.is_empty() {
            return Some(project_part.to_string());
        }
    }
    let project_part = clean.trim_end_matches(".git");
    if !project_part.is_empty() {
        return Some(project_part.to_string());
    }
    None
}

/// Resolve a virtual repo path (e.g. "root/test-project-1.git") to its actual hashed storage path.
/// TODO: Replace with proper auth flow through Rails backend.
fn resolve_actual_repo_path(url_path: &str) -> String {
    // TODO: Query Rails backend to get actual hashed path
    // For now, use a simple mapping based on known projects
    match url_path {
        "root/test-project-1.git" => "@hashed/4a/44/4a44dc15364204a80fe80e9039455cc1608281820fe2b24f1e5233ade6af1dd5.git".to_string(),
        "toaru/gitlab-rust.git" => "@hashed/19/58/19581e27de7ced00ff1ce50b2047e7a567c76b1cbaebabe5ef03f7c3017bb5b7.git".to_string(),
        "toaru/xiaomi-switch.git" => "@hashed/ef/2d/ef2d127de37b942baad06145e54b0c619a1f22327b2ebbcfbec78f5564afe39d.git".to_string(),
        "toaru/novel.git" => "@hashed/79/02/7902699be42c8a8e46fbbb4501726517e86b22c56a189f7625a6da49081b2451.git".to_string(),
        "toaru/sgk-information-retriever.git" => "@hashed/6b/86/6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b.git".to_string(),
        "toaru/seed-vc-pro.git" => "@hashed/d4/73/d4735e3a265e16eee03f59718b9b5d03019c07d8b6c51f90da3a666eec13ab35.git".to_string(),
        "toaru/index-tts-pro.git" => "@hashed/4e/07/4e07408562bedb8b60ce05c1decfe3ad16b72230967de01f640b7e4729b49fce.git".to_string(),
        "toaru/halo-auto-push.git" => "@hashed/4b/22/4b227777d4dd1fc61c6f884f48641d02b4d121d3fd328cb08b5531fcacdabf8a.git".to_string(),
        "toaru/maple-os.git" => "@hashed/e7/f6/e7f6c011776e8db7cd330b54174fd76f7d0216b612387a5ffcfb81e6f0919683.git".to_string(),
        _ => url_path.to_string(),
    }
}

/// Format git pkt-line advertisement response for info/refs
/// Gitaly's InfoRefsResponse already includes the full pkt-line formatted data.
fn format_pkt_line_advertisement(_service: &str, refs_data: &[u8]) -> Vec<u8> {
    // Gitaly already returns properly formatted pkt-line data including the
    // "# service=..." header and flush-pkt separator.
    refs_data.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_pool_default() {
        let pool = ConnectionPool::default();
        assert_eq!(pool.max_connections, 100);
        assert_eq!(pool.max_idle_per_host, 10);
        assert_eq!(pool.idle_timeout, Duration::from_secs(90));
        assert_eq!(pool.connect_timeout, Duration::from_secs(10));
        assert_eq!(pool.request_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_circuit_breaker_new() {
        let cb = CircuitBreaker::new(5, Duration::from_secs(60));
        assert_eq!(cb.max_failures, 5);
        assert_eq!(cb.timeout, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn test_circuit_breaker_initially_closed() {
        let cb = CircuitBreaker::new(5, Duration::from_secs(60));
        assert!(!cb.is_open().await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_opens_after_failures() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(60));
        cb.record_failure().await;
        cb.record_failure().await;
        cb.record_failure().await;
        assert!(cb.is_open().await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_resets_on_success() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(60));
        cb.record_failure().await;
        cb.record_failure().await;
        cb.record_success().await;
        assert!(!cb.is_open().await);
    }

    #[test]
    fn test_generate_cache_key_static_asset_strips_version() {
        let method = Method::GET;
        let uri: Uri = "/assets/app.js?v=12345".parse().unwrap();
        let key = generate_cache_key(&method, &uri);
        assert_eq!(key, "GET:/assets/app.js");
    }

    #[test]
    fn test_generate_cache_key_static_asset_keeps_other_params() {
        let method = Method::GET;
        let uri: Uri = "/assets/app.js?v=12345&lang=en".parse().unwrap();
        let key = generate_cache_key(&method, &uri);
        assert_eq!(key, "GET:/assets/app.js?lang=en");
    }

    #[test]
    fn test_generate_cache_key_api_keeps_query() {
        let method = Method::GET;
        let uri: Uri = "/api/v4/projects?page=1".parse().unwrap();
        let key = generate_cache_key(&method, &uri);
        assert_eq!(key, "GET:/api/v4/projects?page=1");
    }

    #[test]
    fn test_generate_cache_key_css_asset() {
        let method = Method::GET;
        let uri: Uri = "/assets/style.css?cb=abc123".parse().unwrap();
        let key = generate_cache_key(&method, &uri);
        assert_eq!(key, "GET:/assets/style.css");
    }

    #[test]
    fn test_generate_cache_key_image_asset() {
        let method = Method::GET;
        let uri: Uri = "/uploads/avatar.png?_=1234567890".parse().unwrap();
        let key = generate_cache_key(&method, &uri);
        assert_eq!(key, "GET:/uploads/avatar.png");
    }

    #[test]
    fn test_generate_cache_key_no_query() {
        let method = Method::GET;
        let uri: Uri = "/assets/app.js".parse().unwrap();
        let key = generate_cache_key(&method, &uri);
        assert_eq!(key, "GET:/assets/app.js");
    }

    #[test]
    fn test_block_graphql_get_mutation() {
        let uri: Uri = "/api/graphql?query=mutation%7Bfoo%7D".parse().unwrap();
        assert!(is_blocked_graphql_get_mutation(&Method::GET, &uri, &HeaderMap::new()));
    }

    #[test]
    fn test_allow_graphql_get_query() {
        let uri: Uri = "/api/graphql?query=%7BcurrentUser%7Bid%7D%7D".parse().unwrap();
        assert!(!is_blocked_graphql_get_mutation(&Method::GET, &uri, &HeaderMap::new()));
    }

    #[test]
    fn test_allow_graphql_post_mutation() {
        let uri: Uri = "/api/graphql".parse().unwrap();
        assert!(!is_blocked_graphql_get_mutation(&Method::POST, &uri, &HeaderMap::new()));
    }

    #[test]
    fn test_allow_graphql_websocket_get() {
        let uri: Uri = "/api/graphql?query=mutation%7Bfoo%7D".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", HeaderValue::from_static("websocket"));
        assert!(!is_blocked_graphql_get_mutation(&Method::GET, &uri, &headers));
    }

    #[test]
    fn test_skip_cache_for_graphql_and_api() {
        assert!(!should_cache_response(&Method::GET, "/api/graphql"));
        assert!(!should_cache_response(&Method::GET, "/api/v4/projects"));
        assert!(!should_cache_response(&Method::POST, "/api/graphql"));
        assert!(should_cache_response(&Method::GET, "/assets/app.js"));
    }

    #[test]
    fn test_skip_cache_for_html_login_and_nls() {
        assert!(!should_cache_response(&Method::GET, "/users/sign_in"));
        assert!(!should_cache_response(&Method::GET, "/"));
        assert!(!should_cache_response(
            &Method::GET,
            "/assets/webpack/vscode/out/nls.messages.js"
        ));
        assert!(should_cache_response(&Method::GET, "/assets/application.css"));
    }

    #[test]
    fn test_response_allows_shared_cache() {
        let mut headers = HeaderMap::new();
        headers.insert("cache-control", HeaderValue::from_static("public, max-age=3600"));
        assert!(response_allows_shared_cache(&headers));

        headers.insert(
            "cache-control",
            HeaderValue::from_static("max-age=0, private, must-revalidate"),
        );
        assert!(!response_allows_shared_cache(&headers));

        let mut cookie_headers = HeaderMap::new();
        cookie_headers.insert("set-cookie", HeaderValue::from_static("_gitlab_session=abc"));
        assert!(!response_allows_shared_cache(&cookie_headers));
    }

    #[test]
    fn test_git_pktline_is_kept_as_raw_bytes() {
        let body = Bytes::from_static(b"001e# service=git-upload-pack\n0000");
        assert!(looks_like_git_pktline(&body));
        assert_eq!(body.as_ref(), b"001e# service=git-upload-pack\n0000");
    }

    #[test]
    fn test_repo_from_auth_rejects_incomplete_gitaly_data() {
        let auth: crate::api::Response = serde_json::from_str(
            r#"{
                "GitalyServer": {"address": "unix:/var/opt/gitlab/gitaly/gitaly.socket", "token": ""},
                "Repository": {
                    "storage_name": "default",
                    "relative_path": "",
                    "gl_project_path": "group/repo"
                }
            }"#,
        )
        .unwrap();

        assert!(repo_from_auth(&auth).is_none());
    }

    #[test]
    fn test_repo_from_auth_accepts_empty_gitaly_token() {
        let auth: crate::api::Response = serde_json::from_str(
            r#"{
                "GitalyServer": {"address": "unix:/var/opt/gitlab/gitaly/gitaly.socket", "token": ""},
                "Repository": {
                    "storage_name": "default",
                    "relative_path": "@hashed/repo.git",
                    "gl_project_path": "group/repo",
                    "gl_repository": "project-7"
                }
            }"#,
        )
        .unwrap();

        assert!(repo_from_auth(&auth).is_some());
    }

    #[test]
    fn test_repo_from_auth_accepts_complete_gitaly_data() {
        let auth: crate::api::Response = serde_json::from_str(
            r#"{
                "GitalyServer": {"address": "127.0.0.1:8075", "token": "token"},
                "Repository": {
                    "storage_name": "default",
                    "relative_path": "@hashed/repo.git",
                    "gl_project_path": "group/repo",
                    "gl_repository": "project-7"
                }
            }"#,
        )
        .unwrap();

        assert!(repo_from_auth(&auth).is_some());
    }

    #[test]
    fn test_is_git_smart_http_matches_wiki_and_snippets() {
        assert!(is_git_smart_http("/group/repo.git/info/refs"));
        assert!(is_git_smart_http("/group/repo.wiki.git/info/refs"));
        assert!(is_git_smart_http("/snippets/1.git/git-upload-pack"));
        assert!(is_git_smart_http("/group/repo.git/git-receive-pack"));
        assert!(!is_git_smart_http("/group/repo.git/info/lfs"));
        assert!(!is_git_smart_http("/api/v4/projects"));
    }

    #[test]
    fn test_is_internal_workhorse_json_ignores_plain_json() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        assert!(!is_internal_workhorse_json(&headers));

        headers.insert(
            "content-type",
            "application/vnd.gitlab-workhorse+json".parse().unwrap(),
        );
        assert!(is_internal_workhorse_json(&headers));
    }
}
