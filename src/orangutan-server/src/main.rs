mod config;
mod request_guards;
mod routes;
mod util;

use axum::{
    http::{Response, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
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

#[cfg(feature = "templating")]
use crate::util::templating;
use crate::{
    config::NOT_FOUND_FILE,
    routes::{main_route, update_content_routes},
    util::error,
};

#[derive(Clone, Default)]
struct AppState {
    #[cfg(feature = "templating")]
    tera: Tera,
}

#[tokio::main]
async fn main() {
    // build our application with a single route
    let app = Router::new().nest("/", routes::router()).layer(
        ServiceBuilder::new()
            .layer(TraceLayer::new_for_http())
            .layer(handle_refresh_token),
    );
    // .register("/", catchers![unauthorized, forbidden, not_found])

    let mut app_state = AppState::default();

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
            std::process::exit(1)
        }
    }

    info!("Generating default website");
    if let Err(err) = liftoff() {
        panic!("{err}");
    }

    let app = app.with_state(app_state);

    // Run our app with hyper, listening globally on port 8080.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();
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
            error(format!("Could not get default website directory: {}", err));
            return Err("This page doesn't exist or you are not allowed to see it.");
        },
    };
    let file_path = website_dir.join(NOT_FOUND_FILE);
    ServeFile::open(file_path.clone()).await.map_err(|err| {
        error(format!(
            "Could not read \"not found\" file at <{}>: {}",
            file_path.display(),
            err
        ));
        "This page doesn't exist or you are not allowed to see it."
    })
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
                StatusCode::UNAUTHORIZED.into()
            },
            #[cfg(feature = "templating")]
            Self::ClientError(_) => {
                debug!("{self}");
                StatusCode::BAD_REQUEST.into()
            },
            _ => {
                error(format!("{self}"));
                StatusCode::INTERNAL_SERVER_ERROR.into()
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
