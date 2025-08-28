extern crate biscuit_auth as biscuit;

use std::{process, time::SystemTime};

use biscuit::{
    builder::{BlockBuilder, Fact},
    macros::{block, fact},
    Biscuit,
};
use iso8601_duration::Duration as IsoDuration;
use lazy_static::lazy_static;
use orangutan_helpers::readers::keys_reader::KeysReader;
use tracing::error;

lazy_static! {
    static ref ROOT_KEY: biscuit::KeyPair = {
        let keys_reader = <dyn KeysReader>::detect();
        match keys_reader.get_root_biscuit_key() {
            Ok(public_key) => public_key,
            Err(err) => {
                error!("Error generating root Biscuit key: {err}");
                process::exit(1);
            },
        }
    };
}

pub struct RefreshToken(Biscuit);

impl RefreshToken {
    /// Try to create a `RefreshToken` from strongly typed data.
    pub fn new(
        duration: std::time::Duration,
        profiles: impl Iterator<Item = String>,
    ) -> Result<Self, Error> {
        let mut builder = Biscuit::builder();

        // Add profiles to Biscuit
        for profile in profiles {
            let fact = fact!("profile({profile});");
            builder
                .add_fact(fact.to_owned())
                .map_err(|e| Error::CannotAddFact(fact, e))?;
        }

        // Create first Biscuit block
        let mut biscuit = builder
            .build(&ROOT_KEY)
            .map_err(Error::CannotBuildBiscuit)?;

        // Add expiry block to Biscuit
        let expiry_block = block!(
            "check if time($time), $time <= {expiry};",
            expiry = SystemTime::now() + duration,
        );
        biscuit = biscuit
            .append(expiry_block.to_owned())
            .map_err(|e| Error::CannotAddBlock(expiry_block, e))?;

        Ok(Self(biscuit))
    }

    /// Try to create a `RefreshToken` from loosely typed `String`s.
    pub fn try_from(
        duration: String,
        profiles: impl Iterator<Item = String>,
    ) -> Result<Self, Error> {
        let duration = IsoDuration::parse(&duration)
            .map_err(|e| Error::MalformattedDuration(duration.clone(), e))?
            .to_std()
            .ok_or(Error::UnsupportedDuration(duration.clone()))?;
        Self::new(duration, profiles)
    }

    pub fn as_base64(&self) -> Result<String, Error> {
        // Encode Biscuit to Base64
        let biscuit_base64 = self
            .0
            .to_base64()
            // Remove Base64 padding (for easier URL query parameter parsing)
            .map(|b| remove_padding(&b).to_owned())
            .map_err(Error::CannotConvertToBase64)?;

        Ok(biscuit_base64)
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

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Could not add fact '{0:?}' to Biscuit: {1}")]
    CannotAddFact(Fact, biscuit::error::Token),
    #[error("Error building Biscuit: {0}")]
    CannotBuildBiscuit(biscuit::error::Token),
    #[error("Could not add block '{0}' to Biscuit: {1}")]
    CannotAddBlock(BlockBuilder, biscuit::error::Token),
    #[error("Could convert Biscuit to Base64: {0}")]
    CannotConvertToBase64(biscuit::error::Token),
    #[error("Duration '{0}' malformatted ({1:?}). Check ISO 8601.")]
    MalformattedDuration(String, iso8601_duration::ParseDurationError),
    // FIXME: Support `year` and `month` durations
    #[error("Could not convert `iso8601_duration::Duration` to `std::time::Duration`. '{0}' contains `year` or `month`, which isn't supported by `iso8601_duration::duration::Duration::to_std`.")]
    UnsupportedDuration(String),
}
