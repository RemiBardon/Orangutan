use std::io::{Read, Write};
use std::{env, fmt, io};
use std::fs::File;
use std::path::{PathBuf, Path};
use std::process::exit;
use std::time::SystemTime;
use iso8601_duration::Duration as IsoDuration;

extern crate biscuit_auth as biscuit;
use biscuit::Biscuit;
use biscuit::macros::{fact, block};
#[macro_use]
extern crate lazy_static;
use log::{error, trace};
use urlencoding::encode;

const ROOT_KEY_NAME: &'static str = "_biscuit_root";

lazy_static! {
    static ref BASE_DIR: &'static Path = Path::new(".orangutan");
    static ref KEYS_DIR: PathBuf = BASE_DIR.join("keys");

    static ref ROOT_KEY: biscuit::KeyPair = {
        match get_root_key() {
            Ok(public_key) => public_key,
            Err(err) => {
                error!("Error generating root Biscuit key: {}", err);
                exit(1);
            }
        }
    };
}

fn main() {
    let mut builder = Biscuit::builder();
    for profile in env::args().skip(2) {
        let fact = fact!("profile({profile});");
        builder.add_fact(fact.clone())
            .expect(&format!("Could not add fact '{:?}' to Biscuit", fact));
    }
    match builder.build(&ROOT_KEY) {
        Ok(mut biscuit) => {
            let duration = IsoDuration::parse(
                &env::args().skip(1).next()
                    .expect("Duration required as the first argument.")
                )
                .expect("Duration malformatted. Check ISO 8601.")
                .to_std()
                .expect("Cannot convert `iso8601_duration::Duration` to `std::time::Duration`.");
            let expiry_block = block!(
                "check if time($time), $time <= {expiry};",
                expiry = SystemTime::now() + duration,
            );
            biscuit = biscuit.append(expiry_block)
                .expect(&format!("Could not add block '' to Biscuit"));
            match biscuit.to_base64() {
                Ok(biscuit_base64) => println!("{}", encode(&biscuit_base64)),
                Err(err) => error!("{}", err),
            }
        },
        Err(err) => error!("{}", err),
    }
}

fn get_root_key() -> Result<biscuit::KeyPair, Error> {
    let key_name = ROOT_KEY_NAME;
    let key_file = key_file(key_name);

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

fn key_file(key_name: &str) -> PathBuf {
    KEYS_DIR.join(format!("{}.key", key_name))
}

#[derive(Debug)]
enum Error {
    IO(io::Error),
    BiscuitFormat(biscuit::error::Format),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::IO(err) => err.fmt(f),
            Error::BiscuitFormat(err) => err.fmt(f),
        }
    }
}
