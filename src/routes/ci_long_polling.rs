#![allow(dead_code, unused_imports)]

use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use axum::http::header;
use axum::http::HeaderValue;
use crate::builds::{
    self, action_for_watch, parse_runner_request, should_watch, RegisterAction,
    MAX_REGISTER_BODY_SIZE, RUNNER_BUILD_QUEUE_HEADER_KEY, RUNNER_BUILD_QUEUE_HEADER_VALUE,
};
use crate::proxy;

pub async fn handle_ci_long_polling(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    let path = req.uri().path();
    if !path.starts_with("/api/v4/jobs/request") {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }

    if state.ci_long_polling.is_zero() {
        return proxy_register(state, req).await;
    }

    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_REGISTER_BODY_SIZE).await {
        Ok(b) => b,
        Err(_) => return polling_status(StatusCode::PAYLOAD_TOO_LARGE),
    };

    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let parsed = parse_runner_request(content_type, &bytes).ok();
    let watch = parsed
        .as_ref()
        .map(|r| should_watch(&r.token, &r.last_update, state.ci_long_polling))
        .unwrap_or(false);

    if !watch {
        let req = Request::from_parts(parts, Body::from(bytes));
        return proxy_register(state, req).await;
    }

    let runner = parsed.expect("watch implies parsed runner request");
    let redis = match state.redis.clone() {
        Some(client) => client,
        None => {
            let req = Request::from_parts(parts, Body::from(bytes));
            return proxy_register(state, req).await;
        }
    };

    let key = builds::runner_queue_key(&runner.token);
    match redis
        .watch_key(&key, &runner.last_update, state.ci_long_polling)
        .await
    {
        Ok(status) => match action_for_watch(status) {
            RegisterAction::Proxy => {
                tracing::debug!(?status, "ci long poll proxying to rails");
                let req = Request::from_parts(parts, Body::from(bytes));
                proxy_register(state, req).await
            }
            RegisterAction::NoContent => {
                tracing::debug!(?status, "ci long poll returning 204");
                polling_status(StatusCode::NO_CONTENT)
            }
        },
        Err(err) => {
            tracing::warn!(error = %err, "ci long poll watch failed, proxying");
            let req = Request::from_parts(parts, Body::from(bytes));
            proxy_register(state, req).await
        }
    }
}

async fn proxy_register(state: crate::state::AppState, req: Request<Body>) -> Response {
    match proxy::proxy_handler(State(state), req).await {
        Ok(mut resp) => {
            set_polling_header(&mut resp);
            resp
        }
        Err(status) => polling_status(status),
    }
}

fn set_polling_header(resp: &mut Response) {
    let name = axum::http::HeaderName::from_bytes(RUNNER_BUILD_QUEUE_HEADER_KEY.as_bytes())
        .expect("static polling header name");
    resp.headers_mut().insert(
        name,
        HeaderValue::from_static(RUNNER_BUILD_QUEUE_HEADER_VALUE),
    );
}

fn polling_status(status: StatusCode) -> Response {
    let mut resp = status.into_response();
    set_polling_header(&mut resp);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ci_long_polling_path() {
        assert!("/api/v4/jobs/request".starts_with("/api/v4/jobs/request"));
        assert!(!"/api/v4/jobs/123".starts_with("/api/v4/jobs/request"));
    }

    #[test]
    fn polling_204_sets_official_header() {
        let resp = polling_status(StatusCode::NO_CONTENT);
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let value = resp
            .headers()
            .get("gitlab-ci-builds-polling")
            .and_then(|v| v.to_str().ok());
        assert_eq!(value, Some("yes"));
    }
}
