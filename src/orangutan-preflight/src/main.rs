use std::process::exit;

use orangutan_helpers::{
    generate::{self, *},
    used_profiles,
    website_id::WebsiteId,
};
use tracing::{debug, error, Level};
use tracing_subscriber::FmtSubscriber;

fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber.");

    if let Err(err) = throwing_main() {
        error!("Error: {}", err);
        exit(1);
    }
}

pub fn throwing_main() -> Result<(), Error> {
    // Generate the website
    generate_website_if_needed(&WebsiteId::default()).map_err(Error::WebsiteGenerationError)?;

    // Generate Orangutan data files
    generate_data_files_if_needed().map_err(Error::CannotGenerateDataFiles)?;

    // Read all profiles just for debug purposes
    let used_profiles = used_profiles();
    debug!("All profiles found: {:?}", used_profiles);

    Ok(())
}

// fn create_directory(directory: &Path) {
//     if !directory.exists() {
//         if let Err(err) = fs::create_dir_all(directory) {
//             error!("Error creating directory {}: {}", directory.display(), err);
//             exit(1);
//         }
//     }
// }

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Website generation error: {0}")]
    WebsiteGenerationError(generate::Error),
    #[error("Could not generate data files: {0}")]
    CannotGenerateDataFiles(generate::Error),
}
