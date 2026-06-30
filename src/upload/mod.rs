#![allow(dead_code)]

use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::state::AppState;

#[derive(Debug, Clone)]
pub struct UploadState {
    pub upload_dir: PathBuf,
    pub max_file_size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadResponse {
    pub id: String,
    pub path: String,
    pub size: u64,
    pub content_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadStatus {
    pub id: String,
    pub status: UploadStatusEnum,
    pub progress: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum UploadStatusEnum {
    Pending,
    Uploading,
    Completed,
    Failed,
}

pub async fn handle_upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, StatusCode> {
    let upload_id = Uuid::new_v4().to_string();
    let upload_path = state.upload.upload_dir.join(&upload_id);

    if let Some(parent) = upload_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            tracing::error!("Failed to create upload directory: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    let mut buf = state.memory_pool.acquire().await;
    let file = tokio::fs::File::create(&upload_path).await.map_err(|e| {
        tracing::error!("Failed to create file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let mut file_writer = tokio::io::BufWriter::new(file);

    let mut total_size = 0u64;
    let mut content_type = String::new();

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        tracing::error!("Failed to read multipart field: {}", e);
        StatusCode::BAD_REQUEST
    })? {
        content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let data = field.bytes().await.map_err(|e| {
            tracing::error!("Failed to read field data: {}", e);
            StatusCode::BAD_REQUEST
        })?;

        if total_size + data.len() as u64 > state.upload.max_file_size {
            state.memory_pool.release(buf).await;
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }

        buf.extend_from_slice(&data);
        total_size += data.len() as u64;
    }

    file_writer.write_all(&buf).await.map_err(|e| {
        tracing::error!("Failed to write file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    file_writer.flush().await.map_err(|e| {
        tracing::error!("Failed to flush file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    state.metrics.record_upload(total_size);
    state.memory_pool.release(buf).await;

    Ok(Json(UploadResponse {
        id: upload_id.clone(),
        path: format!("/uploads/{}", upload_id),
        size: total_size,
        content_type,
    }))
}

pub async fn handle_upload_status(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
) -> Result<Json<UploadStatus>, StatusCode> {
    let upload_path = state.upload.upload_dir.join(&upload_id);

    if !upload_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(UploadStatus {
        id: upload_id,
        status: UploadStatusEnum::Completed,
        progress: 1.0,
    }))
}

pub async fn handle_upload_progress(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
) -> Result<Json<UploadStatus>, StatusCode> {
    let upload_path = state.upload.upload_dir.join(&upload_id);

    if !upload_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(UploadStatus {
        id: upload_id,
        status: UploadStatusEnum::Completed,
        progress: 1.0,
    }))
}
