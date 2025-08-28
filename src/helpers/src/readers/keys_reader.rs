use std::{
    env,
    fs::File,
    io,
    io::{Read, Write},
    path::PathBuf,
};

use crate::config::{KEYS_DIR, ROOT_KEY_NAME};

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
        trace!(
            "Reading key '{}' from environment ({})…",
            key_name,
            env_var_name
        );
        let key_bytes = env::var(env_var_name)?;
        let key = biscuit::PrivateKey::from_bytes_hex(&key_bytes)?;
        Ok(biscuit::KeyPair::from(&key))
    }
}

struct LocalKeysReader {}

impl LocalKeysReader {
    fn key_file(
        &self,
        key_name: &str,
    ) -> PathBuf {
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
            let mut file = File::open(key_file)?;
            let mut key_bytes = String::new();
            file.read_to_string(&mut key_bytes)?;
            let key = biscuit::PrivateKey::from_bytes_hex(&key_bytes)?;
            Ok(biscuit::KeyPair::from(&key))
        } else {
            // If key file does not exist, create a new key and save it to a new file
            trace!(
                "Saving new key '{}' into <{}>…",
                key_name,
                key_file.display()
            );
            let key_pair = biscuit::KeyPair::new();
            let mut file = File::create(&key_file)?;
            file.write_all(key_pair.private().to_bytes_hex().as_bytes())?;
            Ok(key_pair)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Env error: {0}")]
    Env(#[from] env::VarError),
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
    #[error("Could not create <{path}> file: {1}", path = .0.display())]
    CannotCreateFile(PathBuf, io::Error),
    #[error("Could not open <{path}> file: {1}", path = .0.display())]
    CannotOpenFile(PathBuf, io::Error),
    #[error("Could not write in <{path}> file: {1}", path = .0.display())]
    CannotWriteInFile(PathBuf, io::Error),
    #[error("Could not read <{path}> file: {1}", path = .0.display())]
    CannotReadFile(PathBuf, io::Error),
    #[error("{0}")]
    BiscuitFormat(#[from] biscuit::error::Format),
}
