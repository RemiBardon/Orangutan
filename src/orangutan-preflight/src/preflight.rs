#![feature(exit_status_error)]

mod config;
mod helpers;
mod keys_reader;

use crate::config::*;
use crate::helpers::*;
use core::fmt;
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, exit, ExitStatusError};
use tracing_subscriber::FmtSubscriber;
use tracing::{Level, debug, error};
#[macro_use]
extern crate lazy_static;

lazy_static! {
    static ref FILES: Vec<PathBuf> = Vec::new();
}

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

fn throwing_main() -> Result<(), Error> {
    // Copy Some files if needed
    // FIXME: Do not hardcode "PaperMod"
    let shortcodes_dir = Path::new("themes/PaperMod/layouts/shortcodes");
    let shortcodes_dest_dir_path = format!("themes/{}/layouts/shortcodes", THEME_NAME);
    let shortcodes_dest_dir = Path::new(&shortcodes_dest_dir_path);
    copy_directory(shortcodes_dir, shortcodes_dest_dir).unwrap();

    // Generate the website
    generate_website(&WEBSITE_DIR)?;
    append_profile_to_website_files(&WEBSITE_DIR, &DEFAULT_PROFILE);

    // Generate Orangutan data files
    hugo(
        vec!["--disableKinds", "RSS,sitemap,home", "--theme", THEME_NAME],
        WEBSITE_DATA_DIR.display().to_string()
    )
    .map_err(|e| Error::CannotGenerateDataFiles(Box::new(e)))?;

    // Read all profiles just for debug purposes
    let all_profiles = all_profiles();
    debug!("All profiles found: {:?}", all_profiles);

    Ok(())
}

/// Generate the website
fn generate_website(destination: &PathBuf) -> Result<(), Error> {
    let mut params = vec!["--disableKinds", "RSS,sitemap", "--cleanDestinationDir"];
    if env::var("LOCALHOST") == Ok("true".to_string()) {
        params.append(&mut vec!["--baseURL", "http://localhost:8080"]);
    }
    hugo(params, destination.display().to_string())
        .map_err(|e| Error::CannotGenerateWebsite(Box::new(e)))?;

    // Temporary fix to avoid leakage of page existence and content
    // TODO(RemiBardon): Find a solution to avoid removing this file
    empty_index_json(destination).map_err(Error::CannotEmptyIndexJson)?;

    Ok(())
}

fn append_profile_to_website_files(destination: &PathBuf, profile: &str) {
    // Find files
    let mut files: Vec<PathBuf> = Vec::new();
    find(destination, &SUFFIXED_EXTENSIONS, &mut files);

    // Rename files
    for file_path in files {
        let new_path = format!("{}@{}", file_path.display(), profile);
        if let Err(e) = fs::rename(&file_path, &new_path) {
            error!("Could not rename file <{}> to <{}>: {e}", &file_path.display(), &new_path);
        }
    }
}

fn hugo(params: Vec<&str>, destination: String) -> Result<(), Error> {
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

// fn find_remaining_files() -> Vec<PathBuf> {
//     let mut files: Vec<PathBuf> = Vec::new();
//     find(&WEBSITE_DIR, &vec!["html", "json", "xml", "css", "js", "txt"], &mut files);
//     files
// }

fn copy_directory(src: &std::path::Path, dest: &std::path::Path) -> io::Result<()> {
    if src.is_file() {
        // If the source is a file, copy it to the destination
        fs::copy(src, dest)?;
    } else if src.is_dir() {
        // If the source is a directory, create a corresponding directory in the destination
        fs::create_dir_all(dest)?;

        // List the entries in the source directory
        let entries = fs::read_dir(src)?;

        for entry in entries {
            let entry = entry?;
            let entry_dest = dest.join(entry.file_name());

            // Recursively copy each entry in the source directory to the destination directory
            copy_directory(&entry.path(), &entry_dest)?;
        }
    }

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
enum Error {
    CannotExecuteCommand(io::Error),
    CommandExecutionFailed(ExitStatusError),
    CannotGenerateWebsite(Box<Error>),
    CannotGenerateDataFiles(Box<Error>),
    CannotEmptyIndexJson(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CannotExecuteCommand(err) => write!(f, "Could not execute command: {err}"),
            Error::CommandExecutionFailed(err) => write!(f, "Command failed: {err}"),
            Error::CannotGenerateWebsite(err) => write!(f, "Could not generate website: {err}"),
            Error::CannotGenerateDataFiles(err) => write!(f, "Could not generate data files: {err}"),
            Error::CannotEmptyIndexJson(err) => write!(f, "Could not empty <index.json> file: {err}"),
        }
    }
}
