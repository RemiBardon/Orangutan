use std::ops::Deref;

use lazy_static::lazy_static;
use rocket::{
    request::{FromRequest, Outcome},
    Ignite, Request, Rocket,
};
use tracing::error;

lazy_static! {
    static ref WEBSITE_ROOT: String = std::env::var("WEBSITE_ROOT").unwrap_or_default();
}

pub struct WebsiteRoot(String);

impl Deref for WebsiteRoot {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for WebsiteRoot {
    type Error = &'static str;

    async fn from_request(_req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(Self(WEBSITE_ROOT.to_owned()))
    }
}

impl rocket::Sentinel for WebsiteRoot {
    fn abort(_rocket: &Rocket<Ignite>) -> bool {
        if WEBSITE_ROOT.is_empty() {
            error!("Environment variable `WEBSITE_ROOT` not found.");
            return true;
        }
        false
    }
}
