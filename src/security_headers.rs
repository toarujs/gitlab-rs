use axum::{
    http::{HeaderValue, Request, StatusCode},
    middleware::Next,
    response::Response,
};

const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "SAMEORIGIN"),
];

pub async fn security_headers_middleware(
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let mut response = next.run(request).await;

    let is_html = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/html"))
        .unwrap_or(false);

    if is_html {
        let headers = response.headers_mut();
        for (name, value) in SECURITY_HEADERS {
            if !headers.contains_key(*name) {
                if let Ok(v) = HeaderValue::from_str(value) {
                    headers.insert(
                        name.parse::<axum::http::HeaderName>().unwrap(),
                        v,
                    );
                }
            }
        }
    }

    Ok(response)
}
