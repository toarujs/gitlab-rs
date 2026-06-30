#![allow(dead_code)]
use axum::body::Body;
use axum::response::Response;
use image::codecs::webp::WebPEncoder;
use image::ImageEncoder;
use http_body_util::BodyExt;
use moka::future::Cache;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
    Svg,
    Unknown,
}

impl ImageFormat {
    pub fn from_content_type(ct: &str) -> Self {
        if ct.contains("image/png") {
            ImageFormat::Png
        } else if ct.contains("image/jpeg") || ct.contains("image/jpg") {
            ImageFormat::Jpeg
        } else if ct.contains("image/gif") {
            ImageFormat::Gif
        } else if ct.contains("image/webp") {
            ImageFormat::WebP
        } else if ct.contains("image/svg") {
            ImageFormat::Svg
        } else {
            ImageFormat::Unknown
        }
    }

    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "png" => ImageFormat::Png,
            "jpg" | "jpeg" => ImageFormat::Jpeg,
            "gif" => ImageFormat::Gif,
            "webp" => ImageFormat::WebP,
            "svg" => ImageFormat::Svg,
            _ => ImageFormat::Unknown,
        }
    }

    pub fn is_convertible(&self) -> bool {
        matches!(self, ImageFormat::Png | ImageFormat::Jpeg)
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Svg => "image/svg+xml",
            ImageFormat::Unknown => "application/octet-stream",
        }
    }
}

pub struct WebPConverter {
    pub cache: Cache<Vec<u8>, Vec<u8>>,
    #[allow(dead_code)]
    pub quality: f32,
    pub min_size_bytes: usize,
}

impl WebPConverter {
    pub fn new(max_cache_bytes: u64, quality: f32) -> Self {
        let cache: Cache<Vec<u8>, Vec<u8>> = Cache::builder()
            .max_capacity(max_cache_bytes)
            .weigher(|_k: &Vec<u8>, v: &Vec<u8>| v.len() as u32)
            .time_to_idle(Duration::from_secs(3600))
            .build();

        Self {
            cache,
            quality: quality.clamp(1.0, 100.0),
            min_size_bytes: 1024,
        }
    }

    pub async fn convert_to_webp(&self, input: &[u8]) -> Result<Vec<u8>, String> {
        if input.is_empty() {
            return Err("empty input".to_string());
        }

        if input.len() < self.min_size_bytes {
            return Err("image too small for conversion".to_string());
        }

        if let Some(cached) = self.cache.get(input).await {
            return Ok(cached);
        }

        let img = image::load_from_memory(input)
            .map_err(|e| format!("failed to decode image: {}", e))?;

        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();

        let mut output = Vec::new();
        let encoder = WebPEncoder::new_lossless(Cursor::new(&mut output));

        encoder
            .write_image(
                rgba.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("failed to encode webp: {}", e))?;

        self.cache.insert(input.to_vec(), output.clone()).await;

        Ok(output)
    }

    pub async fn resize_and_convert(
        &self,
        input: &[u8],
        max_dim: u32,
    ) -> Result<Vec<u8>, String> {
        if input.is_empty() {
            return Err("empty input".to_string());
        }

        if input.len() < self.min_size_bytes {
            return Err("image too small".to_string());
        }

        let cache_key = {
            let mut key = input.to_vec();
            key.extend_from_slice(&max_dim.to_be_bytes());
            key
        };

        if let Some(cached) = self.cache.get(&cache_key).await {
            return Ok(cached);
        }

        let img = image::load_from_memory(input)
            .map_err(|e| format!("failed to decode image: {}", e))?;

        let (w, h) = (img.width(), img.height());
        let max_side = w.max(h);

        let rgba = if max_side > max_dim {
            let scale = max_dim as f64 / max_side as f64;
            let new_w = (w as f64 * scale) as u32;
            let new_h = (h as f64 * scale) as u32;
            let resized = img.resize_exact(
                new_w,
                new_h,
                image::imageops::FilterType::Lanczos3,
            );
            resized.to_rgba8()
        } else {
            img.to_rgba8()
        };

        let (width, height) = rgba.dimensions();

        let mut output = Vec::new();
        let encoder = WebPEncoder::new_lossless(Cursor::new(&mut output));
        encoder
            .write_image(
                rgba.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("failed to encode webp: {}", e))?;

        self.cache.insert(cache_key, output.clone()).await;

        Ok(output)
    }

    pub fn supports_webp(headers: &axum::http::HeaderMap) -> bool {
        headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .map(|a| a.contains("image/webp"))
            .unwrap_or(false)
    }

    pub fn supports_avif(headers: &axum::http::HeaderMap) -> bool {
        headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .map(|a| a.contains("image/avif"))
            .unwrap_or(false)
    }

    pub fn best_supported_format(headers: &axum::http::HeaderMap) -> ImageTargetFormat {
        let accept = headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if accept.contains("image/avif") {
            ImageTargetFormat::Avif
        } else if accept.contains("image/webp") {
            ImageTargetFormat::WebP
        } else {
            ImageTargetFormat::Original
        }
    }

    pub async fn convert_to_avif(&self, input: &[u8]) -> Result<Vec<u8>, String> {
        if input.is_empty() {
            return Err("empty input".to_string());
        }

        if input.len() < self.min_size_bytes {
            return Err("image too small".to_string());
        }

        if let Some(cached) = self.cache.get(input).await {
            return Ok(cached);
        }

        let img = image::load_from_memory(input)
            .map_err(|e| format!("failed to decode image: {}", e))?;

        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();

        let mut output = Vec::new();
        let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(
            Cursor::new(&mut output),
            4,
            60,
        );

        encoder
            .write_image(
                rgba.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("failed to encode avif: {}", e))?;

        self.cache.insert(input.to_vec(), output.clone()).await;

        Ok(output)
    }

    pub async fn resize_and_convert_avif(
        &self,
        input: &[u8],
        max_dim: u32,
    ) -> Result<Vec<u8>, String> {
        if input.is_empty() {
            return Err("empty input".to_string());
        }

        if input.len() < self.min_size_bytes {
            return Err("image too small".to_string());
        }

        let cache_key = {
            let mut key = b"avif:".to_vec();
            key.extend_from_slice(input);
            key.extend_from_slice(&max_dim.to_be_bytes());
            key
        };

        if let Some(cached) = self.cache.get(&cache_key).await {
            return Ok(cached);
        }

        let img = image::load_from_memory(input)
            .map_err(|e| format!("failed to decode image: {}", e))?;

        let (w, h) = (img.width(), img.height());
        let max_side = w.max(h);

        let rgba = if max_side > max_dim {
            let scale = max_dim as f64 / max_side as f64;
            let new_w = (w as f64 * scale) as u32;
            let new_h = (h as f64 * scale) as u32;
            let resized = img.resize_exact(
                new_w,
                new_h,
                image::imageops::FilterType::Lanczos3,
            );
            resized.to_rgba8()
        } else {
            img.to_rgba8()
        };

        let (width, height) = rgba.dimensions();

        let mut output = Vec::new();
        let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(
            Cursor::new(&mut output),
            4,
            60,
        );
        encoder
            .write_image(
                rgba.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("failed to encode avif: {}", e))?;

        self.cache.insert(cache_key, output.clone()).await;

        Ok(output)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageTargetFormat {
    Avif,
    WebP,
    Original,
}

pub async fn convert_response_to_webp(
    response: Response,
    converter: &Arc<WebPConverter>,
) -> Response {
    let (parts, body) = response.into_parts();

    let body_bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return Response::builder()
                .status(502)
                .body(Body::from("Bad Gateway"))
                .unwrap();
        }
    };

    match converter.convert_to_webp(&body_bytes).await {
        Ok(webp_bytes) if webp_bytes.len() < body_bytes.len() => {
            let mut new_parts = parts;
            new_parts.headers.insert(
                "content-type",
                "image/webp".parse().unwrap(),
            );
            new_parts.headers.insert(
                "content-length",
                webp_bytes.len().to_string().parse().unwrap(),
            );
            new_parts.headers.remove("content-encoding");
            new_parts.headers.remove("transfer-encoding");
            new_parts.headers.insert(
                "vary",
                "Accept".parse().unwrap(),
            );
            Response::from_parts(new_parts, Body::from(webp_bytes))
        }
        _ => {
            tracing::debug!("WebP conversion skipped or no size benefit");
            let mut new_parts = parts;
            new_parts.headers.insert(
                "vary",
                "Accept".parse().unwrap(),
            );
            Response::from_parts(new_parts, Body::from(body_bytes))
        }
    }
}

pub async fn convert_response_to_best_format(
    response: Response,
    converter: &Arc<WebPConverter>,
    format: ImageTargetFormat,
) -> Response {
    match format {
        ImageTargetFormat::Avif => convert_response_to_avif(response, converter).await,
        ImageTargetFormat::WebP => convert_response_to_webp(response, converter).await,
        ImageTargetFormat::Original => response,
    }
}

pub async fn convert_response_to_avif(
    response: Response,
    converter: &Arc<WebPConverter>,
) -> Response {
    let (parts, body) = response.into_parts();

    let body_bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return Response::builder()
                .status(502)
                .body(Body::from("Bad Gateway"))
                .unwrap();
        }
    };

    match converter.convert_to_avif(&body_bytes).await {
        Ok(avif_bytes) if avif_bytes.len() < body_bytes.len() => {
            let mut new_parts = parts;
            new_parts
                .headers
                .insert("content-type", "image/avif".parse().unwrap());
            new_parts.headers.insert(
                "content-length",
                avif_bytes.len().to_string().parse().unwrap(),
            );
            new_parts.headers.remove("content-encoding");
            new_parts.headers.remove("transfer-encoding");
            new_parts
                .headers
                .insert("vary", "Accept".parse().unwrap());
            Response::from_parts(new_parts, Body::from(avif_bytes))
        }
        _ => {
            tracing::debug!("AVIF conversion skipped or no size benefit, falling back to WebP");
            convert_response_to_webp(
                Response::from_parts(parts, Body::from(body_bytes)),
                converter,
            )
            .await
        }
    }
}

pub async fn resize_and_convert_avif_response(
    response: Response,
    converter: &Arc<WebPConverter>,
    max_dim: u32,
) -> Response {
    let (parts, body) = response.into_parts();

    let body_bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return Response::builder()
                .status(502)
                .body(Body::from("Bad Gateway"))
                .unwrap();
        }
    };

    match converter.resize_and_convert_avif(&body_bytes, max_dim).await {
        Ok(avif_bytes) if avif_bytes.len() < body_bytes.len() => {
            let mut new_parts = parts;
            new_parts
                .headers
                .insert("content-type", "image/avif".parse().unwrap());
            new_parts.headers.insert(
                "content-length",
                avif_bytes.len().to_string().parse().unwrap(),
            );
            new_parts.headers.remove("content-encoding");
            new_parts.headers.remove("transfer-encoding");
            new_parts
                .headers
                .insert("vary", "Accept".parse().unwrap());
            Response::from_parts(new_parts, Body::from(avif_bytes))
        }
        _ => {
            tracing::debug!("Resize+AVIF skipped, falling back to WebP");
            resize_and_convert_response(
                Response::from_parts(parts, Body::from(body_bytes)),
                converter,
                max_dim,
            )
            .await
        }
    }
}

pub async fn resize_and_convert_best_response(
    response: Response,
    converter: &Arc<WebPConverter>,
    max_dim: u32,
    format: ImageTargetFormat,
) -> Response {
    match format {
        ImageTargetFormat::Avif => {
            resize_and_convert_avif_response(response, converter, max_dim).await
        }
        ImageTargetFormat::WebP => {
            resize_and_convert_response(response, converter, max_dim).await
        }
        ImageTargetFormat::Original => response,
    }
}

pub async fn resize_and_convert_response(
    response: Response,
    converter: &Arc<WebPConverter>,
    max_dim: u32,
) -> Response {
    let (parts, body) = response.into_parts();

    let body_bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return Response::builder()
                .status(502)
                .body(Body::from("Bad Gateway"))
                .unwrap();
        }
    };

    match converter.resize_and_convert(&body_bytes, max_dim).await {
        Ok(webp_bytes) if webp_bytes.len() < body_bytes.len() => {
            let mut new_parts = parts;
            new_parts.headers.insert("content-type", "image/webp".parse().unwrap());
            new_parts.headers.insert("content-length", webp_bytes.len().to_string().parse().unwrap());
            new_parts.headers.remove("content-encoding");
            new_parts.headers.remove("transfer-encoding");
            new_parts.headers.insert("vary", "Accept".parse().unwrap());
            Response::from_parts(new_parts, Body::from(webp_bytes))
        }
        _ => {
            tracing::debug!("Resize+WebP conversion skipped or no size benefit");
            let mut new_parts = parts;
            new_parts.headers.insert("vary", "Accept".parse().unwrap());
            Response::from_parts(new_parts, Body::from(body_bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_from_content_type() {
        assert_eq!(
            ImageFormat::from_content_type("image/png"),
            ImageFormat::Png
        );
        assert_eq!(
            ImageFormat::from_content_type("image/jpeg"),
            ImageFormat::Jpeg
        );
        assert_eq!(
            ImageFormat::from_content_type("image/webp"),
            ImageFormat::WebP
        );
        assert_eq!(
            ImageFormat::from_content_type("text/html"),
            ImageFormat::Unknown
        );
    }

    #[test]
    fn test_format_from_extension() {
        assert_eq!(ImageFormat::from_extension("png"), ImageFormat::Png);
        assert_eq!(ImageFormat::from_extension("jpg"), ImageFormat::Jpeg);
        assert_eq!(ImageFormat::from_extension("jpeg"), ImageFormat::Jpeg);
        assert_eq!(ImageFormat::from_extension("webp"), ImageFormat::WebP);
        assert_eq!(ImageFormat::from_extension("foo"), ImageFormat::Unknown);
    }

    #[test]
    fn test_is_convertible() {
        assert!(ImageFormat::Png.is_convertible());
        assert!(ImageFormat::Jpeg.is_convertible());
        assert!(!ImageFormat::WebP.is_convertible());
        assert!(!ImageFormat::Gif.is_convertible());
        assert!(!ImageFormat::Svg.is_convertible());
    }

    #[tokio::test]
    async fn test_convert_small_png_to_webp() {
        let converter = WebPConverter::new(100 * 1024 * 1024, 80.0);

        let png_bytes = create_test_png();
        let webp = converter.convert_to_webp(&png_bytes).await;

        assert!(webp.is_ok());
        let webp_bytes = webp.unwrap();
        assert!(!webp_bytes.is_empty());
        assert!(webp_bytes.len() < png_bytes.len() * 2);
    }

    #[tokio::test]
    async fn test_cache_hit() {
        let converter = WebPConverter::new(100 * 1024 * 1024, 80.0);
        let png_bytes = create_test_png();

        let first = converter.convert_to_webp(&png_bytes).await.unwrap();
        let second = converter.convert_to_webp(&png_bytes).await.unwrap();

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn test_empty_input() {
        let converter = WebPConverter::new(100 * 1024 * 1024, 80.0);
        let result = converter.convert_to_webp(&[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resize_large_png() {
        let converter = WebPConverter::new(100 * 1024 * 1024, 80.0);
        let png_bytes = create_large_test_png();
        assert!(png_bytes.len() > 1024);

        let result = converter.resize_and_convert(&png_bytes, 128).await;
        assert!(result.is_ok());
        let webp_bytes = result.unwrap();
        assert!(webp_bytes.len() < png_bytes.len());
    }

    #[tokio::test]
    async fn test_resize_small_noop() {
        let converter = WebPConverter::new(100 * 1024 * 1024, 80.0);
        let png_bytes = create_test_png();

        let result = converter.resize_and_convert(&png_bytes, 1024).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_convert_png_to_avif() {
        let converter = WebPConverter::new(100 * 1024 * 1024, 80.0);
        let png_bytes = create_test_png();

        let result = converter.convert_to_avif(&png_bytes).await;
        assert!(result.is_ok());
        let avif_bytes = result.unwrap();
        assert!(!avif_bytes.is_empty());
    }

    #[tokio::test]
    async fn test_avif_cache_hit() {
        let converter = WebPConverter::new(100 * 1024 * 1024, 80.0);
        let png_bytes = create_test_png();

        let first = converter.convert_to_avif(&png_bytes).await.unwrap();
        let second = converter.convert_to_avif(&png_bytes).await.unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn test_avif_empty_input() {
        let converter = WebPConverter::new(100 * 1024 * 1024, 80.0);
        let result = converter.convert_to_avif(&[]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resize_and_convert_avif() {
        let converter = WebPConverter::new(100 * 1024 * 1024, 80.0);
        let png_bytes = create_large_test_png();

        let result = converter.resize_and_convert_avif(&png_bytes, 128).await;
        assert!(result.is_ok());
        let avif_bytes = result.unwrap();
        assert!(!avif_bytes.is_empty());
    }

    #[test]
    fn test_best_supported_format_avif() {
        use axum::http::HeaderMap;
        let mut headers = HeaderMap::new();
        headers.insert("accept", "image/avif,image/webp,*/*".parse().unwrap());
        assert_eq!(WebPConverter::best_supported_format(&headers), ImageTargetFormat::Avif);
    }

    #[test]
    fn test_best_supported_format_webp() {
        use axum::http::HeaderMap;
        let mut headers = HeaderMap::new();
        headers.insert("accept", "image/webp,*/*".parse().unwrap());
        assert_eq!(WebPConverter::best_supported_format(&headers), ImageTargetFormat::WebP);
    }

    #[test]
    fn test_best_supported_format_original() {
        use axum::http::HeaderMap;
        let mut headers = HeaderMap::new();
        headers.insert("accept", "image/png,*/*".parse().unwrap());
        assert_eq!(WebPConverter::best_supported_format(&headers), ImageTargetFormat::Original);
    }

    fn create_large_test_png() -> Vec<u8> {
        let mut buf = Vec::new();
        let img = image::RgbaImage::from_pixel(1024, 768, image::Rgba([100, 150, 200, 255]));
        let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut buf));
        encoder
            .write_image(
                img.as_raw(),
                1024,
                768,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        buf
    }

    fn create_test_png() -> Vec<u8> {
        let mut buf = Vec::new();
        let img = image::RgbaImage::from_pixel(256, 256, image::Rgba([255, 0, 0, 255]));
        let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut buf));
        encoder
            .write_image(
                img.as_raw(),
                256,
                256,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        buf
    }
}
