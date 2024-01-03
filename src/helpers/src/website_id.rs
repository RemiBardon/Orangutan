use crate::config::*;
use crate::used_profiles;

use std::collections::HashSet;
use std::fmt::Display;
use std::path::PathBuf;

pub struct WebsiteId {
    pub profiles: HashSet<String>,
}

impl WebsiteId {
    pub fn name(&self) -> String {
        // Convert HashSet<String> back to Vec<String> for sorting
        let mut unique_profiles: Vec<String> = self.profiles.clone().into_iter().collect();

        // Sort the profiles alphabetically
        unique_profiles.sort();

        if unique_profiles.is_empty() {
            DEFAULT_PROFILE.to_string()
        } else {
            // Join sorted profiles with ","
            unique_profiles.join(",")
        }
    }

    pub fn dir_name(&self) -> String {
        format!("{}@{}", WEBSITE_DIR_NAME, self.name()).to_string()
    }
}

impl Display for WebsiteId {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        write!(f, "{}", self.dir_name())
    }
}

impl Default for WebsiteId {
    fn default() -> Self {
        let profiles = vec![DEFAULT_PROFILE.to_string()].into_iter().collect();
        return Self { profiles };
    }
}

impl From<&Vec<String>> for WebsiteId {
    fn from(value: &Vec<String>) -> Self {
        if value.is_empty() {
            return Self::default();
        }

        // Convert Vec<String> to HashSet<String> to get unique profiles
        let mut profiles: HashSet<String> = value.to_owned().into_iter().collect();

        // Keep only profiles used by the website
        let mut used_profiles = used_profiles().clone();
        // Insert special "*" profile so it is kept for website generation
        used_profiles.insert("*".to_string());
        profiles = profiles
            .intersection(&used_profiles)
            .map(|s| s.clone())
            .collect();

        return Self { profiles };
    }
}

/// Returns a path to the website directory for a certain list of profiles.
/// This function also ensures uniqueness with a predictable name.
///
/// Website directory is suffixed by "@<p>" where "p" is a list of profiles,
/// sorted alphabetically and joined with ",".
pub fn website_dir(id: &WebsiteId) -> PathBuf {
    DEST_DIR.join(id.dir_name())
}
