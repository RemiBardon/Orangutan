use std::sync::{Arc, RwLock};

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
    pub(crate) static ref ERRORS: Arc<RwLock<Vec<ErrorLog>>> = Arc::default();
    /// Access logs, per "user".
    ///
    /// // NOTE: `Arc` prevents race conditions
    pub(crate) static ref ACCESS_LOGS: Arc<RwLock<Vec<AccessLog>>> = Arc::default();
}

pub(super) fn routes() -> Vec<Route> {
    routes![
        clear_cookies,
        get_user_info,
        errors,
        access_logs
    ]
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

pub struct ErrorLog {
    pub timestamp: DateTime<Utc>,
    pub line: String,
}

#[get("/_errors")]
fn errors(token: Token) -> Result<String, Status> {
    if !token.profiles().contains(&"*".to_owned()) {
        Err(Status::Unauthorized)?
    }

    let mut res = String::new();
    for log in ERRORS.read().unwrap().iter() {
        res.push_str(&format!("{} | {}", log.timestamp, log.line));
    }

    Ok(res)
}

/// In Orangutan, users are a list of profiles.
///
/// NOTE: One day we will introduce `user` facts in Biscuit tokens
///   to differenciate the unique name from profiles.
///   That day we will change this type to just `String`.
type User = Vec<String>;

pub struct AccessLog {
    pub timestamp: DateTime<Utc>,
    pub user: User,
    pub path: String,
}

#[get("/_access-logs")]
fn access_logs(token: Token) -> Result<String, Status> {
    if !token.profiles().contains(&"*".to_owned()) {
        Err(Status::Unauthorized)?
    }

    let mut res = String::new();
    for log in ACCESS_LOGS.read().unwrap().iter() {
        let mut user = log.user.clone();
        user.sort();
        res.push_str(&format!(
            "{} | {}: {}\n",
            log.timestamp,
            user.join(","),
            log.path
        ));
    }

    Ok(res)
}

pub fn log_access(
    user: User,
    path: String,
) {
    ACCESS_LOGS.write().unwrap().push(AccessLog {
        timestamp: Utc::now(),
        user,
        path,
    })
}
