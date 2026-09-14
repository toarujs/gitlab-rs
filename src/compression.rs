use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncReadExt;
use axum::extract::Request;
use axum::http::Extensions;
use axum::middleware::Next;
use http_body::Body;
use tower_http::compression::predicate::{DefaultPredicate, Predicate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressedEncoding {
    Brotli,
    Gzip,
}

impl CompressedEncoding {
    pub fn extension(&self) -> &'static str {
        match self {
            CompressedEncoding::Brotli => ".br",
            CompressedEncoding::Gzip => ".gz",
        }
    }

    pub fn content_encoding(&self) -> &'static str {
        match self {
            CompressedEncoding::Brotli => "br",
            CompressedEncoding::Gzip => "gzip",
        }
    }
}

pub fn negotiate_encoding(headers: &HeaderMap) -> Option<CompressedEncoding> {
    let accept = headers
        .get("accept-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if accept.contains("br") {
        Some(CompressedEncoding::Brotli)
    } else if accept.contains("gzip") {
        Some(CompressedEncoding::Gzip)
    } else {
        None
    }
}

/// Marker inserted on git pack responses so CompressionLayer skips zstd/br.
#[derive(Clone, Copy, Debug)]
pub struct SkipGitPackCompression;

#[derive(Clone)]
pub struct GitPackSkipPredicate {
    inner: DefaultPredicate,
}

impl GitPackSkipPredicate {
    pub fn new() -> Self {
        Self {
            inner: DefaultPredicate::new(),
        }
    }
}

impl Predicate for GitPackSkipPredicate {
    fn should_compress<B>(&self, response: &axum::http::Response<B>) -> bool
    where
        B: Body,
    {
        if !allow_http_compression(response.headers(), response.extensions()) {
            return false;
        }
        self.inner.should_compress(response)
    }
}

pub fn is_git_pack_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path.ends_with("/git-upload-pack") || path.ends_with("/git-receive-pack")
}

pub fn skip_git_http_compression_content_type(content_type: &str) -> bool {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    ct.starts_with("application/x-git-")
}

pub fn allow_http_compression(headers: &HeaderMap, extensions: &Extensions) -> bool {
    if extensions.get::<SkipGitPackCompression>().is_some() {
        return false;
    }
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    !skip_git_http_compression_content_type(ct)
}

pub async fn mark_git_pack_skip_compress(req: Request, next: Next) -> Response {
    let skip = is_git_pack_path(req.uri().path());
    let mut resp = next.run(req).await;
    if skip {
        resp.extensions_mut().insert(SkipGitPackCompression);
    }
    resp
}

pub async fn find_compressed_variant(
    original_path: &Path,
    encoding: CompressedEncoding,
) -> Option<CompressedVariant> {
    let ext = encoding.extension();
    let compressed_path = PathBuf::from(format!("{}{}", original_path.to_string_lossy(), ext));

    if compressed_path.exists() {
        let metadata = fs::metadata(&compressed_path).await.ok()?;
        if metadata.is_file() {
            return Some(CompressedVariant {
                path: compressed_path,
                size: metadata.len(),
                encoding,
            });
        }
    }

    None
}

pub struct CompressedVariant {
    pub path: PathBuf,
    pub size: u64,
    pub encoding: CompressedEncoding,
}

pub struct CompressionResources {
    pub compressions: Vec<CompressionReference>,
    pub original_path: Option<PathBuf>,
    pub original_size: u64,
    pub content_type: Option<String>,
    pub cache_control: Option<String>,
    pub error: Option<Response>,
}

impl CompressionResources {
    pub fn empty() -> Self {
        Self {
            compressions: Vec::new(),
            original_path: None,
            original_size: 0,
            content_type: None,
            cache_control: None,
            error: None,
        }
    }
}

pub struct CompressionReference {
    pub path: PathBuf,
    pub encoding: CompressedEncoding,
    pub size: u64,
}

pub async fn find_compression_resources(
    doc_root: &Path,
    sub_path: &str,
    client_encoding: Option<CompressedEncoding>,
) -> CompressionResources {
    let file_path = doc_root.join("assets").join(sub_path);

    if !file_path.exists() {
        let mut res = CompressionResources::empty();
        res.error = Some(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::empty())
            .unwrap());
        return res;
    }

    if !file_path.is_file() {
        let mut res = CompressionResources::empty();
        res.error = Some(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::empty())
            .unwrap());
        return res;
    }

    let sanitized = match file_path.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            let mut res = CompressionResources::empty();
            res.error = Some(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap());
            return res;
        }
    };

    if !sanitized.starts_with(doc_root) {
        let mut res = CompressionResources::empty();
        res.error = Some(Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(axum::body::Body::empty())
            .unwrap());
        return res;
    }

    let metadata = match fs::metadata(&file_path).await {
        Ok(m) => m,
        Err(_) => {
            let mut res = CompressionResources::empty();
            res.error = Some(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap());
            return res;
        }
    };

    let content_type = mime_guess::from_path(&file_path)
        .first_or_octet_stream()
        .to_string();

    let cache_duration = if file_path.extension().map_or(false, |e| {
        matches!(
            e.to_str(),
            Some("js" | "css" | "png" | "jpg" | "jpeg" | "gif" | "svg" | "woff" | "woff2" | "ttf" | "eot" | "ico")
        )
    }) {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    };

    let mut compressions = Vec::new();

    if let Some(enc) = client_encoding {
        if let Some(variant) = find_compressed_variant(&file_path, enc).await {
            compressions.push(CompressionReference {
                path: variant.path,
                encoding: variant.encoding,
                size: variant.size,
            });
        }
    } else {
        if let Some(variant) = find_compressed_variant(&file_path, CompressedEncoding::Brotli).await {
            compressions.push(CompressionReference {
                path: variant.path,
                encoding: variant.encoding,
                size: variant.size,
            });
        }
        if let Some(variant) = find_compressed_variant(&file_path, CompressedEncoding::Gzip).await {
            compressions.push(CompressionReference {
                path: variant.path,
                encoding: variant.encoding,
                size: variant.size,
            });
        }
    }

    CompressionResources {
        compressions,
        original_path: Some(file_path),
        original_size: metadata.len(),
        content_type: Some(content_type),
        cache_control: Some(cache_duration.to_string()),
        error: None,
    }
}

pub async fn read_file_to_bytes(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    let mut file = fs::File::open(path).await?;
    let mut content = Vec::new();
    file.read_to_end(&mut content).await?;
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_negotiate_brotli() {
        let mut headers = HeaderMap::new();
        headers.insert("accept-encoding", "br, gzip".parse().unwrap());
        assert_eq!(negotiate_encoding(&headers), Some(CompressedEncoding::Brotli));
    }

    #[test]
    fn test_negotiate_gzip() {
        let mut headers = HeaderMap::new();
        headers.insert("accept-encoding", "gzip, deflate".parse().unwrap());
        assert_eq!(negotiate_encoding(&headers), Some(CompressedEncoding::Gzip));
    }

    #[test]
    fn test_negotiate_none() {
        let mut headers = HeaderMap::new();
        headers.insert("accept-encoding", "identity".parse().unwrap());
        assert_eq!(negotiate_encoding(&headers), None);
    }

    #[test]
    fn test_negotiate_empty() {
        let headers = HeaderMap::new();
        assert_eq!(negotiate_encoding(&headers), None);
    }

    #[test]
    fn skips_git_pack_content_types() {
        assert!(skip_git_http_compression_content_type(
            "application/x-git-upload-pack-result"
        ));
        assert!(skip_git_http_compression_content_type(
            "application/x-git-receive-pack-result"
        ));
        assert!(skip_git_http_compression_content_type(
            "application/x-git-upload-pack-advertisement"
        ));
        assert!(skip_git_http_compression_content_type(
            "application/x-git-packed-objects; charset=utf-8"
        ));
    }

    #[test]
    fn keeps_json_and_html_compressible() {
        assert!(!skip_git_http_compression_content_type("application/json"));
        assert!(!skip_git_http_compression_content_type(
            "text/html; charset=utf-8"
        ));
    }

    #[test]
    fn pack_path_matches_upload_and_receive() {
        assert!(is_git_pack_path("/group/repo.git/git-upload-pack"));
        assert!(is_git_pack_path("/group/repo.git/git-receive-pack?service=git-receive-pack"));
        assert!(!is_git_pack_path("/group/repo.git/info/refs"));
        assert!(!is_git_pack_path("/api/v4/projects"));
    }

    #[test]
    fn allow_http_compression_respects_extension_marker() {
        let headers = HeaderMap::new();
        let mut ext = Extensions::new();
        assert!(allow_http_compression(&headers, &ext));
        ext.insert(SkipGitPackCompression);
        assert!(!allow_http_compression(&headers, &ext));
    }
}
