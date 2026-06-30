use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use bytes::Bytes;
use image::ImageEncoder;
use serde::Deserialize;
use std::io::Cursor;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct ImageResizerParams {
    pub location: Option<String>,
    pub format: Option<String>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    pub content_type: Option<String>,
}

pub async fn image_resizer_inject(
    json_data: String,
    _headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let params: ImageResizerParams = serde_json::from_str(&json_data).map_err(|e| {
        tracing::error!("Failed to parse image-resizer params: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let location = params.location.as_deref().unwrap_or("");
    let file_path = PathBuf::from(location);

    let canonical = match tokio::fs::canonicalize(&file_path).await {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!("Image-resizer: path not found: {}", location);
            return Err(StatusCode::NOT_FOUND);
        }
    };

    if !canonical.is_file() {
        tracing::warn!("Image-resizer: not a file: {}", location);
        return Err(StatusCode::NOT_FOUND);
    }

    let file_data = tokio::fs::read(&canonical).await.map_err(|e| {
        tracing::error!("Image-resizer: failed to read file {}: {}", location, e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Resize and convert
    let output = match resize_and_convert(&file_data, &params) {
        Ok(data) => data,
        Err(err) => {
            tracing::error!("Image-resizer: processing failed: {}", err);
            return Err(StatusCode::UNPROCESSABLE_ENTITY);
        }
    };

    let content_type = params.content_type.as_deref().unwrap_or("image/png");
    let output_len = output.len();

    let mut response = Response::new(Body::from(output));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert("content-type", content_type.parse().unwrap());
    headers.insert("content-length", output_len.to_string().parse().unwrap());
    headers.insert("cache-control", "private, max-age=0, must-revalidate".parse().unwrap());

    Ok(response)
}

fn resize_and_convert(data: &[u8], params: &ImageResizerParams) -> Result<Bytes, String> {
    let img = image::load_from_memory(data)
        .map_err(|e| format!("failed to decode image: {}", e))?;

    let has_size = params.width > 0 && params.height > 0;
    let processed = if has_size {
        img.resize_exact(
            params.width,
            params.height,
            image::imageops::FilterType::CatmullRom,
        )
    } else {
        img
    };

    let rgba = processed.to_rgba8();
    let (width, height) = rgba.dimensions();

    let format = params.format.as_deref().unwrap_or("");

    let output = match format {
        "webp" => {
            let mut buf = Vec::new();
            let encoder = image::codecs::webp::WebPEncoder::new_lossless(Cursor::new(&mut buf));
            encoder
                .write_image(rgba.as_raw(), width, height, image::ExtendedColorType::Rgba8)
                .map_err(|e| format!("webp encode: {}", e))?;
            Bytes::from(buf)
        }
        "avif" => {
            let mut buf = Vec::new();
            let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(
                Cursor::new(&mut buf),
                4,
                60,
            );
            encoder
                .write_image(rgba.as_raw(), width, height, image::ExtendedColorType::Rgba8)
                .map_err(|e| format!("avif encode: {}", e))?;
            Bytes::from(buf)
        }
        "jpeg" | "jpg" => {
            let mut buf = Vec::new();
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                Cursor::new(&mut buf),
                85,
            );
            encoder
                .write_image(rgba.as_raw(), width, height, image::ExtendedColorType::Rgba8)
                .map_err(|e| format!("jpeg encode: {}", e))?;
            Bytes::from(buf)
        }
        _ => {
            // PNG (original or unknown format)
            let mut buf = Vec::new();
            let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut buf));
            encoder
                .write_image(rgba.as_raw(), width, height, image::ExtendedColorType::Rgba8)
                .map_err(|e| format!("png encode: {}", e))?;
            Bytes::from(buf)
        }
    };

    Ok(output)
}
