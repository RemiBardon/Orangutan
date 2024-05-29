use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use lazy_static::lazy_static;
use rocket::{
    get,
    http::{CookieJar, Status},
    routes, Route,
};

use crate::request_guards::Token;

lazy_static! {
    /// A list of runtime errors, used to show error logs in an admin page
    /// without having to open the cloud hosting provider's logs.
    ///
    /// // NOTE: `Arc` prevents race conditions
    pub(crate) static ref ERRORS: Arc<Mutex<Vec<(DateTime<Utc>, String)>>> = Arc::default();
}

pub(super) fn routes() -> Vec<Route> {
    routes![clear_cookies, get_user_info, errors]
}

#[get("/clear-cookies")]
fn clear_cookies(cookies: &CookieJar<'_>) -> &'static str {
    for cookie in cookies.iter().map(Clone::clone) {
        cookies.remove(cookie);
    }

    "Success"
}

#[get("/_info")]
fn get_user_info(token: Option<Token>) -> String {
    match token {
        Some(Token { biscuit, .. }) => format!(
            "**Biscuit:**\n\n{}\n\n\
            **Dump:**\n\n{}",
            biscuit.print(),
            biscuit
                .authorizer()
                .map_or_else(|e| format!("Error: {e}"), |a| a.dump_code()),
        ),
        None => "Not authenticated".to_string(),
    }
}

#[get("/_errors")]
fn errors(token: Token) -> Result<String, Status> {
    if token.profiles().contains(&"*".to_owned()) {
        Ok(ERRORS
            .lock()
            .unwrap()
            .iter()
            .map(|(d, l)| format!("{d} | {l}"))
            .collect::<Vec<_>>()
            .join("\n"))
    } else {
        Err(Status::Unauthorized)
    }
}
