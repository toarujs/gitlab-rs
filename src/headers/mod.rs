#![allow(dead_code)]
use axum::http::{HeaderMap, HeaderName, HeaderValue};

pub const MAX_DETECT_SIZE: usize = 4096;

pub const CONTENT_DISPOSITION_HEADER: &str = "content-disposition";
pub const CONTENT_TYPE_HEADER: &str = "content-type";

pub const GITLAB_WORKHORSE_SEND_DATA_HEADER: &str = "gitlab-workhorse-send-data";
pub const X_SENDFILE_HEADER: &str = "x-sendfile";
pub const X_SENDFILE_TYPE_HEADER: &str = "x-sendfile-type";
pub const GITLAB_WORKHORSE_DETECT_CONTENT_TYPE_HEADER: &str =
    "gitlab-workhorse-detect-content-type";

pub const GITLAB_WORKHORSE_HEADERS: &str = "gitlab-workhorse";
pub const GITLAB_WORKHORSE_ERROR_HEADER: &str = "gitlab-workhorse-error";

pub const RESPONSE_HEADERS: [&str; 3] = [
    X_SENDFILE_HEADER,
    GITLAB_WORKHORSE_SEND_DATA_HEADER,
    GITLAB_WORKHORSE_DETECT_CONTENT_TYPE_HEADER,
];

pub fn is_detect_content_type_header_present(headers: &HeaderMap) -> bool {
    headers
        .get(GITLAB_WORKHORSE_DETECT_CONTENT_TYPE_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(false)
}

pub fn any_response_header_present(headers: &HeaderMap) -> bool {
    if !is_detect_content_type_header_present(headers) {
        return false;
    }

    for header in &RESPONSE_HEADERS {
        if headers.contains_key(*header) {
            return true;
        }
    }
    false
}

pub fn remove_response_headers(headers: &mut HeaderMap) {
    for header in &RESPONSE_HEADERS {
        headers.remove(*header);
    }
}

pub fn set_content_type_if_missing(headers: &mut HeaderMap, content_type: &str) {
    if !headers.contains_key(CONTENT_TYPE_HEADER) {
        if let Ok(value) = HeaderValue::from_str(content_type) {
            headers.insert(HeaderName::from_static(CONTENT_TYPE_HEADER), value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_static(GITLAB_WORKHORSE_DETECT_CONTENT_TYPE_HEADER),
            HeaderValue::from_static("true"),
        );
        h
    }

    #[test]
    fn test_is_detect_content_type_present() {
        assert!(is_detect_content_type_header_present(&make_headers()));

        let empty = HeaderMap::new();
        assert!(!is_detect_content_type_header_present(&empty));
    }

    #[test]
    fn test_any_response_header_present() {
        let headers = make_headers();
        assert!(any_response_header_present(&headers));

        let empty = HeaderMap::new();
        assert!(!any_response_header_present(&empty));

        let mut headers2 = make_headers();
        headers2.insert(
            HeaderName::from_static(X_SENDFILE_HEADER),
            HeaderValue::from_static("test"),
        );
        assert!(any_response_header_present(&headers2));
    }

    #[test]
    fn test_remove_response_headers() {
        let mut headers = make_headers();
        headers.insert(
            HeaderName::from_static(X_SENDFILE_HEADER),
            HeaderValue::from_static("test"),
        );
        headers.insert(
            HeaderName::from_static(GITLAB_WORKHORSE_SEND_DATA_HEADER),
            HeaderValue::from_static("test"),
        );
        assert_eq!(headers.len(), 3);

        remove_response_headers(&mut headers);
        assert_eq!(headers.len(), 0);
    }

    #[test]
    fn test_set_content_type_if_missing() {
        let mut headers = HeaderMap::new();
        set_content_type_if_missing(&mut headers, "application/json");
        assert_eq!(
            headers.get(CONTENT_TYPE_HEADER).unwrap().to_str().unwrap(),
            "application/json"
        );

        set_content_type_if_missing(&mut headers, "text/html");
        assert_eq!(
            headers.get(CONTENT_TYPE_HEADER).unwrap().to_str().unwrap(),
            "application/json"
        );
    }
}
