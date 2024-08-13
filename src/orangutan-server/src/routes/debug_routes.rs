use std::sync::{Arc, RwLock};

use axum::{routing::get, Router};
use axum_extra::extract::PrivateCookieJar;
use chrono::{DateTime, Utc};
use lazy_static::lazy_static;

use crate::{
    request_guards::{Token, REVOKED_TOKENS},
    Error,
};

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

pub(super) fn router() -> Router {
    let mut router = Router::new()
        .route("/clear-cookies", get(clear_cookies).put(clear_cookies))
        .route("/_info", get(get_user_info))
        .route("/_errors", get(errors))
        .route("/_access-logs", get(access_logs))
        .route("/_revoked-tokens", get(revoked_tokens));
    #[cfg(feature = "token-generator")]
    {
        router = router.route(
            "/_generate-token",
            get(token_generator::token_generation_form).post(token_generator::generate_token),
        );
    }
    router
}

#[cfg(feature = "templating")]
pub(super) fn templates() -> Vec<(&'static str, &'static str)> {
    vec![(
        "generate-token.html",
        include_str!("templates/generate-token.html.tera"),
    )]
}

fn clear_cookies(cookies: PrivateCookieJar) -> &'static str {
    for cookie in cookies.iter().map(Clone::clone) {
        cookies.remove(cookie);
    }

    "Success"
}

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

fn errors(token: Token) -> Result<String, Error> {
    if !token.profiles().contains(&"*".to_owned()) {
        Err(Error::Unauthorized)?
    }

    let mut res = String::new();
    for log in ERRORS.read().unwrap().iter() {
        res.push_str(&format!("{} | {}\n", log.timestamp, log.line));
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

fn access_logs(token: Token) -> Result<String, Error> {
    if !token.profiles().contains(&"*".to_owned()) {
        Err(Error::Unauthorized)?
    }

    let mut res = String::new();
    for AccessLog {
        timestamp,
        user,
        path,
    } in ACCESS_LOGS.read().unwrap().iter()
    {
        let mut profiles = user.clone();
        // Sort profiles so they are always presented in the same order
        profiles.sort();
        let user = if profiles.is_empty() {
            "?".to_owned()
        } else {
            profiles.join(",")
        };

        res.push_str(&format!("{timestamp} | {user}: {path}\n"));
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

fn revoked_tokens(token: Token) -> Result<String, Error> {
    if !token.profiles().contains(&"*".to_owned()) {
        Err(Error::Forbidden)?
    }

    let mut res = String::new();
    for token in REVOKED_TOKENS.read().unwrap().iter() {
        res.push_str(std::str::from_utf8(token).unwrap_or("<cannot parse>"));
        res.push('\n');
    }

    Ok(res)
}

#[cfg(feature = "token-generator")]
pub mod token_generator {
    use axum::{extract::State, Form};
    use axum_extra::response::Html;
    use orangutan_refresh_token::RefreshToken;
    use serde::Deserialize;

    use crate::{
        context,
        request_guards::Token,
        util::{templating::render, WebsiteRoot},
        AppState, Error,
    };

    fn token_generation_form_(
        tera: &tera::Tera,
        link: Option<String>,
        base_url: &str,
    ) -> Result<Html<String>, Error> {
        let html = render(
            tera,
            "generate-token.html",
            context! { page_title: "Access token generator", link, base_url },
        )?;

        Ok(Html(html))
    }

    pub fn token_generation_form(
        token: Token,
        State(app_state): State<AppState>,
        website_root: WebsiteRoot,
    ) -> Result<Html<String>, Error> {
        if !token.profiles().contains(&"*".to_owned()) {
            Err(Error::Unauthorized)?
        }

        token_generation_form_(&app_state.tera, None, &website_root)
    }

    #[derive(Deserialize)]
    pub struct GenerateTokenForm {
        ttl: String,
        name: String,
        profiles: String,
        url: String,
    }

    pub fn generate_token(
        token: Token,
        State(app_state): State<AppState>,
        Form(form): Form<GenerateTokenForm>,
        website_root: WebsiteRoot,
    ) -> Result<Html<String>, Error> {
        if !token.profiles().contains(&"*".to_owned()) {
            Err(Error::Unauthorized)?
        }

        let mut profiles = vec![form.name.to_owned()];
        profiles.append(&mut form.profiles.split(",").map(ToOwned::to_owned).collect());
        if profiles.contains(&"*".to_string()) {
            Err(Error::ClientError(format!(
                "Profiles cannot contain '*' (got {profiles:?})."
            )))?
        }

        let token = RefreshToken::try_from(form.ttl.to_owned(), profiles.into_iter())?;
        let token_base64 = token.as_base64()?;
        let link = format!("{}?refresh_token={token_base64}", form.url);

        token_generation_form_(&app_state.tera, Some(link), &website_root)
    }
}
