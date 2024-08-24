use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
    str::FromStr,
    sync::RwLock,
    time::SystemTime,
};

use axum::{
    extract::{rejection::QueryRejection, FromRef, FromRequestParts, Query, Request, State},
    http::{request, HeaderMap, HeaderValue, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::{
    either::Either,
    extract::{cookie::Key, PrivateCookieJar},
};
use biscuit_auth::{macros::authorizer, Biscuit};
use lazy_static::lazy_static;
use serde::Deserialize;
use tracing::{debug, trace};

use crate::{
    config::*,
    util::{add_cookie, add_padding, error, profiles},
    AppState,
};

lazy_static! {
    pub static ref REVOKED_TOKENS: RwLock<HashSet<Vec<u8>>> = RwLock::default();
}

pub struct Token {
    pub biscuit: Biscuit,
}

impl Token {
    pub fn profiles(&self) -> Vec<String> {
        profiles(&self.biscuit)
    }
}

impl Deref for Token {
    type Target = Biscuit;

    fn deref(&self) -> &Self::Target {
        &self.biscuit
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    // TODO: Re-enable Basic authentication
    // Invalid,
    #[error("Invalid query: {0}")]
    InvalidQuery(#[from] QueryRejection),
    #[error("Unauthorized")]
    Unauthorized,
}
impl IntoResponse for TokenError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidQuery(err) => err.into_response(),
            Self::Unauthorized => StatusCode::UNAUTHORIZED.into_response(),
        }
    }
}

#[axum::async_trait]
impl<S> FromRequestParts<S> for Token
where
    S: Send + Sync,
    Key: FromRef<S>,
{
    type Rejection = TokenError;

    async fn from_request_parts(
        parts: &mut request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        // if let Some(user_agent) = parts.headers.get(USER_AGENT) {
        //     Ok(ExtractUserAgent(user_agent.clone()))
        // } else {
        //     Err((StatusCode::BAD_REQUEST, "`User-Agent` header is missing"))
        // }
        let mut biscuit: Option<Biscuit> = None;
        let mut should_save: bool = false;

        fn process_token(
            token: &str,
            token_source: &str,
            biscuit: &mut Option<Biscuit>,
            should_save: &mut bool,
        ) {
            // Because tokens can be passed as URL query params,
            // they might have the "=" padding characters removed.
            // We need to add them back.
            let token = add_padding(token);
            match (
                biscuit.clone(),
                Biscuit::from_base64(token, ROOT_KEY.public()),
            ) {
                (None, Ok(new_biscuit)) => {
                    trace!("Found biscuit in {}", token_source);
                    *biscuit = Some(new_biscuit);
                    *should_save = true;
                },
                (Some(acc), Ok(new_biscuit)) => {
                    trace!("Making bigger biscuit from {}", token_source);
                    if let Some(b) = merge_biscuits(&acc, &new_biscuit) {
                        *biscuit = Some(b);
                        *should_save = true;
                    }
                },
                (_, Err(err)) => {
                    debug!("Error decoding biscuit from base64: {}", err);
                },
            }
        }

        // Check cookies
        let cookies = PrivateCookieJar::from_request_parts(parts, state)
            .await
            .map_err(|err| match err {})
            .unwrap();
        if let Some(cookie) = cookies.get(TOKEN_COOKIE_NAME) {
            debug!("Found token cookie");
            let token: &str = cookie.value();
            // NOTE: We don't want to send a `Set-Cookie` header after finding a token in a cookie,
            //   so let's create a temporary value which prevents `process_token` from changing
            //   the global `should_save` value.
            let mut should_save = false;
            process_token(token, "token cookie", &mut biscuit, &mut should_save);
        } else {
            debug!("Did not find a token cookie");
        }

        // Check authorization headers
        let headers = HeaderMap::from_request_parts(parts, state)
            .await
            .map_err(|err| match err {})
            .unwrap();
        let authorization_headers: Vec<&HeaderValue> =
            headers.get_all("Authorization").into_iter().collect();
        debug!(
            "{} 'Authorization' headers provided",
            authorization_headers.len()
        );
        for authorization in authorization_headers {
            let authorization = match authorization.to_str() {
                Ok(str) => str,
                Err(_err) => {
                    trace!("Skipped 1 authorization header because it contains non visible ASCII chars.");
                    continue;
                },
            };
            if authorization.starts_with("Bearer ") {
                debug!("Bearer Authorization provided");
                let token: &str = authorization.trim_start_matches("Bearer ");
                process_token(token, "Bearer token", &mut biscuit, &mut should_save);
            } else if authorization.starts_with("Basic ") {
                debug!("Basic authentication disabled");
            }
        }

        // Check query params
        let query = Query::<HashMap<String, String>>::from_request_parts(parts, state).await?;
        if let Some(token) = query.get(TOKEN_QUERY_PARAM_NAME) {
            debug!("Found token query param");
            process_token(&token, "token query param", &mut biscuit, &mut should_save);
        }

        match biscuit {
            Some(biscuit) => {
                if should_save {
                    add_cookie(&biscuit, cookies);
                }
                Ok(Token { biscuit })
            },
            None => Err(TokenError::Unauthorized),
        }
    }
}

#[derive(Deserialize)]
pub struct RefreshTokenQuery {
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    force: bool,
}

pub async fn handle_refresh_token(
    State(_app_state): State<AppState>,
    cookies: PrivateCookieJar,
    Query(RefreshTokenQuery {
        refresh_token,
        force,
        ..
    }): Query<RefreshTokenQuery>,
    token: Option<Token>,
    req: Request,
    next: Next,
) -> Result<Either<Response, (PrivateCookieJar, Redirect)>, StatusCode> {
    trace!("Running refresh token middleware…");
    let Some(refresh_token) = refresh_token else {
        trace!("Refresh token middleware found no refresh token, forwarding…");
        return Ok(Either::E1(next.run(req).await));
    };

    // NOTE: We're sure there is a query since `refresh_token` is `Some`.
    let query = req.uri().query().unwrap();
    let mut query: HashMap<String, String> = match serde_urlencoded::from_str(query) {
        Ok(params) => params,
        Err(err) => {
            trace!("Invalid query: {err}");
            return Err(StatusCode::BAD_REQUEST);
        },
    };

    // URL-decode the string.
    let mut refresh_token: String = urlencoding::decode(&refresh_token).unwrap().to_string();

    // Because tokens can be passed as URL query params,
    // they might have the "=" padding characters removed.
    // We need to add them back.
    refresh_token = add_padding(&refresh_token);

    let refresh_biscuit: Biscuit = match Biscuit::from_base64(refresh_token, ROOT_KEY.public()) {
        Ok(biscuit) => biscuit,
        Err(err) => {
            debug!("Error decoding biscuit from base64: {}", err);
            return Err(StatusCode::UNAUTHORIZED);
        },
    };

    // NOTE: This is just a hotfix. I had to quickly revoke a token. I'll improve this one day.
    trace!("Checking if refresh token is revoked…");
    trace!(
        "Revocation identifiers: {}",
        refresh_biscuit
            .revocation_identifiers()
            .into_iter()
            .map(hex::encode)
            .collect::<Vec<_>>()
            .join(", "),
    );
    let revoked_id = refresh_biscuit
        .revocation_identifiers()
        .into_iter()
        .collect::<HashSet<Vec<u8>>>()
        .intersection(&REVOKED_TOKENS.read().unwrap())
        .next()
        .cloned();
    if let Some(revoked_id) = revoked_id {
        debug!(
            "Refresh token has been revoked ({})",
            String::from_utf8(revoked_id).unwrap_or("<could not format>".to_string()),
        );
        return Err(StatusCode::FORBIDDEN);
    }

    trace!("Checking if refresh token is valid or not");
    let authorizer = authorizer!(
        r#"
        time({now});
        allow if true;
        "#,
        now = SystemTime::now(),
    );
    if let Err(err) = refresh_biscuit.authorize(&authorizer) {
        debug!("Refresh token is invalid: {}", err);
        return Err(StatusCode::UNAUTHORIZED);
    }

    fn redirect_to_same_page_without_query_param(
        uri: &Uri,
        query: &mut HashMap<String, String>,
        cookies: PrivateCookieJar,
    ) -> Result<(PrivateCookieJar, Redirect), StatusCode> {
        query.remove(REFRESH_TOKEN_QUERY_PARAM_NAME);
        // TODO: Check if we need to URL-encode keys and values or if they are still encoded.
        let query_segs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let redirect_to = if query_segs.is_empty() {
            uri.path().to_string()
        } else {
            format!("{}?{}", uri.path(), query_segs.join("&"))
        };
        let redirect_to = Uri::from_str(&redirect_to).map_err(|err| {
            error(format!("{err}"));
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        debug!("Redirecting to <{redirect_to}> from <{uri}>…");
        Ok((
            cookies,
            Redirect::to(redirect_to.path().to_string().as_str()),
        ))
    }

    if let Some(token) = token {
        if token.profiles().contains(&"*".to_owned()) && !force {
            // NOTE: If a super admin generates an access link and accidentally opens it,
            //   they loose their super admin profile. Then we must regenerate a super admin
            //   access link and send it to the super admin's device, which increases the potential
            //   for such a sensitive link to be intercepted. As a safety measure, we don't do anything
            //   if a super admin uses a refresh token link.
            return redirect_to_same_page_without_query_param(req.uri(), &mut query, cookies)
                .map(Either::E2);
        }
    }

    trace!("Baking new biscuit from refresh token");
    let block_0 = refresh_biscuit.print_block_source(0).unwrap();
    let mut builder = Biscuit::builder();
    builder.add_code(block_0).unwrap();
    let new_biscuit = match builder.build(&ROOT_KEY) {
        Ok(biscuit) => biscuit,
        Err(err) => {
            error(format!("Error: Could not append block to biscuit: {err}"));
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        },
    };
    debug!("Successfully created new biscuit from refresh token");

    // Save token to a HTTP Cookie
    let cookies = add_cookie(&new_biscuit, cookies)?;

    // Redirect to the same page without the refresh token query param
    redirect_to_same_page_without_query_param(req.uri(), &mut query, cookies).map(Either::E2)
}

fn merge_biscuits(
    b1: &Biscuit,
    b2: &Biscuit,
) -> Option<Biscuit> {
    let source = b1.authorizer().unwrap().dump_code();
    let new_code = b2.authorizer().unwrap().dump_code();

    let mut builder = Biscuit::builder();
    builder.add_code(source).unwrap();
    builder.add_code(new_code).unwrap();
    match builder.build(&ROOT_KEY) {
        Ok(b) => Some(b),
        Err(err) => {
            debug!("Error: Could not append block to biscuit: {}", err);
            None
        },
    }
}
