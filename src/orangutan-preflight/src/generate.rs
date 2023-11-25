use crate::config::*;
use crate::helpers::copy_directory;
use core::fmt;
use std::sync::{Mutex, MutexGuard, Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use lazy_static;
use std::collections::HashSet;
use std::env;
use std::fs::File;
use std::io::{self, Write};
use std::path::{PathBuf, Path};
use std::process::{Command, ExitStatusError};
use tracing::{info, debug};

static DATA_FILES_GENERATED: AtomicBool = AtomicBool::new(false);

lazy_static! {
    // NOTE: `Arc` prevents race conditions
    static ref GENERATED_WEBSITES: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
}

fn generate_website(
    id: &WebsiteId,
    destination: &PathBuf,
    generated_websites: &mut MutexGuard<'_, HashSet<PathBuf>>
) -> Result<(), Error> {
    info!("Generating website for {:?}…", id.profiles);
    debug!("Website for {:?} will be generated at <{}>", id.profiles, destination.display());

    let mut params = vec!["--disableKinds", "RSS,sitemap", "--cleanDestinationDir"];
    if env::var("LOCALHOST") == Ok("true".to_string()) {
        params.append(&mut vec!["--baseURL", "http://localhost:8080"]);
    }
    hugo(params, destination.display().to_string())
        .map_err(|e| Error::CannotGenerateWebsite(Box::new(e)))?;

    // Temporary fix to avoid leakage of page existence and content
    // TODO(RemiBardon): Find a solution to avoid removing this file
    empty_index_json(destination).map_err(Error::CannotEmptyIndexJson)?;

    generated_websites.insert(destination.clone());

    Ok(())
}

/// Generate the website
pub fn generate_website_if_needed(website_id: &WebsiteId) -> Result<PathBuf, Error> {
    let website_dir = website_dir(&website_id);

    let mut generated_websites = GENERATED_WEBSITES.lock().unwrap();
    if !generated_websites.contains(&website_dir) {
        generate_website(&website_id, &website_dir, &mut generated_websites)?;
    }

    Ok(website_dir)
}

fn _generate_data_files() -> Result<(), Error> {
    info!("Generating Orangutan data files…");

    // Copy some files if needed
    // FIXME: Do not hardcode "PaperMod"
    let shortcodes_dir = Path::new("themes/PaperMod/layouts/shortcodes");
    let shortcodes_dest_dir_path = format!("themes/{}/layouts/shortcodes", THEME_NAME);
    let shortcodes_dest_dir = Path::new(&shortcodes_dest_dir_path);
    copy_directory(shortcodes_dir, shortcodes_dest_dir).unwrap();

    let res = hugo(
        vec!["--disableKinds", "RSS,sitemap,home", "--theme", THEME_NAME],
        WEBSITE_DATA_DIR.display().to_string()
    )?;

    DATA_FILES_GENERATED.store(true, Ordering::Relaxed);

    Ok(res)
}

pub fn generate_data_files_if_needed() -> Result<(), Error> {
    if DATA_FILES_GENERATED.load(Ordering::Relaxed) {
        Ok(())
    } else {
        _generate_data_files()
    }
}

pub fn hugo(params: Vec<&str>, destination: String) -> Result<(), Error> {
    let base_params: Vec<&str> = vec!["--destination", destination.as_str()];
    let status = Command::new("hugo")
        .args(base_params.iter().chain(params.iter()))
        .status()
        .map_err(Error::CannotExecuteCommand)?;

    status.exit_ok()
        .map_err(Error::CommandExecutionFailed)
}

fn empty_index_json(website_dir: &PathBuf) -> Result<(), io::Error> {
    let index_json_path = website_dir.join("index.json");
    // Open the file in write mode, which will truncate the file if it already exists
    let mut file = File::create(index_json_path)?;
    file.write(b"[]")?;
    Ok(())
}

#[derive(Debug)]
pub enum Error {
    CannotExecuteCommand(io::Error),
    CommandExecutionFailed(ExitStatusError),
    CannotGenerateWebsite(Box<Error>),
    CannotEmptyIndexJson(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CannotExecuteCommand(err) => write!(f, "Could not execute command: {err}"),
            Error::CommandExecutionFailed(err) => write!(f, "Command failed: {err}"),
            Error::CannotGenerateWebsite(err) => write!(f, "Could not generate website: {err}"),
            Error::CannotEmptyIndexJson(err) => write!(f, "Could not empty <index.json> file: {err}"),
        }
    }
}
