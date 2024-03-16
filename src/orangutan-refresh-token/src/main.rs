use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::exit;
use std::time::SystemTime;
use std::{env, fmt, io};

use iso8601_duration::Duration as IsoDuration;

extern crate biscuit_auth as biscuit;
use biscuit::macros::{block, fact};
use biscuit::Biscuit;
use lazy_static::lazy_static;
use orangutan_helpers::config::KEYS_DIR;
use tracing::{error, trace};

const ROOT_KEY_NAME: &'static str = "_biscuit_root";

lazy_static! {
    static ref KEYS_MODE: Result<String, env::VarError> = env::var("KEYS_MODE");
    static ref ROOT_KEY: biscuit::KeyPair = {
        let keys_reader = <dyn KeysReader>::detect();
        match keys_reader.get_root_biscuit_key() {
            Ok(public_key) => public_key,
            Err(err) => {
                error!("Error generating root Biscuit key: {err}");
                exit(1);
            },
        }
    };
}

fn main() {
    let mut builder = Biscuit::builder();
    for profile in env::args().skip(2) {
        let fact = fact!("profile({profile});");
        builder
            .add_fact(fact.clone())
            .expect(&format!("Could not add fact '{fact:?}' to Biscuit"));
    }
    match builder.build(&ROOT_KEY) {
        Ok(mut biscuit) => {
            let duration = IsoDuration::parse(
                &env::args()
                    .skip(1)
                    .next()
                    .expect("Duration required as the first argument."),
            )
            .expect("Duration malformatted. Check ISO 8601.")
            .to_std()
            .expect("Cannot convert `iso8601_duration::Duration` to `std::time::Duration`.");
            let expiry_block = block!(
                "check if time($time), $time <= {expiry};",
                expiry = SystemTime::now() + duration,
            );
            biscuit = biscuit
                .append(expiry_block)
                .expect(&format!("Could not add block '' to Biscuit"));
            match biscuit.to_base64() {
                Ok(biscuit_base64) => {
                    let biscuit_base64 = remove_padding(&biscuit_base64);
                    println!("{biscuit_base64}")
                },
                Err(err) => error!("Error converting Biscuit to Base64: {err}"),
            }
        },
        Err(err) => error!("Error building Biscuit: {err}"),
    }
}

trait KeysReader {
    fn get_root_biscuit_key(&self) -> Result<biscuit::KeyPair, Error>;
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
    fn get_root_biscuit_key(&self) -> Result<biscuit::KeyPair, Error> {
        let key_name = ROOT_KEY_NAME;

        let env_var_name = format!("KEY_{}", key_name);
        trace!(
            "Reading key '{}' from environment ({})…",
            key_name,
            env_var_name
        );
        env::var(env_var_name)
            .map_err(Error::Env)
            .and_then(|key_bytes| {
                let key = biscuit::PrivateKey::from_bytes_hex(&key_bytes)
                    .map_err(Error::BiscuitFormat)?;
                Ok(biscuit::KeyPair::from(&key))
            })
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
            let mut file = File::open(key_file).map_err(Error::IO)?;
            let mut key_bytes = String::new();
            file.read_to_string(&mut key_bytes).map_err(Error::IO)?;
            let key =
                biscuit::PrivateKey::from_bytes_hex(&key_bytes).map_err(Error::BiscuitFormat)?;
            Ok(biscuit::KeyPair::from(&key))
        } else {
            // If key file does not exist, create a new key and save it to a new file
            trace!(
                "Saving new key '{}' into <{}>…",
                key_name,
                key_file.display()
            );
            let key_pair = biscuit::KeyPair::new();
            let mut file = File::create(&key_file).map_err(Error::IO)?;
            file.write_all(key_pair.private().to_bytes_hex().as_bytes())
                .map_err(Error::IO)?;
            Ok(key_pair)
        }
    }
}

#[derive(Debug)]
enum Error {
    IO(io::Error),
    Env(env::VarError),
    BiscuitFormat(biscuit::error::Format),
}

impl fmt::Display for Error {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::IO(err) => write!(f, "IO error: {err}"),
            Self::Env(err) => write!(f, "Env error: {err}"),
            Self::BiscuitFormat(err) => write!(f, "Biscuit format error: {err}"),
        }
    }
}

fn remove_padding<'a>(base64_string: &'a str) -> &'a str {
    // Find the position of the first '=' character
    if let Some(index) = base64_string.find('=') {
        // Remove all characters from the first '=' character to the end
        let result = &base64_string[0..index];
        return result;
    }
    // If no '=' character is found, return the original string
    base64_string
}
