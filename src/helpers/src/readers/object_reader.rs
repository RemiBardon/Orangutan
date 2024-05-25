use std::path::PathBuf;
use std::{fmt, fs, io};

use rocket::fs::NamedFile;
use rocket::response::Responder;
use tracing::trace;

use crate::generate::{self, generate_website_if_needed};
use crate::website_id::WebsiteId;

pub type ObjectReader = LocalObjectReader;

impl ObjectReader {
    pub fn new() -> Self {
        LocalObjectReader {}
    }
}

pub type ReadObjectResponse = LocalReadObjectResponse;

pub struct LocalObjectReader;

#[derive(Responder)]
pub enum LocalReadObjectResponse {
    #[response(status = 404)]
    NotFound(String),
    Found(Result<NamedFile, std::io::Error>),
}

impl LocalObjectReader {
    async fn serve_file(file_path: PathBuf) -> io::Result<NamedFile> {
        NamedFile::open(file_path).await
    }
}

impl LocalObjectReader {
    pub fn list_objects(
        &self,
        prefix: &str,
        website_id: &WebsiteId,
    ) -> Result<Vec<String>, Error> {
        trace!("Listing files with prefix '{}' for {}…", prefix, website_id);

        let website_dir =
            generate_website_if_needed(website_id).map_err(Error::WebsiteGenerationError)?;

        Ok(find_all_files(&website_dir)
            .iter()
            .map(|path| {
                format!(
                    "/{}",
                    path.strip_prefix(website_dir.as_path())
                        .expect("Could not remove prefix")
                        .display()
                )
            })
            .collect())
    }

    pub async fn read_object<'r>(
        &self,
        object_key: &str,
        website_id: &WebsiteId,
    ) -> LocalReadObjectResponse {
        let website_dir = match generate_website_if_needed(website_id) {
            Ok(dir) => dir,
            Err(err) => return LocalReadObjectResponse::NotFound(err.to_string()),
        };
        let file_path = website_dir.join(object_key.strip_prefix("/").unwrap());
        trace!(
            "Reading '{}' from disk at <{}>…",
            object_key,
            file_path.display()
        );

        LocalReadObjectResponse::Found(Self::serve_file(file_path).await)
    }
}

fn find_all_files(dir: &PathBuf) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    find(dir, &mut files);
    files
}

fn find(
    dir: &PathBuf,
    files: &mut Vec<PathBuf>,
) {
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
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Error::WebsiteGenerationError(err) => write!(f, "Website generation error: {err}"),
        }
    }
}
