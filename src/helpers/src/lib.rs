pub mod config;
pub mod generate;
pub mod readers;
pub mod website_id;

use std::{
    collections::HashSet,
    fs::{self, File},
    io,
    ops::Deref,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use lazy_static::lazy_static;
use serde::{de::DeserializeOwned, Deserialize};
use serde_with::{serde_as, DefaultOnNull};
use tracing::{debug, error, trace};

use crate::config::*;

lazy_static! {
    static ref USED_PROFILES: Arc<RwLock<Option<&'static HashSet<String>>>> = Arc::default();
}

pub fn used_profiles<'a>() -> &'a HashSet<String> {
    if let Some(profiles) = *USED_PROFILES.read().unwrap() {
        trace!("Read used profiles from cache");
        return profiles;
    }

    debug!("Reading all used profiles…");
    let acc: &'static mut HashSet<String> = Box::leak(Box::new(HashSet::new()));

    for data_file in find_data_files() {
        // trace!("Reading <{}>…", data_file.display());

        let metadata: PageMetadata = match deser(&data_file) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => {
                error!(
                    "Could not read page metadata at <{}>: File not found",
                    data_file.display(),
                );
                continue;
            },
            Err(err) => {
                error!(
                    "Could not read page metadata at <{}>: {err}",
                    data_file.display(),
                );
                continue;
            },
        };
        let read_allowed = metadata.read_allowed;
        // trace!("  read_allowed: {:?}", read_allowed);

        // Store new profiles
        read_allowed.into_iter().for_each(|p| {
            acc.insert(p.to_owned());
        });
    }

    *USED_PROFILES.write().unwrap() = Some(acc);

    acc
}

pub fn find(
    dir: &PathBuf,
    extensions: &Vec<&str>,
    files: &mut Vec<PathBuf>,
) {
    for entry in fs::read_dir(dir).unwrap() {
        if let Ok(entry) = entry {
            let path = entry.path();
            if path.is_file() {
                if path
                    .extension()
                    .map(|ext| extensions.contains(&ext.to_str().unwrap()))
                    .unwrap_or(false)
                {
                    files.push(path);
                }
            } else if path.is_dir() {
                find(&path, extensions, files);
            }
        }
    }
}

pub fn data_file_path(page_relpath: &PathBuf) -> PathBuf {
    let mut data_file_relpath = page_relpath.with_extension(DATA_FILE_EXTENSION);
    data_file_relpath = match data_file_relpath.strip_prefix("/") {
        Ok(trimmed) => trimmed.to_path_buf(),
        Err(_) => data_file_relpath,
    };
    WEBSITE_DATA_DIR.join(data_file_relpath)
}

fn find_data_files() -> Vec<PathBuf> {
    let mut data_files: Vec<PathBuf> = Vec::new();
    find(
        &WEBSITE_DATA_DIR,
        &vec![DATA_FILE_EXTENSION],
        &mut data_files,
    );
    data_files
}

#[serde_as]
#[derive(Deserialize)]
pub struct PageMetadata {
    // NOTE: Hugo taxonomy term pages contain `read_allowed": null`.
    // TODO: Investigate and fix this, to remove the `serde` default which could be in conflict with the user's preference.
    #[serde(default)]
    #[serde_as(deserialize_as = "DefaultOnNull")]
    pub read_allowed: ReadAllowed,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadAllowed(Vec<String>);

impl Deref for ReadAllowed {
    type Target = Vec<String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for ReadAllowed {
    fn default() -> Self {
        Self(vec![DEFAULT_PROFILE.to_owned()])
    }
}

impl IntoIterator for ReadAllowed {
    type Item = <<Self as Deref>::Target as IntoIterator>::Item;
    type IntoIter = <<Self as Deref>::Target as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

fn deser<T: DeserializeOwned>(file_path: &PathBuf) -> Result<Option<T>, serde_json::Error> {
    let Ok(file) = File::open(file_path) else {
        return Ok(None);
    };
    let res: T = serde_json::from_reader(file)?;
    Ok(Some(res))
}

fn deser_first_match<T: DeserializeOwned>(
    file_paths: Vec<PathBuf>
) -> Result<Option<T>, serde_json::Error> {
    for file_path in file_paths.iter() {
        trace!("Trying {}…", file_path.display());
        match deser(file_path)? {
            Some(res) => {
                // TODO: Check if this index is always the same.
                // debug!("Found page metadata in match #{i}");
                return Ok(Some(res));
            },
            None => continue,
        }
    }
    Ok(None)
}

// `Ok(None)` if file not found.
// `Err(_)` if file found but deserialization error.
// `Ok(Some(_))` if file found.
pub fn page_metadata(page_relpath: &PathBuf) -> Result<Option<PageMetadata>, serde_json::Error> {
    let mut file_paths = vec![
        data_file_path(page_relpath),
        data_file_path(&page_relpath.join("index.html")),
    ];
    // Don't try parsing the exact path if it points to a directory.
    if page_relpath.is_dir() {
        file_paths.remove(0);
    }
    deser_first_match(file_paths)
}

pub fn copy_directory(
    src: &std::path::Path,
    dest: &std::path::Path,
) -> io::Result<()> {
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
