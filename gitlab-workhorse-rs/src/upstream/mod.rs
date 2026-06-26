#![allow(dead_code, unused_imports)]
pub mod notfoundunless;

use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use flate2::read::GzDecoder;
use std::io::Read;

#[derive(Debug, Clone)]
pub struct RouteMetadata {
    pub regexp_str: String,
    pub route_id: String,
    pub backend_id: RouteBackend,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RouteBackend {
    Self_,
    Rails,
    Gitaly,
    GeoPrimary,
}

#[derive(Debug, Clone)]
pub struct RouteOptions {
    pub tracing: bool,
    pub is_geo_proxy_route: bool,
    pub body_limit: i64,
}

impl Default for RouteOptions {
    fn default() -> Self {
        Self {
            tracing: true,
            is_geo_proxy_route: false,
            body_limit: 100 * 1024 * 1024,
        }
    }
}

impl RouteMetadata {
    pub fn new(regexp_str: impl Into<String>, route_id: impl Into<String>, backend_id: RouteBackend) -> Self {
        Self {
            regexp_str: regexp_str.into(),
            route_id: route_id.into(),
            backend_id,
        }
    }
}

pub async fn content_encoding_handler(req: Request, next: Next) -> Result<Response, StatusCode> {
    let content_encoding = req
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if content_encoding.is_empty() {
        return Ok(next.run(req).await);
    }

    if content_encoding == "gzip" {
        let (parts, body) = req.into_parts();
        let body_bytes = axum::body::to_bytes(body, 10 * 1024 * 1024)
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;

        let mut decoder = GzDecoder::new(&body_bytes[..]);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .map_err(|_| StatusCode::BAD_REQUEST)?;

        let mut new_req = Request::from_parts(parts, Body::from(decompressed));
        new_req.headers_mut().remove("content-encoding");
        return Ok(next.run(new_req).await);
    }

    Err(StatusCode::BAD_REQUEST)
}

pub fn deny_websocket(req: &Request) -> bool {
    let upgrade = req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase());

    let connection = req
        .headers()
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase());

    if upgrade.as_deref() == Some("websocket") {
        return false;
    }

    if connection.map_or(false, |c| c.contains("upgrade")) {
        return false;
    }

    true
}

pub fn require_websocket(req: &Request) -> bool {
    let upgrade = req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase());

    upgrade.as_deref() == Some("websocket")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};

    #[test]
    fn test_route_metadata_new() {
        let meta = RouteMetadata::new("/api/.*", "api", RouteBackend::Rails);
        assert_eq!(meta.regexp_str, "/api/.*");
        assert_eq!(meta.route_id, "api");
        assert_eq!(meta.backend_id, RouteBackend::Rails);
    }

    #[test]
    fn test_route_options_default() {
        let opts = RouteOptions::default();
        assert!(opts.tracing);
        assert!(!opts.is_geo_proxy_route);
        assert_eq!(opts.body_limit, 100 * 1024 * 1024);
    }

    #[test]
    fn test_deny_websocket_normal_request() {
        let req = Request::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap();
        assert!(deny_websocket(&req));
    }

    #[test]
    fn test_deny_websocket_websocket_request() {
        let req = Request::builder()
            .uri("/")
            .header("upgrade", "websocket")
            .header("connection", "upgrade")
            .body(Body::empty())
            .unwrap();
        assert!(!deny_websocket(&req));
    }

    #[test]
    fn test_require_websocket() {
        let req = Request::builder()
            .uri("/")
            .header("upgrade", "websocket")
            .body(Body::empty())
            .unwrap();
        assert!(require_websocket(&req));

        let req2 = Request::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap();
        assert!(!require_websocket(&req2));
    }
}
