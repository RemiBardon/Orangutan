mod config;
mod generate;
mod helpers;
mod keys_reader;

use core::fmt;
use std::process::exit;

use tracing::{debug, error, Level};
use tracing_subscriber::FmtSubscriber;

use crate::config::*;
use crate::generate::*;
use crate::helpers::*;

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

#[derive(Debug)]
pub enum Error {
    WebsiteGenerationError(generate::Error),
    CannotGenerateDataFiles(generate::Error),
}

impl fmt::Display for Error {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Error::WebsiteGenerationError(err) => write!(f, "Website generation error: {err}"),
            Error::CannotGenerateDataFiles(err) => {
                write!(f, "Could not generate data files: {err}")
            },
        }
    }
}
