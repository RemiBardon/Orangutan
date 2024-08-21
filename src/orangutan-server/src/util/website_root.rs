use std::ops::Deref;

use lazy_static::lazy_static;

lazy_static! {
    static ref WEBSITE_ROOT: String = std::env::var("WEBSITE_ROOT").unwrap_or_default();
}

#[derive(Clone)]
pub struct WebsiteRoot(String);

impl Deref for WebsiteRoot {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl WebsiteRoot {
    pub fn try_from_env() -> Result<Self, &'static str> {
        if WEBSITE_ROOT.is_empty() {
            Err("Environment variable `WEBSITE_ROOT` not found.")
        } else {
            Ok(Self(WEBSITE_ROOT.to_owned()))
        }
    }
}
