use axum::{
    body::Body,
    extract::{FromRequest, Multipart, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use crate::proxy;

const PACKAGE_MAX_SIZE: usize = 5 * 1024 * 1024 * 1024; // 5GB

fn check_package_size(headers: &HeaderMap) -> Result<(), Response> {
    let content_length = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    if content_length > PACKAGE_MAX_SIZE {
        Err((StatusCode::PAYLOAD_TOO_LARGE, "Package too large").into_response())
    } else {
        Ok(())
    }
}

fn is_multipart(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("multipart/form-data"))
}

/// Routes that GitLab Workhorse accelerates with its request-body uploader.
///
/// The upload verbs must be buffered to a temp file and rewritten into a signed
/// form body; every other method (notably package downloads, which use GET on
/// the exact same paths) is proxied as-is.
macro_rules! package_upload_handler {
    ($name:ident) => {
        pub async fn $name(
            State(state): State<crate::state::AppState>,
            req: Request<Body>,
        ) -> Response {
            if matches!(req.method().as_str(), "PUT" | "POST") {
                let (parts, body) = req.into_parts();
                return crate::upload::request_body::accelerate_request_body(
                    state, parts.method, parts.uri, parts.headers, body,
                )
                .await;
            }
            if let Err(resp) = check_package_size(req.headers()) {
                return resp;
            }
            match proxy::proxy_handler(State(state), req).await {
                Ok(resp) => resp,
                Err(status) => (status, "").into_response(),
            }
        }
    };
}

/// Routes Workhorse leaves unaccelerated (it does not know their upload
/// protocol yet), or that share a path with non-upload endpoints.
macro_rules! package_proxy_handler {
    ($name:ident) => {
        pub async fn $name(
            State(state): State<crate::state::AppState>,
            req: Request<Body>,
        ) -> Response {
            if let Err(resp) = check_package_size(req.headers()) {
                return resp;
            }
            match proxy::proxy_handler(State(state), req).await {
                Ok(resp) => resp,
                Err(status) => (status, "").into_response(),
            }
        }
    };
}

/// Routes handled by Workhorse' *mime multipart* uploader (NuGet, PyPI, Helm).
///
/// These share their path with non-upload traffic, so the multipart rewrite is
/// attempted only for upload verbs that actually carry a multipart body;
/// everything else falls through to a plain proxy.
macro_rules! package_multipart_upload_handler {
    ($name:ident) => {
        pub async fn $name(
            State(state): State<crate::state::AppState>,
            req: Request<Body>,
        ) -> Response {
            if matches!(req.method().as_str(), "PUT" | "POST") && is_multipart(req.headers()) {
                let (parts, body) = req.into_parts();
                let method = parts.method.clone();
                let uri = parts.uri.clone();
                let headers = parts.headers.clone();
                let req = Request::from_parts(parts, body);
                return match Multipart::from_request(req, &state).await {
                    Ok(multipart) => {
                        crate::upload::accelerate::accelerate_package_multipart_request(
                            state,
                            method,
                            uri,
                            headers,
                            multipart,
                            PACKAGE_MAX_SIZE as u64,
                        )
                        .await
                    }
                    Err(_) => (StatusCode::BAD_REQUEST, "invalid multipart body").into_response(),
                };
            }
            if let Err(resp) = check_package_size(req.headers()) {
                return resp;
            }
            match proxy::proxy_handler(State(state), req).await {
                Ok(resp) => resp,
                Err(status) => (status, "").into_response(),
            }
        }
    };
}

package_upload_handler!(handle_maven_upload);
package_upload_handler!(handle_npm_upload);
package_upload_handler!(handle_conan_upload);
package_upload_handler!(handle_generic_upload);
package_upload_handler!(handle_debian_upload);
package_upload_handler!(handle_rpm_upload);
package_upload_handler!(handle_rubygems_upload);
package_upload_handler!(handle_terraform_upload);
package_upload_handler!(handle_ml_models_upload);

package_proxy_handler!(handle_npm_dist_tags);
package_multipart_upload_handler!(handle_nuget_upload);
package_multipart_upload_handler!(handle_pypi_upload);
package_multipart_upload_handler!(handle_helm_upload);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_package_max_size() {
        assert_eq!(PACKAGE_MAX_SIZE, 5 * 1024 * 1024 * 1024);
    }
}
