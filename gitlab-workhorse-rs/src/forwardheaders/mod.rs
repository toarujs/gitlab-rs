#![allow(dead_code, unused_imports)]
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

#[derive(Debug, Clone)]
pub struct Params {
    pub enabled: bool,
    pub allow_list: Vec<String>,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_list: Vec::new(),
        }
    }
}

impl Params {
    pub fn forward_response_headers(
        &self,
        upstream_headers: &HeaderMap,
        preserve_header_keys: &[&str],
        extra_headers: &HeaderMap,
    ) -> HeaderMap {
        let mut result = HeaderMap::new();

        let canonical_protected: Vec<String> = preserve_header_keys
            .iter()
            .map(|k| k.to_lowercase())
            .collect();

        let canonical_allowed: Vec<String> = self
            .allow_list
            .iter()
            .map(|k| k.to_lowercase())
            .collect();

        for (key, value) in upstream_headers.iter() {
            let key_lower = key.as_str().to_lowercase();
            if canonical_protected.contains(&key_lower) {
                continue;
            }

            if self.enabled {
                if canonical_allowed.contains(&key_lower) {
                    result.insert(key.clone(), value.clone());
                }
            } else {
                result.insert(key.clone(), value.clone());
            }
        }

        for (key, value) in extra_headers.iter() {
            result.remove(key);
            result.insert(key.clone(), value.clone());
        }

        result
    }

    pub fn replace_content_type_if_multipart(headers: &mut HeaderMap) {
        if let Some(ct) = headers.get("content-type") {
            if let Ok(ct_str) = ct.to_str() {
                if ct_str.starts_with("multipart") {
                    headers.insert(
                        HeaderName::from_static("content-type"),
                        HeaderValue::from_static("application/octet-stream"),
                    );
                }
            }
        }
    }
}

/// Hop-by-hop headers that should not be forwarded
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// Protected response headers that should not be forwarded from upstream
const PROTECTED_RESPONSE_HEADERS: &[&str] = &[
    "content-length",
    "content-encoding",
    "transfer-encoding",
    "connection",
    "gitlab-workhorse-send-data",
    "gitlab-workhorse-detect-content-type",
    "x-sendfile",
];

/// Forward request headers, filtering out hop-by-hop headers
pub fn forward_request_headers(
    upstream_headers: &HeaderMap,
    _backend_url: &Url,
) -> HeaderMap {
    let mut result = HeaderMap::new();

    // Parse Connection header to get additional hop-by-hop headers
    let mut extra_hop_by_hop: Vec<String> = Vec::new();
    if let Some(conn) = upstream_headers.get("connection") {
        if let Ok(conn_str) = conn.to_str() {
            for name in conn_str.split(',') {
                extra_hop_by_hop.push(name.trim().to_lowercase());
            }
        }
    }

    for (key, value) in upstream_headers.iter() {
        let key_lower = key.as_str().to_lowercase();

        if HOP_BY_HOP_HEADERS.contains(&key_lower.as_str()) {
            continue;
        }

        if extra_hop_by_hop.contains(&key_lower) {
            continue;
        }

        if key_lower == "host" {
            continue;
        }

        result.insert(key.clone(), value.clone());
    }

    result
}

/// Forward response headers from upstream, filtering out protected headers
pub fn forward_response_headers(
    upstream_headers: &HeaderMap,
    _preserve_header_keys: Option<&[&str]>,
    _extra_headers: &[(&str, &str)],
) -> HeaderMap {
    let mut result = HeaderMap::new();

    for (key, value) in upstream_headers.iter() {
        let key_lower = key.as_str().to_lowercase();

        // Skip protected response headers
        if PROTECTED_RESPONSE_HEADERS.contains(&key_lower.as_str()) {
            continue;
        }

        result.append(key.clone(), value.clone());
    }

    // Add extra headers
    for (k, v) in _extra_headers {
        if let Ok(name) = HeaderName::from_bytes(k.as_bytes()) {
            if let Ok(val) = HeaderValue::from_str(v) {
                result.insert(name, val);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_upstream_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("content-type", "application/json".parse().unwrap());
        h.insert("x-custom", "value".parse().unwrap());
        h.insert("authorization", "Bearer token".parse().unwrap());
        h
    }

    #[test]
    fn test_forward_all_when_disabled() {
        let params = Params::default();
        let upstream = make_upstream_headers();
        let result = params.forward_response_headers(&upstream, &[], &HeaderMap::new());
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_forward_only_allowed_when_enabled() {
        let params = Params {
            enabled: true,
            allow_list: vec!["x-custom".to_string()],
        };
        let upstream = make_upstream_headers();
        let result = params.forward_response_headers(&upstream, &[], &HeaderMap::new());
        assert!(result.contains_key("x-custom"));
        assert!(!result.contains_key("authorization"));
    }

    #[test]
    fn test_preserved_headers_excluded() {
        let params = Params::default();
        let upstream = make_upstream_headers();
        let result = params.forward_response_headers(&upstream, &["content-type"], &HeaderMap::new());
        assert!(!result.contains_key("content-type"));
    }

    #[test]
    fn test_extra_headers_applied() {
        let params = Params::default();
        let upstream = make_upstream_headers();
        let mut extra = HeaderMap::new();
        extra.insert("x-extra", "extra-value".parse().unwrap());
        let result = params.forward_response_headers(&upstream, &[], &extra);
        assert_eq!(result.get("x-extra").unwrap().to_str().unwrap(), "extra-value");
    }

    #[test]
    fn test_replace_content_type_multipart() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "multipart/form-data; boundary=test".parse().unwrap());
        Params::replace_content_type_if_multipart(&mut headers);
        assert_eq!(
            headers.get("content-type").unwrap().to_str().unwrap(),
            "application/octet-stream"
        );
    }
}
