#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

static REJECTED_REQUESTS_COUNT: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(0));

const ACCEPTED_METHODS: &[Method] = &[
    Method::GET,
    Method::HEAD,
    Method::POST,
    Method::PUT,
    Method::PATCH,
    Method::DELETE,
    Method::OPTIONS,
    Method::TRACE,
];

pub fn is_method_accepted(method: &Method) -> bool {
    ACCEPTED_METHODS.contains(method)
}

pub fn rejected_requests_count() -> u64 {
    REJECTED_REQUESTS_COUNT.load(Ordering::Relaxed)
}

pub async fn reject_methods_middleware(
    req: axum::http::Request<Body>,
    next: Next,
) -> Response {
    if is_method_accepted(req.method()) {
        next.run(req).await
    } else {
        REJECTED_REQUESTS_COUNT.fetch_add(1, Ordering::Relaxed);
        (StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accepted_methods() {
        assert!(is_method_accepted(&Method::GET));
        assert!(is_method_accepted(&Method::HEAD));
        assert!(is_method_accepted(&Method::POST));
        assert!(is_method_accepted(&Method::PUT));
        assert!(is_method_accepted(&Method::PATCH));
        assert!(is_method_accepted(&Method::DELETE));
        assert!(is_method_accepted(&Method::OPTIONS));
        assert!(is_method_accepted(&Method::TRACE));
    }

    #[test]
    fn test_rejected_methods() {
        let connect = Method::from_bytes(b"CONNECT").unwrap();
        assert!(!is_method_accepted(&connect));

        let custom = Method::from_bytes(b"CUSTOM").unwrap();
        assert!(!is_method_accepted(&custom));
    }

    #[test]
    fn test_rejected_count() {
        let initial = rejected_requests_count();
        REJECTED_REQUESTS_COUNT.fetch_add(1, Ordering::Relaxed);
        assert_eq!(rejected_requests_count(), initial + 1);
    }
}
