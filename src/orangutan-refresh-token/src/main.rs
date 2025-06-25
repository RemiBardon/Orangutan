extern crate biscuit_auth as biscuit;

use std::{env, process};

use orangutan_refresh_token::{Error, RefreshToken};
use tracing::error;
use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    if let Err(err) = main_() {
        error!("{err}");
        process::exit(1)
    }
}

fn main_() -> Result<(), Error> {
    // Parse arguments
    let duration = env::args()
        .skip(1)
        .next()
        .expect("Missing first argument (duration)");
    let profiles = env::args().skip(2);

    // Create token
    let refresh_token = RefreshToken::try_from(duration, profiles)?;
    let token_base64 = refresh_token.as_base64()?;

    // Print token to `stdout`
    println!("{token_base64}");

    Ok(())
}
