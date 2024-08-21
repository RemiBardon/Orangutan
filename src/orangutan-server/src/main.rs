mod config;
mod request_guards;
mod routes;
mod util;

use std::{fs, process::ExitCode};

use axum::{
    extract::FromRef,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    Router,
};
use axum_extra::extract::cookie::Key;
use orangutan_helpers::{
    generate::{self, *},
    website_id::WebsiteId,
};
use request_guards::{handle_refresh_token, REVOKED_TOKENS};
use tera::Tera;
use tower_http::{services::ServeFile, trace::TraceLayer};
#[cfg(feature = "templating")]
use tracing::debug;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, FmtSubscriber};
use util::WebsiteRoot;

#[cfg(feature = "templating")]
use crate::util::templating;
use crate::{config::NOT_FOUND_FILE, routes::update_content_routes, util::error};

#[derive(Clone)]
struct AppState {
    website_root: WebsiteRoot,
    cookie_key: Key,
    #[cfg(feature = "templating")]
    tera: Tera,
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.cookie_key.clone()
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let website_root = match WebsiteRoot::try_from_env() {
        Ok(r) => r,
        Err(err) => {
            tracing::error!("{err}");
            return ExitCode::FAILURE;
        },
    };

    let mut app_state = AppState {
        website_root,
        // FIXME: Use predefined key.
        cookie_key: Key::generate(),
        #[cfg(feature = "templating")]
        tera: Default::default(),
    };

    info!("Setting up tracing…");
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber.");

    // Add support for templating if needed
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
        .route_layer(middleware::from_fn_with_state(
            app_state.clone(),
            handle_refresh_token,
        ))
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

// #[catch(401)]
async fn unauthorized() -> Result<ServeFile, &'static str> {
    not_found().await
}

// #[catch(403)]
async fn forbidden() -> &'static str {
    "403 Forbidden. Token revoked."
}

/// TODO: Re-enable Basic authentication
///   (`.raw_header("WWW-Authenticate", "Basic realm=\"This page is protected. Please log in.\"")`).
// #[catch(404)]
async fn not_found() -> Result<ServeFile, &'static str> {
    let website_id = WebsiteId::default();
    let website_dir = match generate_website_if_needed(&website_id) {
        Ok(dir) => dir,
        Err(err) => {
            error(format!("Could not get default website directory: {err}"));
            return Err("This page doesn't exist or you are not allowed to see it.");
        },
    };
    let file_path = website_dir.join(NOT_FOUND_FILE);
    match fs::exists(&file_path) {
        Ok(true) => Ok(ServeFile::new(file_path.clone())),
        Ok(false) => {
            error(format!(
                "Could not read \"not found\" file at <{}>: File doesn't exist.",
                file_path.display(),
            ));
            Err("This page doesn't exist or you are not allowed to see it.")
        },
        Err(err) => {
            error(format!(
                "Could not read \"not found\" file at <{}>: {err}",
                file_path.display(),
            ));
            Err("This page doesn't exist or you are not allowed to see it.")
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
    #[cfg(feature = "templating")]
    #[error("Templating error: {0}")]
    TemplatingError(#[from] templating::Error),
    #[cfg(feature = "templating")]
    #[error("Internal server error: {0}")]
    InternalServerError(String),
    #[cfg(feature = "templating")]
    #[error("Client error: {0}")]
    ClientError(String),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => {
                warn!("{self}");
                StatusCode::UNAUTHORIZED.into_response()
            },
            #[cfg(feature = "templating")]
            Self::ClientError(_) => {
                debug!("{self}");
                StatusCode::BAD_REQUEST.into_response()
            },
            _ => {
                error(format!("{self}"));
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
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
