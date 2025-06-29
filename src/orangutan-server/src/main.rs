mod config;
mod request_guards;
mod routes;
mod util;

use std::{fs, process::ExitCode};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    Router,
};
use orangutan_helpers::{
    generate::{self, *},
    website_id::WebsiteId,
};
use request_guards::{handle_refresh_token, REVOKED_TOKENS};
use tokio::runtime::Handle;
use tower::Service;
use tower_http::{
    services::{fs::ServeFileSystemResponseBody, ServeFile},
    trace::TraceLayer,
};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;
#[cfg(feature = "token-generator")]
use util::WebsiteRoot;

#[cfg(feature = "templating")]
use crate::util::templating;
use crate::{config::NOT_FOUND_FILE, routes::update_content_routes, util::error};

#[derive(Clone)]
struct AppState {
    #[cfg(feature = "token-generator")]
    website_root: WebsiteRoot,
    #[cfg(feature = "templating")]
    tera: tera::Tera,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    #[cfg(feature = "token-generator")]
    let website_root = match WebsiteRoot::try_from_env() {
        Ok(r) => r,
        Err(err) => {
            tracing::error!("{err}");
            return ExitCode::FAILURE;
        },
    };

    let app_state = AppState {
        #[cfg(feature = "token-generator")]
        website_root,
        #[cfg(feature = "templating")]
        tera: Default::default(),
    };

    // Add support for templating if needed
    #[cfg(feature = "templating")]
    let mut app_state = app_state;
    #[cfg(feature = "templating")]
    {
        info!("Initializing templating engine…");
        if let Err(err) = app_state.tera.add_raw_templates(routes::templates()) {
            tracing::error!("{err}");
            return ExitCode::FAILURE;
        }
    }

    let app = Router::new()
        .nest("/", routes::router())
        .layer(TraceLayer::new_for_http())
        .route_layer(middleware::from_fn(handle_refresh_token))
        .with_state(app_state);
    // .register("/", catchers![unauthorized, forbidden, not_found])

    info!("Generating default website");
    if let Err(err) = liftoff() {
        tracing::error!("{err}");
        return ExitCode::FAILURE;
    }

    // Run our app with hyper, listening globally on port 8080.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();

    return ExitCode::SUCCESS;
}

fn liftoff() -> Result<(), Error> {
    create_tmp_dir()?;
    clone_repository()?;
    // NOTE: This is just a hotfix. I had to quickly revoke a token. I'll improve this one day.
    *REVOKED_TOKENS.write().unwrap() = read_revoked_tokens()?;
    generate_default_website()?;
    Ok(())
}

/// TODO: Re-enable Basic authentication
///   (`.raw_header("WWW-Authenticate", "Basic realm=\"This page is protected. Please log in.\"")`).
fn not_found() -> Result<Response<ServeFileSystemResponseBody>, Response> {
    fn fallback() -> Response {
        let mut fallback =
            "This page doesn't exist or you are not allowed to see it.".into_response();
        *fallback.status_mut() = StatusCode::NOT_FOUND;
        fallback
    }

    let website_id = WebsiteId::default();
    let website_dir = generate_website_if_needed(&website_id).map_err(|err| {
        error(format!("Could not get default website directory: {err}"));
        fallback()
    })?;

    let file_path = website_dir.join(NOT_FOUND_FILE);
    match fs::metadata(&file_path) {
        Ok(_) => {
            let res = tokio::task::block_in_place(move || {
                Handle::current().block_on(async move {
                    ServeFile::new(file_path.clone())
                        .call(Request::new(Body::empty()))
                        .await
                        .map_err(|err| match err {})
                        .unwrap()
                })
            });
            Ok(res)
        },
        Err(err) => {
            error(format!(
                "Could not read \"not found\" file at <{}>: {err}",
                file_path.display(),
            ));
            Err(fallback())
        },
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    WebsiteGenerationError(#[from] generate::Error),
    #[error("Could not update content: {0}")]
    UpdateContentError(#[from] update_content_routes::Error),
    #[error("Unauthorized")]
    Unauthorized,
    #[error("Forbidden")]
    Forbidden,
    #[error("Token revoked")]
    TokenRevoked,
    #[cfg(feature = "templating")]
    #[error("Templating error: {0}")]
    TemplatingError(#[from] templating::Error),
    #[error("Internal server error: {0}")]
    InternalServerError(String),
    #[error("Client error: {0}")]
    ClientError(String),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => {
                warn!("{self}");
                (StatusCode::UNAUTHORIZED, not_found()).into_response()
            },
            Self::Forbidden => {
                warn!("{self}");
                (StatusCode::FORBIDDEN, not_found()).into_response()
            },
            Self::TokenRevoked => {
                warn!("{self}");
                (StatusCode::FORBIDDEN, "403 Forbidden. Token revoked.").into_response()
            },
            Self::ClientError(_) => {
                debug!("{self}");
                (StatusCode::BAD_REQUEST, not_found()).into_response()
            },
            _ => {
                error(format!("{self}"));
                (StatusCode::INTERNAL_SERVER_ERROR, "I messed up. My bad 🙊").into_response()
            },
        }
    }
}

#[cfg(feature = "templating")]
impl From<orangutan_refresh_token::Error> for Error {
    fn from(err: orangutan_refresh_token::Error) -> Self {
        match err {
            orangutan_refresh_token::Error::CannotAddFact(_, _)
            | orangutan_refresh_token::Error::CannotBuildBiscuit(_)
            | orangutan_refresh_token::Error::CannotAddBlock(_, _)
            | orangutan_refresh_token::Error::CannotConvertToBase64(_) => {
                Self::InternalServerError(format!("Token generation error: {err}"))
            },
            orangutan_refresh_token::Error::MalformattedDuration(_, _)
            | orangutan_refresh_token::Error::UnsupportedDuration(_) => {
                Self::ClientError(format!("Invalid token data: {err}"))
            },
        }
    }
}
