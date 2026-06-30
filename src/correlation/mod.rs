#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    http::{HeaderName, HeaderValue, Request},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

pub const CORRELATION_ID_HEADER: &str = "x-request-id";
pub const FORWARDED_REQUEST_ID_HEADER: &str = "x-forwarded-request-id";

static CORRELATION_ID_HEADER_NAME: HeaderName = HeaderName::from_static("x-request-id");

pub fn generate_correlation_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn extract_correlation_id(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(&CORRELATION_ID_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

pub async fn correlation_id_middleware(
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let correlation_id = extract_correlation_id(req.headers())
        .unwrap_or_else(generate_correlation_id);

    if let Ok(val) = HeaderValue::from_str(&correlation_id) {
        req.headers_mut().insert(&CORRELATION_ID_HEADER_NAME, val);
    }

    let mut response = next.run(req).await;

    if let Ok(val) = HeaderValue::from_str(&correlation_id) {
        response.headers_mut().insert(&CORRELATION_ID_HEADER_NAME, val);
    }

    response
}

pub fn add_workhorse_headers(
    headers: &mut axum::http::HeaderMap,
    version: &str,
    start_time: std::time::Instant,
) {
    if let Ok(val) = HeaderValue::from_str(&format!("gitlab-workhorse-rs/{}", version)) {
        headers.insert(HeaderName::from_static("gitlab-workhorse"), val);
    }

    let elapsed_nanos = start_time.elapsed().as_nanos();
    if let Ok(val) = HeaderValue::from_str(&elapsed_nanos.to_string()) {
        headers.insert(
            HeaderName::from_static("gitlab-workhorse-proxy-start"),
            val,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_correlation_id() {
        let id = generate_correlation_id();
        assert!(!id.is_empty());
        assert_eq!(id.len(), 36); // UUID format
    }

    #[test]
    fn test_extract_correlation_id() {
        let mut headers = axum::http::HeaderMap::new();
        assert!(extract_correlation_id(&headers).is_none());

        headers.insert(
            &CORRELATION_ID_HEADER_NAME,
            HeaderValue::from_static("test-id-123"),
        );
        assert_eq!(
            extract_correlation_id(&headers).unwrap(),
            "test-id-123"
        );
    }

    #[test]
    fn test_extract_correlation_id_invalid() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            &CORRELATION_ID_HEADER_NAME,
            HeaderValue::from_bytes(b"\xff\xfe").unwrap(),
        );
        assert!(extract_correlation_id(&headers).is_none());
    }
}
