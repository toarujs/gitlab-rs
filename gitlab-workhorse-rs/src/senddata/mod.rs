#![allow(dead_code)]
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL_SAFE, Engine};
use std::sync::Arc;
use tokio::sync::RwLock;

pub mod sendfile;
pub mod sendurl;
pub mod git_injectors;
pub mod imageresizer_injecter;

/// Header name matching Go's title-case format: `Gitlab-Workhorse-Send-Data`
pub const SEND_DATA_HEADER: &str = "gitlab-workhorse-send-data";

pub type InjectFn = Arc<
    dyn Fn(String, HeaderMap) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, StatusCode>> + Send>>
        + Send
        + Sync,
>;

pub struct Injecter {
    pub name: String,
    pub prefix: String,
    pub inject: InjectFn,
}

pub struct InjecterRegistry {
    injecters: RwLock<Vec<Injecter>>,
}

impl InjecterRegistry {
    pub fn new() -> Self {
        Self {
            injecters: RwLock::new(Vec::new()),
        }
    }

    pub async fn register(&self, injecter: Injecter) {
        let mut injecters = self.injecters.write().await;
        injecters.push(injecter);
    }

    pub async fn find(&self, data_header: &str) -> Option<Injecter> {
        let injecters = self.injecters.read().await;
        for injecter in injecters.iter() {
            if data_header == injecter.prefix || data_header.starts_with(&injecter.prefix) {
                return Some(Injecter {
                    name: injecter.name.clone(),
                    prefix: injecter.prefix.clone(),
                    inject: injecter.inject.clone(),
                });
            }
        }
        None
    }
}

/// Encode send-data in Go-compatible format: PREFIX + base64_url_safe_no_pad(JSON)
pub fn encode_send_data(prefix: &str, json: &str) -> String {
    let encoded = BASE64_URL_SAFE.encode(json.as_bytes());
    format!("{}{}", prefix, encoded)
}

/// Decode send-data header value
/// Go format: PREFIX + base64_url_safe_no_pad(JSON)
/// Prefixes match Go's format (without "send-data:" prefix)
pub fn decode_send_data(header_value: &str) -> Option<(String, String)> {
    // Go uses these exact prefixes (no "send-data:" prefix)
    let prefixes = [
        "send-file:",
        "send-url:",
        "git-archive:",
        "git-blob:",
        "git-diff:",
        "git-snapshot:",
        "git-format-patch:",
        "git-changed-paths:",
        "git-list-blobs:",
        "artifacts-entry:",
        "image-resizer:",
        "dependency-proxy:",
        "orbit-query:",
    ];

    for prefix in &prefixes {
        if let Some(encoded) = header_value.strip_prefix(prefix) {
            // Go uses URL-safe Base64 without padding
            if let Ok(decoded) = BASE64_URL_SAFE.decode(encoded) {
                if let Ok(json_str) = String::from_utf8(decoded) {
                    return Some((prefix.to_string(), json_str));
                }
            }
            // Fallback: try standard Base64 (for compatibility)
            if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) {
                if let Ok(json_str) = String::from_utf8(decoded) {
                    return Some((prefix.to_string(), json_str));
                }
            }
        }
    }
    None
}

pub async fn intercept_send_data(
    status: StatusCode,
    headers: &HeaderMap,
    _body: &str,
    registry: &InjecterRegistry,
) -> Option<Response> {
    if !status.is_success() {
        return None;
    }

    // Try both lowercase and title-case header names for compatibility
    let send_data_value = headers
        .get(SEND_DATA_HEADER)
        .or_else(|| headers.get("gitlab-workhorse-send-data"));
    let header_str = send_data_value?.to_str().ok()?;

    let (prefix, json_data) = decode_send_data(header_str)?;

    // Find injecter with matching prefix (registry uses "send-data:PREFIX" format)
    let injecter = registry.find(&format!("send-data:{}", prefix)).await
        .or_else(|| {
            // Fallback: try finding with just the prefix
            let rt = tokio::runtime::Handle::current();
            rt.block_on(registry.find(&prefix))
        })?;

    let fake_headers = HeaderMap::new();
    match (injecter.inject)(json_data, fake_headers).await {
        Ok(response) => Some(response),
        Err(_) => {
            let mut error_headers = HeaderMap::new();
            error_headers.insert("content-type", "text/plain".parse().unwrap());
            Some((StatusCode::INTERNAL_SERVER_ERROR, error_headers, "senddata injection failed").into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_send_data_go_compatible() {
        // Test with Go-style prefix (no "send-data:" prefix)
        let encoded = encode_send_data("send-file:", "{\"path\":\"/tmp/test\"}");
        assert!(encoded.starts_with("send-file:"));

        let decoded = decode_send_data(&encoded);
        assert!(decoded.is_some());
        let (prefix, json) = decoded.unwrap();
        assert_eq!(prefix, "send-file:");
        assert!(json.contains("test"));
    }

    #[test]
    fn test_decode_send_data_go_format() {
        // Simulate what Go would send
        let json = r#"{"URL":"https://example.com/file","AllowRedirects":true}"#;
        let encoded = BASE64_URL_SAFE.encode(json.as_bytes());
        let header_value = format!("send-url:{}", encoded);

        let decoded = decode_send_data(&header_value);
        assert!(decoded.is_some());
        let (prefix, json_str) = decoded.unwrap();
        assert_eq!(prefix, "send-url:");
        assert!(json_str.contains("example.com"));
    }

    #[test]
    fn test_decode_send_data_invalid() {
        assert!(decode_send_data("invalid-header").is_none());
        assert!(decode_send_data("unknown:data").is_none());
    }

    #[tokio::test]
    async fn test_injecter_registry() {
        let registry = InjecterRegistry::new();

        let injecter = Injecter {
            name: "test".to_string(),
            prefix: "send-data:test:".to_string(),
            inject: Arc::new(|_json, _headers| {
                Box::pin(async {
                    let mut h = HeaderMap::new();
                    h.insert("content-type", "text/plain".parse().unwrap());
                    Ok((StatusCode::OK, h, "test ok").into_response())
                })
            }),
        };

        registry.register(injecter).await;

        let found = registry.find("send-data:test:").await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "test");
    }

    #[test]
    fn test_decode_all_go_prefixes() {
        // Test all Go send-data prefixes
        let test_cases = vec![
            ("send-file:", r#"{"path":"/tmp/test"}"#),
            ("send-url:", r#"{"URL":"https://example.com"}"#),
            ("git-archive:", r#"{"ArchivePath":"/tmp/archive"}"#),
            ("git-blob:", r#"{"RepoPath":"/tmp/repo"}"#),
            ("git-diff:", r#"{"RepoPath":"/tmp/repo"}"#),
            ("git-snapshot:", r#"{"RepoPath":"/tmp/repo"}"#),
            ("artifacts-entry:", r#"{"Archive":"/tmp/artifacts.zip","Entry":"file.txt"}"#),
        ];

        for (prefix, json) in test_cases {
            let encoded = encode_send_data(prefix, json);
            let decoded = decode_send_data(&encoded);
            assert!(decoded.is_some(), "Failed to decode prefix: {}", prefix);
            let (decoded_prefix, decoded_json) = decoded.unwrap();
            assert_eq!(decoded_prefix, prefix);
            assert_eq!(decoded_json, json);
        }
    }
}
