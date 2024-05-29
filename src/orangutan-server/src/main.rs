mod config;
mod request_guards;
mod routes;
mod util;

use object_reader::ObjectReader;
use orangutan_helpers::{
    generate::{self, *},
    readers::object_reader,
    website_id::WebsiteId,
};
use rocket::{
    catch, catchers,
    fairing::AdHoc,
    fs::NamedFile,
    http::Status,
    response::{self, Responder},
    Request,
};
use routes::{main_route, update_content_routes};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use util::error;

use crate::config::NOT_FOUND_FILE;

#[rocket::launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", routes::routes())
        .register("/", catchers![unauthorized, not_found])
        .manage(ObjectReader::new())
        .attach(AdHoc::on_liftoff("Tracing subsciber", |_| {
            Box::pin(async move {
                let subscriber = FmtSubscriber::builder()
                    .with_max_level(Level::TRACE)
                    .finish();
                tracing::subscriber::set_global_default(subscriber)
                    .expect("Failed to set tracing subscriber.");
            })
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
}

fn liftoff() -> Result<(), Error> {
    clone_repository().map_err(Error::WebsiteGenerationError)?;
    generate_default_website().map_err(Error::WebsiteGenerationError)?;
    Ok(())
}

#[catch(401)]
async fn unauthorized() -> Result<NamedFile, &'static str> {
    not_found().await
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
    #[error("Website generation error: {0}")]
    WebsiteGenerationError(generate::Error),
    #[error(transparent)]
    MainRouteError(#[from] main_route::Error),
    #[error("Could not update content: {0}")]
    UpdateContentError(#[from] update_content_routes::Error),
}

#[rocket::async_trait]
impl<'r> Responder<'r, 'static> for Error {
    fn respond_to(
        self,
        _: &'r Request<'_>,
    ) -> response::Result<'static> {
        error(format!("{self}"));
        Err(Status::InternalServerError)
    }
}
