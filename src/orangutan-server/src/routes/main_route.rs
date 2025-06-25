use std::{path::PathBuf, str::FromStr, time::SystemTime};

use axum::{
    extract::{Request, State},
    http::{
        header::{HeaderMap, ACCEPT},
        Uri,
    },
    response::Response,
    routing::get,
    Router,
};
use axum_extra::either::Either;
use biscuit_auth::macros::authorizer;
use mime::Mime;
use orangutan_helpers::{
    page_metadata,
    website_id::{website_dir, WebsiteId},
};
use tower::Service;
use tower_http::services::{fs::ServeFileSystemResponseBody, ServeDir, ServeFile};
use tracing::{debug, trace};

use crate::{config::*, request_guards::Token, routes::debug_routes::log_access, AppState, Error};

pub(super) fn router() -> Router<AppState> {
    Router::<AppState>::new()
        .route("/", get(handle_request))
        .route("/*path", get(handle_request))
}

// #[axum::debug_handler]
async fn handle_request(
    State(_app_state): State<AppState>,
    uri: Uri,
    token: Option<Token>,
    ref headers: HeaderMap,
) -> Result<Either<Response<ServeFileSystemResponseBody>, Error>, Error> {
    let path = uri.path();
    trace!("GET {}", &path);

    let user_profiles: Vec<String> = token.as_ref().map(Token::profiles).unwrap_or_default();
    debug!("User has profiles {user_profiles:?}");
    let website_id = WebsiteId::from(&user_profiles);
    let website_dir = website_dir(&website_id);

    // Log access only if the page is HTML.
    // WARN: This solution is far from perfect as someone requesting a page
    //   without setting the `Accept` header would not be logged even though
    //   they’d get the file back.
    fn accepts(
        headers: &HeaderMap,
        mime: Mime,
    ) -> bool {
        // NOTE: Real-life example `Accept`: "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"
        let Some(accept) = headers.get(ACCEPT) else {
            return false;
        };

        // Transform the header value into a string with only visible ASCII
        // characters.
        let accept = match accept.to_str() {
            Ok(str) => str,
            Err(err) => {
                debug!("'{accept:?}' could not be mapped to a string: {err}");
                return false;
            },
        };

        let mut mimes = accept
            // Split the header value into individual MIME types.
            .split(",")
            .filter_map(|mime| -> Option<Mime> {
                mime.parse::<Mime>()
                    .inspect_err(|err| debug!("'{mime}' could not be mapped to a MIME type: {err}"))
                    .ok()
            });

        let expected_mime = mime.essence_str();
        mimes.any(|mime| mime.essence_str() == expected_mime)
    }
    if accepts(headers, mime::TEXT_HTML) {
        log_access(user_profiles.to_owned(), path.to_owned());
    }

    async fn serve_file(
        website_dir: &PathBuf,
        path: &Uri,
    ) -> Response<ServeFileSystemResponseBody> {
        let fallback = website_dir.join(NOT_FOUND_FILE);
        trace!(
            "Serving {path} at {} falling back to {}…",
            website_dir.display(),
            fallback.display(),
        );
        let mut serve_dir = ServeDir::new(website_dir).not_found_service(ServeFile::new(fallback));
        serve_dir
            .call(Request::get(path).body(()).unwrap())
            .await
            .map_err(|err| match err {})
            .unwrap()
    }

    let page_relpath = PathBuf::from_str(&path).unwrap();
    let Some(page_metadata) = page_metadata(&page_relpath)
        .map_err(orangutan_helpers::generate::Error::CannotReadPageMetadata)?
    else {
        // If metadata can't be found, it means it's a static file
        trace!("File <{path}> did not explicitly allow profiles, serving static file");
        // TODO: Un-hardcode this value.
        let res = serve_file(&website_dir, &Uri::from_str(path).unwrap()).await;
        return Ok(Either::E1(res));
    };

    let allowed_profiles = page_metadata.read_allowed;
    // debug!(
    //     "Page <{}> can be read by {}",
    //     &path,
    //     allowed_profiles
    //         .iter()
    //         .map(|p| format!("'{}'", p))
    //         .collect::<Vec<_>>()
    //         .join(", ")
    // );

    let mut profile: Option<String> = None;
    let biscuit = token.map(|t| t.biscuit);
    for allowed_profile in allowed_profiles {
        trace!("Checking if profile '{allowed_profile}' exists in token…");
        if allowed_profile == DEFAULT_PROFILE {
            profile = Some(allowed_profile);
        } else if let Some(ref biscuit) = biscuit {
            let authorizer = authorizer!(
                r#"
                operation("read");
                time({now});
                right({p}, "read");
                right("*", "read");

                allow if
                operation($op),
                profile($p),
                right($p, $op);
                "#,
                p = allowed_profile.clone(),
                now = SystemTime::now()
            );
            // trace!(
            //     "Running authorizer '{}' on '{}'…",
            //     authorizer.dump_code(),
            //     biscuit.authorizer().unwrap().dump_code()
            // );
            if biscuit.authorize(&authorizer).is_ok() {
                profile = Some(allowed_profile);
            }
        }
    }
    if profile.is_none() {
        debug!("No profile allowed in token");
        return Ok(Either::E2(Error::Forbidden));
    }

    let res = serve_file(
        &website_dir,
        &Uri::from_str(page_metadata.path.to_str().unwrap()).unwrap(),
    )
    .await;
    Ok(Either::E1(res))
}
