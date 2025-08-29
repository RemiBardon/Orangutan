use axum::RequestExt as _;

use crate::request_guards::Token;

/// Attach a request ID to improve debugging.
pub async fn request_id_middleware(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Generate a unique ID (UUIDv4)
    let request_id = {
        let uri = req.uri().clone();
        let token = req.extract_parts::<Token>().await.ok();

        use std::hash::{DefaultHasher, Hash as _, Hasher as _};
        let mut hasher = DefaultHasher::new();
        (uri, token).hash(&mut hasher);
        hasher.finish()
    };

    tracing::Span::current().record("request_id", request_id);

    // Insert into request extensions so handlers can access it
    req.extensions_mut().insert(request_id.clone());

    // Continue down the stack
    let mut response = next.run(req).await;

    // Optionally, include it in response headers
    response
        .headers_mut()
        .insert("x-request-id", request_id.to_string().parse().unwrap());

    response
}

/// Attach tracing records to improve debugging.
pub async fn tracing_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use tracing::trace;

    let span = tracing::Span::current();

    let path = req.uri().path();
    trace!("GET {}", &path);
    span.record("path", req.uri().path());

    next.run(req).await
}
