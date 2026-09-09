use axum::{
    body::Body,
    extract::{Request, State},
    http::Method,
    response::{IntoResponse, Response},
};
use crate::proxy;

pub async fn handle_projects_index(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
) -> Response {
    if req.method() == Method::GET {
        match crate::hotpath::projects::accelerate(&state, &req).await {
            crate::hotpath::projects::AccelResult::Hit(resp) => return resp,
            crate::hotpath::projects::AccelResult::Fallback { reason, error } => {
                tracing::warn!(
                    path_template = %crate::hotpath::PROJECTS_PATH_TEMPLATE,
                    reason = %reason,
                    error = error,
                    "Projects list accelerate fallback to Puma"
                );
            }
        }
    }
    match proxy::proxy_handler(State(state), req).await {
        Ok(resp) => resp,
        Err(status) => (status, "").into_response(),
    }
}
