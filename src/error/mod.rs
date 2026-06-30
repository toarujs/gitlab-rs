#![allow(dead_code)]
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<ErrorDetails>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner_error: Option<String>,
}

impl ErrorResponse {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            error: status
                .canonical_reason()
                .unwrap_or("Unknown Error")
                .to_string(),
            message: message.into(),
            status: status.as_u16(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: ErrorDetails) -> Self {
        self.details = Some(details);
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        let details = self.details.get_or_insert(ErrorDetails {
            code: None,
            target: None,
            inner_error: None,
        });
        details.code = Some(code.into());
        self
    }

    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        let details = self.details.get_or_insert(ErrorDetails {
            code: None,
            target: None,
            inner_error: None,
        });
        details.target = Some(target.into());
        self
    }
}

impl fmt::Display for ErrorResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.error, self.message)
    }
}

impl IntoResponse for ErrorResponse {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self)).into_response()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Authentication failed: {0}")]
    Authentication(String),

    #[error("Authorization failed: {0}")]
    Authorization(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Internal server error: {0}")]
    Internal(String),

    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Payload too large: {0}")]
    PayloadTooLarge(String),

    #[error("Unprocessable entity: {0}")]
    UnprocessableEntity(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Gateway error: {0}")]
    Gateway(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl AppError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            AppError::Authentication(_) => StatusCode::UNAUTHORIZED,
            AppError::Authorization(_) => StatusCode::FORBIDDEN,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Timeout(_) => StatusCode::REQUEST_TIMEOUT,
            AppError::RateLimitExceeded => StatusCode::TOO_MANY_REQUESTS,
            AppError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            AppError::UnprocessableEntity(_) => StatusCode::UNPROCESSABLE_ENTITY,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::Gateway(_) => StatusCode::BAD_GATEWAY,
            AppError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Serialization(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn error_code(&self) -> &'static str {
        match self {
            AppError::Authentication(_) => "AUTHENTICATION_FAILED",
            AppError::Authorization(_) => "AUTHORIZATION_FAILED",
            AppError::NotFound(_) => "NOT_FOUND",
            AppError::BadRequest(_) => "BAD_REQUEST",
            AppError::Internal(_) => "INTERNAL_ERROR",
            AppError::ServiceUnavailable(_) => "SERVICE_UNAVAILABLE",
            AppError::Timeout(_) => "TIMEOUT",
            AppError::RateLimitExceeded => "RATE_LIMIT_EXCEEDED",
            AppError::PayloadTooLarge(_) => "PAYLOAD_TOO_LARGE",
            AppError::UnprocessableEntity(_) => "UNPROCESSABLE_ENTITY",
            AppError::Conflict(_) => "CONFLICT",
            AppError::Gateway(_) => "GATEWAY_ERROR",
            AppError::Io(_) => "IO_ERROR",
            AppError::Serialization(_) => "SERIALIZATION_ERROR",
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let error_response =
            ErrorResponse::new(status, self.to_string()).with_code(self.error_code());
        error_response.into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

pub fn handle_error<E: std::error::Error>(err: E) -> AppError {
    AppError::Internal(err.to_string())
}

pub fn handle_anyhow_error(err: anyhow::Error) -> AppError {
    AppError::Internal(err.to_string())
}

impl From<reqwest::Error> for AppError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            AppError::Timeout(err.to_string())
        } else if err.is_connect() {
            AppError::Gateway(err.to_string())
        } else {
            AppError::Internal(err.to_string())
        }
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::Serialization(err.to_string())
    }
}

impl From<toml::de::Error> for AppError {
    fn from(err: toml::de::Error) -> Self {
        AppError::Serialization(err.to_string())
    }
}
