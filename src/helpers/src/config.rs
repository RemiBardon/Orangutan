use std::{env, path::PathBuf};

use lazy_static::lazy_static;

pub const THEME_NAME: &str = "Orangutan";
pub const DATA_FILE_EXTENSION: &str = "orangutan";
pub const DEFAULT_PROFILE: &str = "_default";
pub const ROOT_KEY_NAME: &'static str = "_biscuit_root";
pub const COOKIE_KEY_ENV_VAR_NAME: &'static str = "COOKIE_ENCRYPTION_KEY";

pub(super) const WEBSITE_DIR_NAME: &'static str = "website";

lazy_static! {
    pub static ref WEBSITE_REPOSITORY: String = env::var("WEBSITE_REPOSITORY")
        .expect("Environment variable `WEBSITE_REPOSITORY` is required.");
    pub static ref MODE: Result<String, env::VarError> = env::var("MODE");
    pub static ref KEYS_MODE: Result<String, env::VarError> = env::var("KEYS_MODE");
}
lazy_static! {
    static ref WORK_DIR: PathBuf = env::current_dir().unwrap();
    pub static ref BASE_DIR: PathBuf = WORK_DIR.join(".orangutan");
    pub static ref TMP_DIR: PathBuf = BASE_DIR.join("tmp");
    pub static ref KEYS_DIR: PathBuf = BASE_DIR.join("keys");
    pub(super) static ref WEBSITE_ROOT: PathBuf = BASE_DIR.join("website-src");
    pub(super) static ref HUGO_CONFIG_DIR: PathBuf = BASE_DIR.join("hugo-config");
    pub static ref DEST_DIR: PathBuf = BASE_DIR.join("out");
    pub static ref WEBSITE_DATA_DIR: PathBuf = DEST_DIR.join("data");
}
