use std::{path::{PathBuf, Path}, env, collections::HashSet, fmt::Display};

use lazy_static;

use crate::helpers::used_profiles;

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
    pub static ref WEBSITE_DATA_DIR: PathBuf = DEST_DIR.join("data");
    pub static ref SUFFIXED_EXTENSIONS: Vec<&'static str> = vec!["html", "json", "xml", "css", "js", "txt"];

    pub static ref MODE: Result<String, env::VarError> = env::var("MODE");
    pub static ref KEYS_MODE: Result<String, env::VarError> = env::var("KEYS_MODE");
}

pub struct WebsiteId {
    pub profiles: HashSet<String>,
}

impl WebsiteId {
    fn dir_name(&self) -> String {
        // Convert HashSet<String> back to Vec<String> for sorting
        let mut unique_profiles: Vec<String> = self.profiles.clone().into_iter().collect();

        // Sort the profiles alphabetically
        unique_profiles.sort();

        // Join sorted profiles with ","
        unique_profiles.join(",")
    }
}

impl From<&Vec<String>> for WebsiteId {
    fn from(value: &Vec<String>) -> Self {
        if value.is_empty() {
            return Self::default()
        }

        // Convert Vec<String> to HashSet<String> to get unique profiles
        let mut profiles: HashSet<String> = value.to_owned().into_iter().collect();

        // Keep only profiles used by the website
        let mut used_profiles = used_profiles().clone();
        // Insert special "*" profile so it is kept for website generation
        used_profiles.insert("*".to_string());
        profiles = profiles.intersection(&used_profiles).map(|s| s.clone()).collect();

        return Self { profiles }
    }
}

impl Display for WebsiteId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.dir_name())
    }
}

impl Default for WebsiteId {
    fn default() -> Self {
        let profiles = vec![DEFAULT_PROFILE.to_string()].into_iter().collect();
        return Self { profiles }
    }
}

/// Returns a path to the website directory for a certain list of profiles.
/// This function also ensures uniqueness with a predictable name.
///
/// Website directory is suffixed by "@<p>" where "p" is a list of profiles,
/// sorted alphabetically and joined with ",".
pub fn website_dir(id: &WebsiteId) -> PathBuf {
    DEST_DIR.join(format!("{}@{}", WEBSITE_DIR_NAME, id.dir_name()))
}
