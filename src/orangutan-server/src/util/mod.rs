#[cfg(feature = "templating")]
pub mod templating;
#[cfg(feature = "token-generator")]
mod website_root;

use axum_extra::extract::{
    cookie::{Cookie, SameSite},
    PrivateCookieJar,
};
use biscuit_auth::{
    builder::{Fact, Term},
    Biscuit,
};
use chrono::Utc;
use time::Duration;
use tracing::error;

#[cfg(feature = "token-generator")]
pub use self::website_root::WebsiteRoot;
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
    cookies: PrivateCookieJar,
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

#[cfg(test)]
mod tests {
    use super::add_padding;

    #[test]
    fn test_base64_padding() {
        assert_eq!(add_padding("a"), "a===".to_string());
        assert_eq!(add_padding("ab"), "ab==".to_string());
        assert_eq!(add_padding("abc"), "abc=".to_string());
        assert_eq!(add_padding("abcd"), "abcd".to_string());

        assert_eq!(add_padding("a==="), "a===".to_string());
        assert_eq!(add_padding("ab=="), "ab==".to_string());
        assert_eq!(add_padding("abc="), "abc=".to_string());
        assert_eq!(add_padding("abcd"), "abcd".to_string());
    }

    // #[test]
    // fn test_should_force_token_refresh() {
    //     assert_eq!(should_force_token_refresh(None), false);
    //     assert_eq!(should_force_token_refresh(Some(Ok(true))), true);
    //     assert_eq!(should_force_token_refresh(Some(Ok(false))), false);
    //     assert_eq!(
    //         should_force_token_refresh(Some(Err(Errors::new().with_name("yes")))),
    //         true
    //     );
    //     assert_eq!(
    //         should_force_token_refresh(Some(Err(Errors::new().with_name("no")))),
    //         true
    //     );
    //     assert_eq!(
    //         should_force_token_refresh(Some(Err(Errors::new().with_name("")))),
    //         true
    //     );
    // }
}
