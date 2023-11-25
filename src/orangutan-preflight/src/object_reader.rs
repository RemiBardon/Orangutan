use std::{io::Read, path::PathBuf, fs::{File, self}, fmt};

use crate::config::WebsiteId;
use crate::generate::{self, generate_website_if_needed};
use tracing::{debug, trace};

pub trait ObjectReader {
    fn list_objects(&self, prefix: &str, website_id: &WebsiteId) -> Result<Vec<String>, Error>;
    // TODO: Change result type to Result<Vec<u8>, Error>
    fn read_object(&self, object_key: &str, website_id: &WebsiteId) -> Option<Vec<u8>>;
}

impl dyn ObjectReader {
    pub fn detect() -> Box<dyn ObjectReader> {
        Box::new(LocalObjectReader {})
    }
}
struct LocalObjectReader {}

impl LocalObjectReader {
    fn serve_file(file_path: PathBuf) -> Option<Vec<u8>> {
        if let Ok(mut file) = File::open(&file_path) {
            let mut data: Vec<u8> = Vec::new();
            if let Err(err) = file.read_to_end(&mut data) {
                debug!("Could not read <{}> from disk: {}", file_path.display(), err);
                return None
            }
            Some(data)
        } else {
            debug!("Could not read <{}> from disk: Cannot open file", file_path.display());
            None
        }
    }
}

impl ObjectReader for LocalObjectReader {
    fn list_objects(&self, prefix: &str, website_id: &WebsiteId) -> Result<Vec<String>, Error> {
        trace!("Listing files with prefix '{}' for {}…", prefix, website_id);

        let website_dir = generate_website_if_needed(website_id)
            .map_err(Error::WebsiteGenerationError)?;

        Ok(find_all_files(&website_dir).iter()
            .map(|path|
                format!("/{}", path
                    .strip_prefix(website_dir.as_path())
                    .expect("Could not remove prefix")
                    .display())
            )
            .collect())
    }

    fn read_object(&self, object_key: &str, website_id: &WebsiteId) -> Option<Vec<u8>> {
        let Ok(website_dir) = generate_website_if_needed(website_id) else {
            return None
        };
        let file_path = website_dir.join(object_key.strip_prefix("/").unwrap());
        trace!("Reading '{}' from disk at <{}>…", object_key, file_path.display());

        Self::serve_file(file_path)
    }
}

fn find_all_files(dir: &PathBuf) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    find(dir, &mut files);
    files
}

fn find(dir: &PathBuf, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        if let Ok(entry) = entry {
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            } else if path.is_dir() {
                find(&path, files);
            }
        }
    }
}

#[derive(Debug)]
pub enum Error {
    WebsiteGenerationError(generate::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::WebsiteGenerationError(err) => write!(f, "Website generation error: {err}"),
        }
    }
}

