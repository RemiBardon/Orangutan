use crate::config::{KEYS_DIR, ROOT_KEY_NAME};

use std::io::{Write, Read};
use std::{fmt, io};
use std::fs::File;
use std::{env, path::PathBuf};

extern crate biscuit_auth as biscuit;
use lazy_static::lazy_static;
use tracing::trace;

lazy_static! {
    static ref KEYS_MODE: Result<String, env::VarError> = env::var("KEYS_MODE");
}

pub trait KeysReader {
    fn get_root_biscuit_key(&self) -> Result<biscuit::KeyPair, Error>;
}

impl dyn KeysReader {
    pub fn detect() -> Box<dyn KeysReader> {
        match KEYS_MODE.clone().unwrap_or("".to_string()).as_str() {
            "LOCAL" => Box::new(LocalKeysReader {}),
            "ENV" | _ => Box::new(EnvKeysReader {}),
        }
    }
}

struct EnvKeysReader {}

impl KeysReader for EnvKeysReader {
    fn get_root_biscuit_key(&self) -> Result<biscuit::KeyPair, Error> {
        let key_name = ROOT_KEY_NAME;

        let env_var_name = format!("KEY_{}", key_name);
        trace!("Reading key '{}' from environment ({})…", key_name, env_var_name);
        env::var(env_var_name)
            .map_err(Error::Env)
            .and_then(|key_bytes| {
                let key = biscuit::PrivateKey::from_bytes_hex(&key_bytes).map_err(Error::BiscuitFormat)?;
                Ok(biscuit::KeyPair::from(&key))
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
    fn get_root_biscuit_key(&self) -> Result<biscuit::KeyPair, Error> {
        let key_name = ROOT_KEY_NAME;

        let key_file = self.key_file(key_name);

        if key_file.exists() {
            // If key file exists, read the file
            trace!("Reading key '{}' from <{}>…", key_name, key_file.display());
            let mut file = File::open(key_file).map_err(Error::IO)?;
            let mut key_bytes = String::new();
            file.read_to_string(&mut key_bytes).map_err(Error::IO)?;
            let key = biscuit::PrivateKey::from_bytes_hex(&key_bytes).map_err(Error::BiscuitFormat)?;
            Ok(biscuit::KeyPair::from(&key))
        } else {
            // If key file does not exist, create a new key and save it to a new file
            trace!("Saving new key '{}' into <{}>…", key_name, key_file.display());
            let key_pair = biscuit::KeyPair::new();
            let mut file = File::create(&key_file).map_err(Error::IO)?;
            file.write_all(key_pair.private().to_bytes_hex().as_bytes()).map_err(Error::IO)?;
            Ok(key_pair)
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Env(env::VarError),
    IO(io::Error),
    CannotCreateFile(PathBuf, io::Error),
    CannotOpenFile(PathBuf, io::Error),
    CannotWriteInFile(PathBuf, io::Error),
    CannotReadFile(PathBuf, io::Error),
    BiscuitFormat(biscuit::error::Format),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Env(err) => err.fmt(f),
            Error::IO(err) => err.fmt(f),
            Error::CannotCreateFile(path, err) => write!(f, "Could not create <{}> file: {err}", path.display()),
            Error::CannotOpenFile(path, err) => write!(f, "Could not open <{}> file: {err}", path.display()),
            Error::CannotWriteInFile(path, err) => write!(f, "Could not write in <{}> file: {err}", path.display()),
            Error::CannotReadFile(path, err) => write!(f, "Could not read <{}> file: {err}", path.display()),
            Error::BiscuitFormat(err) => err.fmt(f),
        }
    }
}
