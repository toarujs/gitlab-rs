#![allow(dead_code, unused_imports)]
use crate::senddata::sendfile;

pub const ARTIFACTS_ENTRY_PREFIX: &str = "artifacts-entry:";

pub fn detect_content_type(file_name: &str) -> &'static str {
    let ext = file_name.rsplit('.').next().unwrap_or("");
    match ext {
        "json" => "application/json",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "gzip" => "application/gzip",
        "tar" => "application/x-tar",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

pub fn escape_quotes(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn content_disposition_attachment(file_name: &str) -> String {
    let basename = file_name.rsplit('/').next().unwrap_or(file_name);
    format!(
        "attachment; filename=\"{}\"",
        escape_quotes(basename)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_content_type() {
        assert_eq!(detect_content_type("data.json"), "application/json");
        assert_eq!(detect_content_type("page.html"), "text/html");
        assert_eq!(detect_content_type("image.png"), "image/png");
        assert_eq!(detect_content_type("unknown.xyz"), "application/octet-stream");
    }

    #[test]
    fn test_escape_quotes() {
        assert_eq!(escape_quotes("hello"), "hello");
        assert_eq!(escape_quotes("he\"llo"), "he\\\"llo");
        assert_eq!(escape_quotes("he\\llo"), "he\\\\llo");
    }

    #[test]
    fn test_content_disposition() {
        let cd = content_disposition_attachment("/path/to/file.txt");
        assert_eq!(cd, "attachment; filename=\"file.txt\"");
    }
}
