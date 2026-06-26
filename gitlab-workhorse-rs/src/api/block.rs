#![allow(dead_code, unused_imports)]
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};

use super::RESPONSE_CONTENT_TYPE;

pub async fn block(req: Request, next: Next) -> Result<Response, StatusCode> {
    let response = next.run(req).await;

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if content_type.starts_with(RESPONSE_CONTENT_TYPE) {
        return Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "text/plain")
            .body(Body::from("Internal Server Error\n"))
            .unwrap());
    }

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::get, Router};
    use axum_test::TestServer;

    #[tokio::test]
    async fn test_block_internal_response() {
        async fn handler() -> Response {
            Response::builder()
                .header("content-type", RESPONSE_CONTENT_TYPE)
                .body(Body::from("secret data"))
                .unwrap()
        }

        let app = Router::new().route(
            "/",
            get(|| async {
                block_middleware(handler().await).await
            }),
        );

        let server = TestServer::new(app).unwrap();
        let response = server.get("/").await;

        assert_eq!(response.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(response.text().contains("Internal Server Error"));
    }

    #[tokio::test]
    async fn test_block_pass_through() {
        async fn handler() -> Response {
            Response::builder()
                .header("content-type", "text/plain")
                .body(Body::from("hello world"))
                .unwrap()
        }

        let app = Router::new().route(
            "/",
            get(|| async {
                block_middleware(handler().await).await
            }),
        );

        let server = TestServer::new(app).unwrap();
        let response = server.get("/").await;

        assert_eq!(response.status_code(), StatusCode::OK);
        assert!(response.text().contains("hello world"));
    }

    async fn block_middleware(response: Response) -> Response {
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if content_type.starts_with(RESPONSE_CONTENT_TYPE) {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("content-type", "text/plain")
                .body(Body::from("Internal Server Error\n"))
                .unwrap()
        } else {
            response
        }
    }
}
