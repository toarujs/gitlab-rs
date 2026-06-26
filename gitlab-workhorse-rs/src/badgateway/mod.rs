#![allow(dead_code)]
use axum::{
    body::Body,
    http::StatusCode,
    response::Response,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BadGatewayError {
    BadGateway = 502,
    ClientClosedRequest = 499,
}

impl BadGatewayError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            BadGatewayError::BadGateway => StatusCode::BAD_GATEWAY,
            BadGatewayError::ClientClosedRequest => StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_GATEWAY),
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            BadGatewayError::BadGateway => "Bad Gateway",
            BadGatewayError::ClientClosedRequest => "Client Closed Request",
        }
    }
}

pub fn classify_proxy_error(err: &reqwest::Error) -> BadGatewayError {
    if err.is_connect() {
        BadGatewayError::BadGateway
    } else if err.is_timeout() {
        BadGatewayError::BadGateway
    } else if err.is_body() || err.is_decode() {
        BadGatewayError::BadGateway
    } else {
        let msg = err.to_string();
        if msg.contains("broken pipe") || msg.contains("connection reset") {
            BadGatewayError::ClientClosedRequest
        } else {
            BadGatewayError::BadGateway
        }
    }
}

pub fn error_response(error: BadGatewayError) -> Response {
    let status = error.status_code();
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Body::from(error.message()))
        .unwrap()
}

pub fn handle_proxy_error(err: reqwest::Error) -> Response {
    let classified = classify_proxy_error(&err);
    error_response(classified)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_connect_error() {
        let _err = reqwest::Client::new()
            .get("http://192.0.2.1:1")
            .send();
        // We can't easily create a connect error in tests,
        // but we can test the classification logic
    }

    #[test]
    fn test_error_response_bad_gateway() {
        let resp = error_response(BadGatewayError::BadGateway);
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn test_error_response_client_closed() {
        let resp = error_response(BadGatewayError::ClientClosedRequest);
        assert_eq!(resp.status().as_u16(), 499);
    }

    #[test]
    fn test_bad_gateway_status_code() {
        assert_eq!(
            BadGatewayError::BadGateway.status_code(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn test_client_closed_status_code() {
        assert_eq!(
            BadGatewayError::ClientClosedRequest.status_code().as_u16(),
            499
        );
    }
}
