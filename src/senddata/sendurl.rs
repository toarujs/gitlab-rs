#![allow(dead_code)]
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::time::Duration;

/// Send-URL parameters compatible with Go Workhorse format
/// Go uses PascalCase field names, so we support both via aliases
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SendUrlParams {
    pub url: String,
    #[serde(default, alias = "Method")]
    pub method: Option<String>,
    #[serde(default, alias = "AllowRedirects")]
    pub allow_redirects: Option<bool>,
    #[serde(default, alias = "AllowLocalhost")]
    pub allow_localhost: Option<bool>,
    #[serde(default, alias = "AllowedEndpoints")]
    pub allowed_endpoints: Option<Vec<String>>,
    #[serde(default, alias = "SSRFFilter")]
    pub ssrf_filter: Option<bool>,
    #[serde(default, alias = "DialTimeout")]
    pub dial_timeout: Option<String>,
    #[serde(default, alias = "ResponseHeaderTimeout")]
    pub response_header_timeout: Option<String>,
    #[serde(default, alias = "ErrorResponseStatus")]
    pub error_response_status: Option<u16>,
    #[serde(default, alias = "TimeoutResponseStatus")]
    pub timeout_response_status: Option<u16>,
    #[serde(default, alias = "Body")]
    pub body: Option<String>,
    #[serde(default, alias = "Header")]
    pub header: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(default, alias = "ResponseHeaders")]
    pub response_headers: Option<std::collections::HashMap<String, String>>,
}

/// Parse Go duration format (e.g., "10s", "1m30s", "500ms") to seconds
fn parse_go_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.ends_with("ms") {
        let ms: f64 = s[..s.len() - 2].parse().ok()?;
        Some((ms / 1000.0).ceil() as u64)
    } else if s.ends_with('s') {
        let secs: f64 = s[..s.len() - 1].parse().ok()?;
        Some(secs.ceil() as u64)
    } else if s.ends_with('m') {
        let mins: f64 = s[..s.len() - 1].parse().ok()?;
        Some((mins * 60.0).ceil() as u64)
    } else if s.ends_with('h') {
        let hours: f64 = s[..s.len() - 1].parse().ok()?;
        Some((hours * 3600.0).ceil() as u64)
    } else {
        let secs: f64 = s.parse().ok()?;
        Some(secs.ceil() as u64)
    }
}

fn is_ssrf_safe(url_str: &str) -> bool {
    if let Ok(parsed) = url::Url::parse(url_str) {
        let host = parsed.host_str().unwrap_or("");

        if host.is_empty() {
            return false;
        }

        let blocked_hosts = [
            "127.0.0.1", "0.0.0.0", "::1",
            "localhost", "localhost.localdomain",
            "169.254.169.254",
        ];

        if blocked_hosts.contains(&host) {
            return false;
        }

        if host.starts_with("127.") || host.starts_with("10.") || host.starts_with("192.168.") {
            return false;
        }

        if let Some(first_octet) = host.split('.').next().and_then(|s| s.parse::<u16>().ok()) {
            if first_octet == 172 && host.split('.').nth(1).and_then(|s| s.parse::<u16>().ok()).map_or(false, |o| (16..=31).contains(&o)) {
                return false;
            }
        }

        if parsed.port().map_or(false, |p| {
            matches!(p, 22 | 25 | 465 | 587 | 3306 | 5432 | 6379 | 11211 | 27017)
        }) {
            return false;
        }

        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return false;
        }
    }
    true
}

pub async fn send_url_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: SendUrlParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse send-url params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    if !is_ssrf_safe(&params.url) {
        tracing::warn!("SSRF blocked: {}", params.url);
        return Err(StatusCode::FORBIDDEN);
    }

    let method = params.method.as_deref().unwrap_or("GET");
    // Parse timeout from Go duration format (e.g., "10s", "1m30s")
    let timeout = params.response_header_timeout.as_deref()
        .and_then(|s| parse_go_duration(s))
        .unwrap_or(30);
    let allow_redirects = params.allow_redirects.unwrap_or(true);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .redirect(if allow_redirects {
            // Use custom redirect policy to validate each redirect target
            reqwest::redirect::Policy::custom(|attempt| {
                let url = attempt.url();
                if !is_ssrf_safe(url.as_str()) {
                    tracing::warn!("SSRF redirect blocked: {}", url);
                    attempt.stop()
                } else if attempt.previous().len() >= 5 {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            })
        } else {
            reqwest::redirect::Policy::none()
        })
        .no_proxy()
        .build()
        .map_err(|e| {
            tracing::error!("Failed to create send-url client: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let req = match method.to_uppercase().as_str() {
        "GET" => client.get(&params.url),
        "POST" => client.post(&params.url),
        "PUT" => client.put(&params.url),
        "DELETE" => client.delete(&params.url),
        "HEAD" => client.head(&params.url),
        _ => return Err(StatusCode::METHOD_NOT_ALLOWED),
    };

    let response = req.send().await.map_err(|e| {
        tracing::error!("Send-url request failed: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    let status = axum::http::StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut response_headers = HeaderMap::new();
    for (key, value) in response.headers() {
        if let Ok(name) = key.to_string().parse::<axum::http::HeaderName>() {
            if let Ok(val) = value.to_str().unwrap_or("").parse::<axum::http::HeaderValue>() {
                response_headers.insert(name, val);
            }
        }
    }

    let body = response.bytes().await.map_err(|e| {
        tracing::error!("Failed to read send-url body: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    Ok((status, response_headers, body.to_vec()).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssrf_safe_urls() {
        assert!(is_ssrf_safe("https://example.com/file.txt"));
        assert!(is_ssrf_safe("http://gitlab.com/api/v4/test"));
        assert!(is_ssrf_safe("https://cdn.example.com/assets/image.png"));
    }

    #[test]
    fn test_ssrf_block_localhost() {
        assert!(!is_ssrf_safe("http://127.0.0.1:8080/admin"));
        assert!(!is_ssrf_safe("http://localhost/test"));
        assert!(!is_ssrf_safe("https://127.0.0.1/secret"));
    }

    #[test]
    fn test_ssrf_block_private_ips() {
        assert!(!is_ssrf_safe("http://10.0.0.1/api"));
        assert!(!is_ssrf_safe("http://192.168.1.1/admin"));
        assert!(!is_ssrf_safe("http://172.16.0.1/test"));
    }

    #[test]
    fn test_ssrf_block_metadata() {
        assert!(!is_ssrf_safe("http://169.254.169.254/latest/meta-data"));
    }

    #[test]
    fn test_ssrf_block_dangerous_ports() {
        assert!(!is_ssrf_safe("http://example.com:3306/"));
        assert!(!is_ssrf_safe("http://example.com:6379/"));
        assert!(!is_ssrf_safe("http://example.com:22/"));
    }

    #[test]
    fn test_ssrf_block_non_http_scheme() {
        assert!(!is_ssrf_safe("file:///etc/passwd"));
        assert!(!is_ssrf_safe("ftp://example.com/file"));
        assert!(!is_ssrf_safe("gopher://localhost/test"));
    }
}
