use orangutan_helpers::generate::{self, *};
use rocket::{post, response::status::BadRequest, routes, Route};

use crate::error;

pub(super) fn routes() -> Vec<Route> {
    routes![update_content_github, update_content_other]
}

/// TODO: [Validate webhook deliveries](https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#validating-webhook-deliveries)
#[post("/update-content/github")]
fn update_content_github() -> Result<(), crate::Error> {
    // Update repository
    pull_repository().map_err(Error::CannotPullOutdatedRepository)?;

    // Remove outdated websites
    let state = trash_outdated_websites().map_err(Error::CannotTrashOutdatedWebsites)?;

    // Pre-generate default website as we will access it at some point anyway
    match generate_default_website().map_err(Error::WebsiteGenerationError) {
        Err(err) => {
            error(format!("{err}"));
            recover_trash(state).map_err(Error::CannotRecoverTrash)?
        },
        Ok(()) => empty_trash(state).map_err(Error::CannotEmptyTrash)?,
    }

    Ok(())
}

#[post("/update-content/<source>")]
fn update_content_other(source: &str) -> BadRequest<String> {
    BadRequest(format!("Source '{source}' is not supported."))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Website generation error: {0}")]
    WebsiteGenerationError(generate::Error),
    #[error("Cannot pull outdated repository: {0}")]
    CannotPullOutdatedRepository(generate::Error),
    #[error("Cannot trash outdated websites: {0}")]
    CannotTrashOutdatedWebsites(generate::Error),
    #[error("Cannot recover trash: {0}")]
    CannotRecoverTrash(generate::Error),
    #[error("Cannot empty trash: {0}")]
    CannotEmptyTrash(generate::Error),
}
