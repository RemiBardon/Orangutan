use std::{path::PathBuf, str::FromStr, time::SystemTime};

use axum::{
    extract::Path,
    http::{header::ACCEPT, HeaderMap},
    routing::get,
    Router,
};
use biscuit_auth::macros::authorizer;
use mime::Mime;
use orangutan_helpers::{
    page_metadata,
    website_id::{website_dir, WebsiteId},
};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{debug, trace};

use crate::{config::*, request_guards::Token, routes::debug_routes::log_access, util::error};

pub(super) fn router() -> Router {
    Router::new()
        .route("/", get(handle_request))
        .route("/*path", get(handle_request))
}

async fn handle_request(
    Path(path): Path<String>,
    token: Option<Token>,
    headers: HeaderMap<String>,
) -> Result<Option<()>, crate::Error> {
    // FIXME: Handle error
    let path = path;
    trace!("GET {}", &path);

    let user_profiles: Vec<String> = token.as_ref().map(Token::profiles).unwrap_or_default();
    debug!("User has profiles {user_profiles:?}");
    let website_id = WebsiteId::from(&user_profiles);
    let website_dir = website_dir(&website_id);

    // Log access only if the page is HTML.
    // WARN: This solution is far from perfect as someone requesting a page without setting the `Accept` header
    //   would not be logged even though they'd get the file back.
    let accept = headers
        .get(ACCEPT)
        .map(|value| value.parse::<Mime>().ok())
        .flatten();
    if accept.is_some_and(|m| m.type_() == mime::HTML) {
        log_access(user_profiles.to_owned(), path.to_owned());
    }

    let page_relpath = PathBuf::from_str(&path).unwrap();
    let Some(page_metadata) = page_metadata(&page_relpath)? else {
        // If metadata can't be found, it means it's a static file
        trace!("File <{path> did not explicitly allow profiles, serving static file");
        // TODO: Un-hardcode this value.
        return ServeDir::new("static")
            .not_found_service(ServeFile::new(website_dir.join(NOT_FOUND_FILE)));
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
        return Ok(None);
    }

    let page_abspath = website_dir.join(page_metadata.path);

    ServeFile::new(page_abspath);
    ServeFile::new(website_dir.join(NOT_FOUND_FILE));

    panic!()
}
