use std::{path::PathBuf, str::FromStr as _};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header::HeaderMap, Uri},
    response::Response,
    routing::get,
    Router,
};
use orangutan_helpers::{
    generate::generate_website_if_needed,
    page_metadata,
    website_id::{website_dir, WebsiteId},
};
use tower::ServiceExt;
use tower_http::services::{fs::ServeFileSystemResponseBody, ServeDir, ServeFile};
use tracing::{debug, trace};

use crate::{
    auth::is_authorized,
    request_guards::Token,
    routes::debug_routes::log_access,
    util::{accepts, VecExt as _},
    AppState, Error,
};

pub(super) fn router() -> Router<AppState> {
    Router::<AppState>::new()
        .route("/", get(handle_request))
        .route("/{*path}", get(handle_request))
}

// #[axum::debug_handler]
#[tracing::instrument(skip_all, fields(uri))]
async fn handle_request(
    State(_app_state): State<AppState>,
    uri: Uri,
    token: Option<Token>,
    ref headers: HeaderMap,
    req: Request<Body>,
) -> Result<Response<ServeFileSystemResponseBody>, Error> {
    let path = uri.path();

    let user_profiles: Vec<String> = token.as_ref().map(Token::profiles).unwrap_or_default();
    // debug!("User has profiles {user_profiles:?}");
    tracing::Span::current().record("profiles", &user_profiles.sorted().join(","));

    let website_id = WebsiteId::from(&user_profiles);
    tracing::Span::current().record("website_id", &website_id.name());

    // Log access only if the page is HTML.
    // WARN: This solution is far from perfect as someone requesting a page
    //   without setting the `Accept` header would not be logged even though
    //   they’d get the file back.
    if accepts(headers, mime::TEXT_HTML) {
        log_access(user_profiles.to_owned(), path.to_owned());
    }

    // Generate the website if needed.
    generate_website_if_needed(&website_id)?;

    let page_relpath = PathBuf::from_str(&path).unwrap();
    let Some(page_metadata) = page_metadata(&page_relpath)
        .map_err(orangutan_helpers::generate::Error::CannotReadPageMetadata)?
    else {
        // If metadata can’t be found, it means it’s a static file.
        trace!("File <{path}> did not explicitly allow profiles, serving static file.");
        return Ok(serve_file(&website_id, req).await);
    };

    let allowed_profiles = page_metadata.read_allowed;
    tracing::Span::current().record("allowed_profiles", &allowed_profiles.join(","));

    if is_authorized(token, allowed_profiles) {
        Ok(serve_file(&website_id, req).await)
    } else {
        debug!("No allowed profile found in token.");
        Err(Error::Forbidden)
    }
}

async fn serve_file(
    website_id: &WebsiteId,
    req: Request<Body>,
) -> Response<ServeFileSystemResponseBody> {
    let website_dir = website_dir(&website_id);

    let fallback = website_dir.join(crate::config::NOT_FOUND_FILE);
    trace!(
        "Serving {path} at {website_dir} falling back to {fallback}…",
        path = req.uri().path(),
        website_dir = website_dir.display(),
        fallback = fallback.display(),
    );

    let service = ServeDir::new(website_dir)
        // NOTE: Default behavior exlicited for clarity.
        .append_index_html_on_directories(true)
        .not_found_service(ServeFile::new(fallback));

    service
        .oneshot(req)
        .await
        .map_err(|err| match err {})
        .unwrap()
}
