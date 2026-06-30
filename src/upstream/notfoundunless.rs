#![allow(dead_code)]
use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    response::Response,
};

pub fn not_found_unless_response(pass: bool) -> Option<Response> {
    if pass {
        None
    } else {
        Some(
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("404 Not Found"))
                .unwrap(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_not_found_unless_pass_true() {
        assert!(not_found_unless_response(true).is_none());
    }

    #[test]
    fn test_not_found_unless_pass_false() {
        let resp = not_found_unless_response(false).unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
