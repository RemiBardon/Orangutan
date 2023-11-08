#![feature(exit_status_error)]

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key};
use base64::{Engine as _, engine::general_purpose};
use core::fmt;
use std::collections::HashMap;
use serde_json::{self, Value};
use std::env;
use std::fs::{self, File};
use std::io::{self, Write, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, exit, ExitStatusError};
use std::sync::Mutex;
use tracing_subscriber::FmtSubscriber;
use tracing::{Level, debug, error, trace, warn};
#[macro_use]
extern crate lazy_static;

const THEME_NAME: &str = "Orangutan";
const DATA_FILE_EXTENSION: &str = "orangutan";
const READ_ACCESS_FIELD: &str = "read_access";
const DEFAULT_PROFILE: &str = "_default";

lazy_static! {
    static ref BASE_DIR: &'static Path = Path::new(".orangutan");
    static ref DEST_DIR: PathBuf = BASE_DIR.join("website");
    static ref KEYS_DIR: PathBuf = BASE_DIR.join("keys");
    static ref KEYS: Mutex<HashMap<String, Key<Aes256Gcm>>> = Mutex::new(HashMap::new());
    static ref FILES: Vec<PathBuf> = Vec::new();

    static ref KEYS_MODE: Result<String, env::VarError> = env::var("KEYS_MODE");
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
    // Create necessary directories
    create_directory(&DEST_DIR);
    create_directory(&KEYS_DIR);

    // Copy Some files if needed
    // FIXME: Do not hardcode "PaperMod"
    let shortcodes_dir = Path::new("themes/PaperMod/layouts/shortcodes");
    let shortcodes_dest_dir_path = format!("themes/{}/layouts/shortcodes", THEME_NAME);
    let shortcodes_dest_dir = Path::new(&shortcodes_dest_dir_path);
    copy_directory(shortcodes_dir, shortcodes_dest_dir).unwrap();

    // Generate the website
    let mut params = vec!["--disableKinds", "RSS,sitemap", "--cleanDestinationDir"];
    if env::var("LOCALHOST") == Ok("true".to_string()) {
        params.append(&mut vec!["--baseURL", "http://localhost:8080"]);
    }
    hugo(params)
        .map_err(|e| Error::CannotGenerateWebsite(Box::new(e)))?;
    // Generate Orangutan data files
    hugo(vec!["--disableKinds", "RSS,sitemap,home", "--theme", THEME_NAME])
        .map_err(|e| Error::CannotGenerateDataFiles(Box::new(e)))?;

    // Temporary fix to avoid leakage of page existence and content
    // TODO(RemiBardon): Find a solution to avoid removing this file
    empty_index_json().map_err(Error::CannotEmptyIndexJson)?;

    // Encrypt all files with read profiles explicitly defined, then delete the originals
    for data_file in find_data_files() {
        debug!("{}", data_file.display());

        let read_access_profiles = read_access_profiles(&data_file);
        fs::remove_file(&data_file).map_err(|e| Error::CannotDeleteFile(data_file.clone(), e))?;

        if let Some(read_profiles) = read_access_profiles {
            debug!("  read_profiles: {:?}", read_profiles);

            let html_file = data_file.with_extension("html");
            if !html_file.exists() {
                warn!("  HTML file <{}> doesn't exist", html_file.display());
                continue;
            }

            for profile in read_profiles {
                encrypt_file(&html_file, &profile)?;
            }

            fs::remove_file(&html_file).map_err(|e| Error::CannotDeleteFile(html_file.clone(), e))?;
        }
    }

    // Encrypt all remaining files with default profile, then delete the originals
    for file in find_remaining_files() {
        encrypt_file(&file, DEFAULT_PROFILE)?;
        fs::remove_file(&file).map_err(|e| Error::CannotDeleteFile(file.clone(), e))?;
    }

    Ok(())
}

fn hugo(params: Vec<&str>) -> Result<(), Error> {
    let destination = DEST_DIR.to_str().unwrap().to_owned();
    let base_params: Vec<&str> = vec!["--destination", destination.as_str()];
    let status = Command::new("hugo")
        .args(base_params.iter().chain(params.iter()))
        .status()
        .map_err(Error::CannotExecuteCommand)?;

    status.exit_ok()
        .map_err(Error::CommandExecutionFailed)
}

fn empty_index_json() -> Result<(), io::Error> {
    let index_json_path = DEST_DIR.join("index.json");
    if !index_json_path.exists() {
        let mut file = File::create(index_json_path)?;
        file.write(b"[]")?;
    }
    Ok(())
}

fn find_data_files() -> Vec<PathBuf> {
    let mut data_files: Vec<PathBuf> = Vec::new();
    find(&DEST_DIR, &vec![DATA_FILE_EXTENSION], &mut data_files);
    data_files
}

fn find_remaining_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    find(&DEST_DIR, &vec!["html", "json", "xml", "css", "js", "txt"], &mut files);
    files
}

fn find(dir: &PathBuf, extensions: &Vec<&str>, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        if let Ok(entry) = entry {
            let path = entry.path();
            if path.is_file() {
                if path.extension().map(|ext| extensions.contains(&ext.to_str().unwrap())).unwrap_or(false) {
                    files.push(path);
                }
            } else if path.is_dir() {
                find(&path, extensions, files);
            }
        }
    }
}

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

fn read_access_profiles(data_file: &PathBuf) -> Option<Vec<String>> {
    let file = File::open(data_file).ok()?;
    let reader = io::BufReader::new(file);
    let data: Value = match serde_json::from_reader(reader) {
        Ok(data) => data,
        Err(_) => return None,
    };

    let read_access = data.get(READ_ACCESS_FIELD)?;

    let result: Result<Vec<String>, serde_json::Error> = serde_json::from_value(read_access.to_owned());
    match result {
        Ok(profiles) => Some(profiles),
        Err(_) => None,
    }
}

fn encrypt_file(in_file: &PathBuf, key_name: &str) -> Result<(), Error> {
    let out_file = in_file.with_file_name(format!("{}@{}", in_file.file_name().unwrap().to_str().unwrap(), key_name));

    trace!("Encrypting <{}> into <{}>…", in_file.display(), out_file.display());

    // Read file contents
    let mut plaintext = String::new();
    {
        let mut file = File::open(in_file).map_err(|e| Error::CannotOpenFile(in_file.clone(), e))?;
        file.read_to_string(&mut plaintext).map_err(|e| Error::CannotReadFile(in_file.clone(), e))?;
    }

    // Encrypt the text
    let keys_reader = <dyn KeysReader>::detect();
    let key = keys_reader.get_key(key_name)
        .or(keys_reader.get_key(DEFAULT_PROFILE))?;
    let cipher = Aes256Gcm::new(&key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher.encrypt(&nonce, plaintext.as_ref()).map_err(Error::AES)?;

    // Save the encrypted text to a file
    {
        let mut file = File::create(&out_file).map_err(|e| Error::CannotCreateFile(out_file.clone(), e))?;
        file.write_all(&nonce).map_err(|e| Error::CannotWriteInFile(out_file.clone(), e))?;
        file.write_all(&ciphertext).map_err(|e| Error::CannotWriteInFile(out_file.clone(), e))?;
    }

    Ok(())
}

trait KeysReader {
    fn get_key(&self, key_name: &str) -> Result<Key<Aes256Gcm>, Error>;
}

impl dyn KeysReader {
    fn detect() -> Box<dyn KeysReader> {
        match KEYS_MODE.clone().unwrap_or("".to_string()).as_str() {
            "LOCAL" => Box::new(LocalKeysReader {}),
            "ENV" | _ => Box::new(EnvKeysReader {}),
        }
    }
}

struct EnvKeysReader {}

impl KeysReader for EnvKeysReader {
    fn get_key(&self, key_name: &str) -> Result<Key<Aes256Gcm>, Error> {
        let mut keys = KEYS.lock().unwrap();
        if let Some(key) = keys.get(key_name) {
            trace!("Read key '{}' from cache", key_name);
            return Ok(*key);
        }

        let env_var_name = format!("KEY_{}", key_name);
        trace!("Reading key '{}' from environment ({})…", key_name, env_var_name);
        env::var(env_var_name)
            .map_err(Error::Env)
            .and_then(|key_base64| {
                // FIXME: Handle error
                let key = general_purpose::STANDARD.decode(key_base64).unwrap();

                let key_bytes: [u8; 32] = key.as_slice().try_into().unwrap();
                let key: Key<Aes256Gcm> = key_bytes.into();
                keys.insert(key_name.to_string(), key);
                Ok(key)
            })
    }
}

struct LocalKeysReader {}

impl LocalKeysReader {
    fn key_file(&self, key_name: &str) -> PathBuf {
        KEYS_DIR.join(format!("{}.key", key_name))
    }
}

impl KeysReader for LocalKeysReader {
    fn get_key(&self, key_name: &str) -> Result<Key<Aes256Gcm>, Error> {
        let mut keys = KEYS.lock().unwrap();
        if let Some(key) = keys.get(key_name) {
            trace!("Read key '{}' from cache", key_name);
            return Ok(*key);
        }

        let key_file = self.key_file(key_name);

        if key_file.exists() {
            // If key file exists, read the file
            trace!("Reading key '{}' from <{}>…", key_name, key_file.display());
            let mut file = File::open(&key_file).map_err(|e| Error::CannotOpenFile(key_file.clone(), e))?;

            let mut buf: Vec<u8> = Vec::new();
            file.read_to_end(&mut buf).map_err(|e| Error::CannotReadFile(key_file.clone(), e))?;
            // FIXME: Handle error
            let key = general_purpose::STANDARD.decode(buf).unwrap();
            // FIXME: Handle error
            let key_bytes: [u8; 32] = key.as_slice().try_into().unwrap();

            let key: Key<Aes256Gcm> = key_bytes.into();
            keys.insert(key_name.to_string(), key);
            Ok(key)
        } else {
            // If key file does not exist, create a new key and save it to a new file
            trace!("Saving new key '{}' into <{}>…", key_name, key_file.display());
            let key = Aes256Gcm::generate_key(OsRng);

            // Encode key as base64
            let mut buf: Vec<u8> = Vec::new();
            // Make sure we'll have a slice big enough for base64 + padding
            buf.resize(key.len() * 4 / 3 + 4, 0);
            let bytes_written = general_purpose::STANDARD.encode_slice(key, &mut buf).unwrap();
            // shorten our vec down to just what was written
            buf.truncate(bytes_written);

            let mut file = File::create(&key_file).map_err(|e| Error::CannotCreateFile(key_file.clone(), e))?;
            file.write_all(&buf).map_err(|e| Error::CannotWriteInFile(key_file.clone(), e))?;
            keys.insert(key_name.to_string(), key);
            Ok(key)
        }
    }
}

fn create_directory(directory: &Path) {
    if !directory.exists() {
        if let Err(err) = fs::create_dir_all(directory) {
            error!("Error creating directory {}: {}", directory.display(), err);
            exit(1);
        }
    }
}

#[derive(Debug)]
enum Error {
    Env(env::VarError),
    AES(aes_gcm::Error),
    CannotExecuteCommand(io::Error),
    CommandExecutionFailed(ExitStatusError),
    CannotGenerateWebsite(Box<Error>),
    CannotGenerateDataFiles(Box<Error>),
    CannotEmptyIndexJson(io::Error),
    CannotCreateFile(PathBuf, io::Error),
    CannotOpenFile(PathBuf, io::Error),
    CannotWriteInFile(PathBuf, io::Error),
    CannotReadFile(PathBuf, io::Error),
    CannotDeleteFile(PathBuf, io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Env(err) => err.fmt(f),
            Error::AES(err) => err.fmt(f),
            Error::CannotExecuteCommand(err) => write!(f, "Could not execute command: {err}"),
            Error::CommandExecutionFailed(err) => write!(f, "Command failed: {err}"),
            Error::CannotGenerateWebsite(err) => write!(f, "Could not generate website: {err}"),
            Error::CannotGenerateDataFiles(err) => write!(f, "Could not generate data files: {err}"),
            Error::CannotEmptyIndexJson(err) => write!(f, "Could not empty <index.json> file: {err}"),
            Error::CannotCreateFile(path, err) => write!(f, "Could not create <{}> file: {err}", path.display()),
            Error::CannotOpenFile(path, err) => write!(f, "Could not open <{}> file: {err}", path.display()),
            Error::CannotWriteInFile(path, err) => write!(f, "Could not write in <{}> file: {err}", path.display()),
            Error::CannotReadFile(path, err) => write!(f, "Could not read <{}> file: {err}", path.display()),
            Error::CannotDeleteFile(path, err) => write!(f, "Could not delete <{}> file: {err}", path.display()),
        }
    }
}
