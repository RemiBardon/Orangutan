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
use std::process::{Command, exit};
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

    // Generate the website
    hugo(vec!["--disableKinds", "RSS,sitemap", "--cleanDestinationDir", "--baseURL", "http://localhost:8080"]).map_err(Error::IO)?;
    // Generate Orangutan data files
    hugo(vec!["--disableKinds", "RSS,sitemap,home", "--theme", THEME_NAME]).map_err(Error::IO)?;

    // Temporary fix to avoid leakage of page existence and content
    // TODO(RemiBardon): Find a solution to avoid removing this file
    empty_index_json().map_err(Error::IO)?;

    // Encrypt all files with read profiles explicitly defined, then delete the originals
    for data_file in find_data_files() {
        debug!("{}", data_file.display());

        let read_access_profiles = read_access_profiles(&data_file);
        fs::remove_file(&data_file).map_err(Error::IO)?;

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

            fs::remove_file(html_file).map_err(Error::IO)?;
        }
    }

    // Encrypt all remaining files with default profile, then delete the originals
    for file in find_remaining_files() {
        encrypt_file(&file, DEFAULT_PROFILE)?;
        fs::remove_file(file).map_err(Error::IO)?;
    }

    Ok(())
}

fn hugo(params: Vec<&str>) -> Result<std::process::ExitStatus, io::Error> {
    let destination = DEST_DIR.to_str().unwrap().to_owned();
    let base_params: Vec<&str> = vec!["--destination", destination.as_str()];
    Command::new("hugo")
        .args(base_params.iter().chain(params.iter()))
        .status()
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
        let mut file = File::open(in_file).map_err(Error::IO)?;
        file.read_to_string(&mut plaintext).map_err(Error::IO)?;
    }

    // Encrypt the text
    let keys_reader = <dyn KeysReader>::detect();
    let key = keys_reader.get_key(key_name)?;
    let cipher = Aes256Gcm::new(&key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher.encrypt(&nonce, plaintext.as_ref()).map_err(Error::AES)?;

    // Save the encrypted text to a file
    {
        let mut file = File::create(&out_file).map_err(Error::IO)?;
        file.write_all(&nonce).map_err(Error::IO)?;
        file.write_all(&ciphertext).map_err(Error::IO)?;
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
            let mut file = File::open(key_file).map_err(Error::IO)?;

            let mut buf: Vec<u8> = Vec::new();
            file.read_to_end(&mut buf).map_err(Error::IO)?;
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

            let mut file = File::create(&key_file).map_err(Error::IO)?;
            file.write_all(&buf).map_err(Error::IO)?;
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
    IO(io::Error),
    Env(env::VarError),
    AES(aes_gcm::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::IO(err) => err.fmt(f),
            Error::Env(err) => err.fmt(f),
            Error::AES(err) => err.fmt(f),
        }
    }
}
