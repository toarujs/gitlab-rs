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
use super::secret;
use super::senddata;
use super::state::AppState;
use super::transport;

#[derive(Debug, Clone)]
pub struct ProxyState {
    pub backend_url: Url,
    pub client: Client,
    pub auth_socket: Option<String>,
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

/// Main proxy handler - extracts body from request and forwards it
pub async fn proxy_handler(
    State(state): State<AppState>,
    req: axum::http::Request<Body>,
) -> Result<Response, StatusCode> {
    let device = req.extensions().get::<crate::device_detection::DeviceClass>().copied();
    let is_mobile = device.map(|d| d.is_mobile_or_tablet()).unwrap_or(false);
    let method = req.method().clone();
    let method_str = method.to_string();
    let uri = req.uri().clone();
    let path_str = uri.path().to_string();
    let headers = req.headers().clone();
    let client_accepts_webp = crate::imageresizer::WebPConverter::supports_webp(&headers);
    let best_format = crate::imageresizer::WebPConverter::best_supported_format(&headers);

    let body_bytes = req.into_body().collect().await
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .to_bytes();

    let result = proxy_request_with_bytes(State(state.clone()), method.clone(), uri, headers, body_bytes).await;

    match result {
        Ok(mut response) => {
            let original_status = response.status();

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
                let new_response = if is_mobile {
                    crate::html_injection::inject_into_response(response).await
                } else {
                    crate::html_injection::inject_lang_switcher(response).await
                };
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
    let body_bytes = Bytes::from(body);
    proxy_request_with_bytes(State(state), method, uri, headers, body_bytes).await
}

/// Core proxy function that handles bytes body
pub async fn proxy_request_with_bytes(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
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

    let cache_key = format!("{}:{}", method.as_str(), uri.path());

    if let Some(ref cache) = state.cache {
        if method == Method::GET {
            if let Some(entry) = cache.get(&cache_key).await {
                state.metrics.record_request_duration(timer.elapsed_ms() as f64 / 1000.0);
                timer.finish(200);
                let mut response_headers = HeaderMap::new();
                response_headers.insert("content-type", entry.content_type.parse().unwrap());
                response_headers.insert("x-cache", "HIT".parse().unwrap());
                return Ok((StatusCode::OK, response_headers, entry.data.to_vec()).into_response());
            }
        }
    }

    let metrics = state.metrics.clone();
    let method_str = method.as_str().to_string();
    let path_str = uri.path().to_string();
    let result = if let Some(ref socket_path) = state.proxy.auth_socket {
        let socket_path = socket_path.clone();
        proxy_via_unix_socket(&state, method, uri, headers, body, &socket_path).await
    } else {
        proxy_via_tcp(&state, method, uri, headers, body).await
    };

    let duration = timer.elapsed_ms();
    metrics.record_request_duration(duration as f64 / 1000.0);

    let response_status = match &result {
        Ok(resp) => resp.status().as_u16(),
        Err(s) => s.as_u16(),
    };
    timer.finish(response_status);

    if let Ok(ref response) = result {
        tracing::info!(
            method = %method_str,
            path = %path_str,
            status = response_status,
            duration_ms = duration,
            "Proxy request completed"
        );
    }

    result
}

async fn proxy_via_tcp(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
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
    if let Some(ref secret) = state.secret {
        let mut auth_headers = HeaderMap::new();
        if secret::add_workhorse_headers(&mut auth_headers, secret).is_ok() {
            for (key, value) in auth_headers.iter() {
                request = request.header(key.as_str(), value.to_str().unwrap_or(""));
            }
        }
    }

    // Forward body for methods that support it
    if matches!(method, Method::POST | Method::PUT | Method::PATCH) && !body.is_empty() {
        request = request.body(body);
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

            // Apply forwardheaders filtering to response
            let filtered_response_headers = forwardheaders::forward_response_headers(
                &response_headers,
                None,
                &[],
            );

            // Read response body as bytes (preserves binary data)
            let resp_body = response.bytes().await.unwrap_or_default();

            // Check for send-data injection (only for text responses)
            if let Ok(body_text) = std::str::from_utf8(&resp_body) {
                if let Some(inject_response) = senddata::intercept_send_data(
                    status,
                    &filtered_response_headers,
                    body_text,
                    &state.injecters,
                ).await {
                    return Ok(inject_response);
                }
            }

            // Cache successful GET responses
            if let Some(ref cache) = state.cache {
                if method == Method::GET && status.is_success() {
                    let content_type = filtered_response_headers
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("application/octet-stream")
                        .to_string();
                    let cache_key = format!("{}:{}", method.as_str(), uri.path());
                    cache.set(cache_key, resp_body.clone(), content_type, None).await;
                }
            }

            let filtered_response_headers = filtered_response_headers;

            Ok((status, filtered_response_headers, resp_body).into_response())
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

async fn proxy_via_unix_socket(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    socket_path: &str,
) -> Result<Response, StatusCode> {
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;
    use hyperlocal::UnixConnector;

    let client: Client<UnixConnector, http_body_util::Full<Bytes>> = Client::builder(TokioExecutor::new()).build(UnixConnector);

    let path = uri.path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let hex_path = hex::encode(socket_path);
    let unix_uri = format!("unix://{}:0{}", hex_path, path);

    let mut request = hyper::Request::builder()
        .method(method.as_str())
        .uri(&unix_uri);

    for (key, value) in headers.iter() {
        if let Ok(v) = value.to_str() {
            if key != "host" {
                request = request.header(key.as_str(), v);
            }
        }
    }

    let original_host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    request = request.header("host", original_host);
    request = request.header("X-Forwarded-Proto", "https");

    let req = request
        .body(http_body_util::Full::new(body))
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let resp = client.request(req.into()).await.map_err(|e| {
        tracing::error!("Unix socket proxy error: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

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

    // Apply forwardheaders filtering
    let filtered_response_headers = forwardheaders::forward_response_headers(
        &response_headers,
        None,
        &[],
    );

    let collected = resp.into_body().collect().await.map_err(|e| {
        tracing::error!("Unix socket body read error: {}", e);
        StatusCode::BAD_GATEWAY
    })?;
    let body_bytes = collected.to_bytes();

    // Check for send-data injection (only for text responses)
    if let Ok(body_text) = std::str::from_utf8(&body_bytes) {
        if let Some(inject_response) = senddata::intercept_send_data(
            status,
            &filtered_response_headers,
            body_text,
            &state.injecters,
        ).await {
            return Ok(inject_response);
        }
    }

    let filtered_response_headers = filtered_response_headers;

    Ok((status, filtered_response_headers, body_bytes).into_response())
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
    let ws_url = format!("ws://{}:{}{}", ws_host, ws_port, uri.path());

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
}
