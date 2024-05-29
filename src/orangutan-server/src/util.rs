use biscuit_auth::{
    builder::{Fact, Term},
    Biscuit,
};
use chrono::Utc;
use rocket::http::{Cookie, CookieJar, SameSite};
use time::Duration;
use tracing::error;

use crate::{
    config::TOKEN_COOKIE_NAME,
    routes::debug_routes::{ErrorLog, ERRORS},
};

pub fn error(err: String) {
    ERRORS.write().unwrap().push(ErrorLog {
        timestamp: Utc::now(),
        line: err.to_owned(),
    });
    error!(err);
}

pub fn profiles(biscuit: &Biscuit) -> Vec<String> {
    biscuit
        .authorizer()
        .unwrap()
        .query_all("data($name) <- profile($name)")
        .unwrap()
        .iter()
        .map(|f: &Fact| match f.predicate.terms.get(0).unwrap() {
            Term::Str(s) => s.clone(),
            t => panic!("Term {t} should be of type String"),
        })
        .collect()
}

pub fn add_padding(base64_string: &str) -> String {
    // If the base64 string is already padded, don't do anything.
    if base64_string.ends_with("=") {
        return base64_string.to_string();
    }

    match base64_string.len() % 4 {
        // If the base64 string has a multiple of 4 characters, don't do anything.
        0 => base64_string.to_string(),
        // If the base64 string doesn't have a multiple of 4 characters,
        // create a new string with the required padding characters.
        n => format!("{}{}", base64_string, "=".repeat(4 - n)),
    }
}

pub fn add_cookie(
    biscuit: &Biscuit,
    cookies: &CookieJar<'_>,
) {
    match biscuit.to_base64() {
        Ok(base64) => {
            cookies.add(
                Cookie::build((TOKEN_COOKIE_NAME, base64))
                    .path("/")
                    .max_age(Duration::days(365 * 5))
                    .http_only(true)
                    .secure(true)
                    .same_site(SameSite::Strict),
            );
        },
        Err(err) => {
            error(format!("Error setting token cookie: {err}"));
        },
    }
}
