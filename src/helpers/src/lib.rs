pub mod config;
pub mod generate;
pub mod readers;
pub mod website_id;

use std::{
    collections::HashSet,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use lazy_static::lazy_static;
use serde_json::Value;
use tracing::trace;

use crate::config::*;

lazy_static! {
    static ref USED_PROFILES: Arc<Mutex<Option<&'static HashSet<String>>>> =
        Arc::new(Mutex::new(None));
}

pub fn used_profiles<'a>() -> &'a HashSet<String> {
    let mut used_profiles = USED_PROFILES.lock().unwrap();
    if let Some(profiles) = used_profiles.clone() {
        trace!("Read used profiles from cache");
        return profiles;
    }

    trace!("Reading used profiles…");
    let acc: &'static mut HashSet<String> = Box::leak(Box::new(HashSet::new()));

    for data_file in find_data_files() {
        // trace!("Reading <{}>…", data_file.display());

        // Make sure this generator isn't broken (could be replaced by unit tests)
        // let html_file = html_file(&data_file).unwrap();
        // trace!("{}", html_file.display());

        let read_allowed = read_allowed(&data_file).unwrap();
        // trace!("  read_allowed: {:?}", read_allowed);

        // Store new profiles
        read_allowed.iter().for_each(|p| {
            acc.insert(p.clone());
        });
    }

    *used_profiles = Some(acc);

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

fn _html_file(data_file: &PathBuf) -> Option<Option<PathBuf>> {
    let file = File::open(data_file).ok()?;
    let reader = io::BufReader::new(file);
    let data: Value = match serde_json::from_reader(reader) {
        Ok(data) => data,
        Err(_) => return None,
    };

    let path = data.get(PATH_FIELD)?;

    Some(serde_json::from_value(path.to_owned()).ok())
}

// fn html_file(data_file: &PathBuf) -> Result<PathBuf, ()> {
//     match _html_file(data_file) {
//         Some(Some(path)) => Ok(path),
//         Some(None) => {
//             error!("Path not defined");
//             Err(())
//         },
//         None => {
//             error!("File not found");
//             Err(())
//         },
//     }
// }

pub fn data_file(html_file: &PathBuf) -> PathBuf {
    let mut data_file_rel = html_file
        .strip_prefix(WEBSITE_DATA_DIR.to_path_buf())
        .unwrap_or(html_file)
        .with_extension(DATA_FILE_EXTENSION);
    data_file_rel = match data_file_rel.strip_prefix("/") {
        Ok(trimmed) => trimmed.to_path_buf(),
        Err(_) => data_file_rel,
    };
    WEBSITE_DATA_DIR.join(data_file_rel)
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

fn _read_allowed(data_file: &PathBuf) -> Option<Option<Vec<String>>> {
    let file = File::open(data_file).ok()?;
    let reader = io::BufReader::new(file);
    let data: Value = match serde_json::from_reader(reader) {
        Ok(data) => data,
        Err(_) => return None,
    };

    let read_allowed = data.get(READ_ALLOWED_FIELD)?;

    Some(serde_json::from_value(read_allowed.to_owned()).ok())
}

// `None` if file not found
pub fn read_allowed(data_file: &PathBuf) -> Option<Vec<String>> {
    _read_allowed(data_file).map(|o| o.unwrap_or(vec![DEFAULT_PROFILE.to_string()]))
}

pub fn object_key<P: AsRef<Path>>(
    path: &P,
    profile: &str,
) -> String {
    let path = path.as_ref();
    if let Some(ext) = path.extension() {
        if SUFFIXED_EXTENSIONS.contains(&ext.to_str().unwrap()) {
            return format!("{}@{}", path.display(), profile);
        }
    }
    path.display().to_string()
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
