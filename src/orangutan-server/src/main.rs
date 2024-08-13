mod config;
mod request_guards;
mod routes;
mod util;

use std::{
    convert::Infallible,
    fmt::Display,
    ops::Deref,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, RwLock, RwLockReadGuard,
    },
};

use object_reader::ObjectReader;
use orangutan_helpers::{
    generate::{self, *},
    readers::object_reader,
    website_id::WebsiteId,
};
use rocket::{
    catch, catchers,
    fairing::{self, AdHoc, Fairing},
    fs::NamedFile,
    http::Status,
    request::{self, FromRequest},
    response::{self, Responder},
    Request,
};
use routes::auth_routes::REVOKED_TOKENS;
#[cfg(feature = "templating")]
use tracing::debug;
use tracing::warn;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[cfg(feature = "templating")]
use crate::util::templating;
use crate::{
    config::NOT_FOUND_FILE,
    routes::{main_route, update_content_routes},
    util::error,
};

#[rocket::launch]
fn rocket() -> _ {
    let rocket = rocket::build()
        .mount("/", routes::routes())
        .register("/", catchers![unauthorized, forbidden, not_found])
        .manage(ObjectReader::new())
        .attach(AdHoc::on_ignite("Tracing subsciber", |rocket| async move {
            let subscriber = FmtSubscriber::builder()
                .with_env_filter(EnvFilter::from_default_env())
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("Failed to set tracing subscriber.");
            rocket
        }))
        .attach(AdHoc::on_liftoff("Website generation", |rocket| {
            Box::pin(async move {
                if let Err(err) = liftoff() {
                    // We drop the error to get a Rocket-formatted panic.
                    drop(err);
                    rocket.shutdown().notify();
                }
            })
        }))
        .attach(RequestIdFairing)
        .attach(TracingSpanFairing);

    // Add support for templating if needed
    #[cfg(feature = "templating")]
    let rocket = rocket.attach(AdHoc::on_ignite(
        "Initialize templating engine",
        |rocket| async move {
            let mut tera = tera::Tera::default();
            if let Err(err) = tera.add_raw_templates(routes::templates()) {
                tracing::error!("{err}");
                std::process::exit(1)
            }
            rocket.manage(tera)
        },
    ));

    rocket
}

fn liftoff() -> Result<(), Error> {
    create_tmp_dir()?;
    clone_repository()?;
    // NOTE: This is just a hotfix. I had to quickly revoke a token. I'll improve this one day.
    *REVOKED_TOKENS.write().unwrap() = read_revoked_tokens()?;
    generate_default_website()?;
    Ok(())
}

#[catch(401)]
async fn unauthorized() -> Result<NamedFile, &'static str> {
    not_found().await
}

#[catch(403)]
async fn forbidden() -> &'static str {
    "403 Forbidden. Token revoked."
}

/// TODO: Re-enable Basic authentication
///   (`.raw_header("WWW-Authenticate", "Basic realm=\"This page is protected. Please log in.\"")`).
#[catch(404)]
async fn not_found() -> Result<NamedFile, &'static str> {
    let website_id = WebsiteId::default();
    let website_dir = match generate_website_if_needed(&website_id) {
        Ok(dir) => dir,
        Err(err) => {
            error(format!("Could not get default website directory: {}", err));
            return Err("This page doesn't exist or you are not allowed to see it.");
        },
    };
    let file_path = website_dir.join(NOT_FOUND_FILE);
    NamedFile::open(file_path.clone()).await.map_err(|err| {
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
    #[error(transparent)]
    MainRouteError(#[from] main_route::Error),
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

#[rocket::async_trait]
impl<'r> Responder<'r, 'static> for Error {
    fn respond_to(
        self,
        _: &'r Request<'_>,
    ) -> response::Result<'static> {
        match self {
            Self::Unauthorized => {
                warn!("{self}");
                Err(Status::Unauthorized)
            },
            #[cfg(feature = "templating")]
            Self::ClientError(_) => {
                debug!("{self}");
                Err(Status::BadRequest)
            },
            _ => {
                error(format!("{self}"));
                Err(Status::InternalServerError)
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

// ===== Request ID =====

#[derive(Debug, Clone)]
pub struct RequestId(String);
impl Deref for RequestId {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Display for RequestId {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        Display::fmt(self.deref(), f)
    }
}
#[rocket::async_trait]
impl<'r> FromRequest<'r> for RequestId {
    type Error = Infallible;

    async fn from_request(req: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        static COUNTER: AtomicUsize = AtomicUsize::new(1);
        let request_id = RequestId(
            req.headers()
                .get_one("X-Request-Id")
                .map(ToString::to_string)
                .unwrap_or_else(|| COUNTER.fetch_add(1, Ordering::Relaxed).to_string()),
        );
        request::Outcome::Success(request_id)
    }
}

#[rocket::async_trait]
trait RequestIdTrait {
    async fn id(&self) -> RequestId;
}

#[rocket::async_trait]
impl<'r> RequestIdTrait for Request<'r> {
    async fn id(&self) -> RequestId {
        self.guard::<RequestId>().await.unwrap().to_owned()
    }
}

// ===== Request ID fairing =====

struct RequestIdFairing;

#[rocket::async_trait]
impl Fairing for RequestIdFairing {
    fn info(&self) -> fairing::Info {
        fairing::Info {
            name: "Add a unique request ID to every request",
            kind: fairing::Kind::Request,
        }
    }

    /// See <https://rocket.rs/guide/v0.5/state/#request-local-state>
    /// and <https://users.rust-lang.org/t/idiomatic-rust-way-to-generate-unique-id/33805/6>.
    async fn on_request(
        &self,
        req: &mut Request<'_>,
        _: &mut rocket::Data<'_>,
    ) {
        static COUNTER: AtomicUsize = AtomicUsize::new(1);
        let request_id = RequestId(
            req.headers()
                .get_one("X-Request-Id")
                .map(ToString::to_string)
                .unwrap_or_else(|| COUNTER.fetch_add(1, Ordering::Relaxed).to_string()),
        );
        req.local_cache(|| request_id);
    }
}

// ===== Tracing span =====

#[derive(Clone)]
pub struct TracingSpan(Arc<RwLock<tracing::Span>>);

impl TracingSpan {
    pub fn get(&self) -> RwLockReadGuard<'_, tracing::Span> {
        self.0.read().unwrap()
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for TracingSpan {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, ()> {
        let span: &TracingSpan =
            request.local_cache(|| panic!("Tracing span should be managed by then"));
        request::Outcome::Success(span.to_owned())
    }
}

// ===== Tracing span fairing =====

struct TracingSpanFairing;

#[rocket::async_trait]
impl Fairing for TracingSpanFairing {
    fn info(&self) -> fairing::Info {
        fairing::Info {
            name: "Add request information to tracing span",
            kind: fairing::Kind::Request,
        }
    }

    async fn on_request(
        &self,
        req: &mut Request<'_>,
        _: &mut rocket::Data<'_>,
    ) {
        let user_agent = req.headers().get_one("User-Agent").unwrap_or("none");
        let request_id = req.id().await;

        let span = tracing::debug_span!(
            "request",
            request_id = %request_id,
            http.method = %req.method(),
            http.uri = %req.uri().path(),
            http.user_agent = %user_agent,
            otel.name=%format!("{} {}", req.method(), req.uri().path()),
        );

        req.local_cache(|| TracingSpan(Arc::new(RwLock::new(span))));
    }
}
