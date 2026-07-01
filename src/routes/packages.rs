use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use crate::proxy;

const PACKAGE_MAX_SIZE: usize = 5 * 1024 * 1024 * 1024; // 5GB

fn check_package_size(req: &Request<Body>) -> Result<(), Response> {
    let content_length = req
        .headers()
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

macro_rules! package_upload_handler {
    ($name:ident) => {
        pub async fn $name(
            State(state): State<crate::state::AppState>,
            req: Request<Body>,
        ) -> Response {
            if let Err(resp) = check_package_size(&req) {
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
package_upload_handler!(handle_nuget_upload);
package_upload_handler!(handle_conan_upload);
package_upload_handler!(handle_generic_upload);
package_upload_handler!(handle_pypi_upload);
package_upload_handler!(handle_debian_upload);
package_upload_handler!(handle_rpm_upload);
package_upload_handler!(handle_rubygems_upload);
package_upload_handler!(handle_terraform_upload);
package_upload_handler!(handle_helm_upload);
package_upload_handler!(handle_ml_models_upload);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_package_max_size() {
        assert_eq!(PACKAGE_MAX_SIZE, 5 * 1024 * 1024 * 1024);
    }
}
