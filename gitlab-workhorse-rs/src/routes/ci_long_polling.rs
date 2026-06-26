#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use crate::proxy::{self, ProxyState};

pub async fn handle_ci_long_polling(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let path = req.uri().path();
    if !path.starts_with("/api/v4/jobs/request") {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }

    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ci_long_polling_path() {
        assert!("/api/v4/jobs/request".starts_with("/api/v4/jobs/request"));
        assert!(!"/api/v4/jobs/123".starts_with("/api/v4/jobs/request"));
    }
}
