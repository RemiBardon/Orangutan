use axum::{extract::Path, routing::post, Router};
use orangutan_helpers::generate::{self, *};

use crate::{error, request_guards::REVOKED_TOKENS, AppState};

pub(super) fn router() -> Router<AppState> {
    Router::<AppState>::new()
        .route("/update-content/github", post(update_content_github))
        .route("/update-content/:source", post(update_content_other))
}

/// TODO: [Validate webhook deliveries](https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#validating-webhook-deliveries)
async fn update_content_github() -> Result<(), crate::Error> {
    // Update repository
    pull_repository().map_err(Error::CannotPullOutdatedRepository)?;

    // Read revoked tokens list
    // FIXME: This cannot be reverted
    *REVOKED_TOKENS.write().unwrap() =
        read_revoked_tokens().map_err(Error::CannotReadRevokedTokens)?;

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

async fn update_content_other(Path(source): Path<String>) -> crate::Error {
    crate::Error::ClientError(format!("Source '{source}' is not supported."))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Website generation error: {0}")]
    WebsiteGenerationError(generate::Error),
    #[error("Cannot pull outdated repository: {0}")]
    CannotPullOutdatedRepository(generate::Error),
    #[error("Cannot read revoked tokens: {0}")]
    CannotReadRevokedTokens(generate::Error),
    #[error("Cannot trash outdated websites: {0}")]
    CannotTrashOutdatedWebsites(generate::Error),
    #[error("Cannot recover trash: {0}")]
    CannotRecoverTrash(generate::Error),
    #[error("Cannot empty trash: {0}")]
    CannotEmptyTrash(generate::Error),
}
