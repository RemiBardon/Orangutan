use std::{path::{PathBuf, Path}, env};

use lazy_static;

pub const THEME_NAME: &str = "Orangutan";
pub const DATA_FILE_EXTENSION: &str = "orangutan";
pub const READ_ALLOWED_FIELD: &str = "read_allowed";
pub const PATH_FIELD: &str = "path";
pub const DEFAULT_PROFILE: &str = "_default";
pub const ROOT_KEY_NAME: &'static str = "_biscuit_root";
pub const TOKEN_COOKIE_NAME: &'static str = "token";
pub const TOKEN_QUERY_PARAM_NAME: &'static str = "token";
pub const REFRESH_TOKEN_QUERY_PARAM_NAME: &'static str = "refresh_token";
pub const FORCE_QUERY_PARAM_NAME: &'static str = "force";
pub const NOT_FOUND_FILE: &'static str = "/404.html";

const WEBSITE_DIR_NAME: &'static str = "website";

lazy_static! {
    pub static ref BASE_DIR: &'static Path = Path::new(".orangutan");
    pub static ref KEYS_DIR: PathBuf = BASE_DIR.join("keys");
    pub static ref DEST_DIR: PathBuf = BASE_DIR.join("out");
    pub static ref WEBSITE_DIR: PathBuf = DEST_DIR.join(WEBSITE_DIR_NAME);
    pub static ref WEBSITE_DATA_DIR: PathBuf = DEST_DIR.join("data");
    pub static ref SUFFIXED_EXTENSIONS: Vec<&'static str> = vec!["html", "json", "xml", "css", "js", "txt"];

    pub static ref MODE: Result<String, env::VarError> = env::var("MODE");
    pub static ref KEYS_MODE: Result<String, env::VarError> = env::var("KEYS_MODE");
}

pub fn website_dir(profile: String) -> PathBuf {
    WEBSITE_DIR.with_file_name(format!("{}-{}", WEBSITE_DIR_NAME, profile))
}
