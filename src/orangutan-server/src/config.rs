use std::process::exit;

use lazy_static::lazy_static;
pub(super) use orangutan_helpers::config::*;
use orangutan_helpers::readers::keys_reader::KeysReader;
use tracing::error;

pub(super) const TOKEN_COOKIE_NAME: &'static str = "token";
pub(super) const TOKEN_QUERY_PARAM_NAME: &'static str = "token";
pub(super) const REFRESH_TOKEN_QUERY_PARAM_NAME: &'static str = "refresh_token";
pub(super) const NOT_FOUND_FILE: &'static str = "404.html";

lazy_static! {
    pub(super) static ref ROOT_KEY: biscuit_auth::KeyPair = {
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
