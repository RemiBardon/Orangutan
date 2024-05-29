use std::ops::Deref;

use biscuit_auth::Biscuit;
use rocket::{http::Status, outcome::Outcome, request, request::FromRequest, Request};
use tracing::{debug, trace};

use crate::{
    config::*,
    util::{add_cookie, add_padding, profiles},
};

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

#[derive(Debug)]
pub enum TokenError {
    // TODO: Re-enable Basic authentication
    // Invalid,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for Token {
    type Error = TokenError;

    async fn from_request(req: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
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
        if let Some(cookie) = req.cookies().get(TOKEN_COOKIE_NAME) {
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
        let authorization_headers: Vec<&str> = req.headers().get("Authorization").collect();
        debug!(
            "{} 'Authorization' headers provided",
            authorization_headers.len()
        );
        for authorization in authorization_headers {
            if authorization.starts_with("Bearer ") {
                debug!("Bearer Authorization provided");
                let token: &str = authorization.trim_start_matches("Bearer ");
                process_token(token, "Bearer token", &mut biscuit, &mut should_save);
            } else if authorization.starts_with("Basic ") {
                debug!("Basic authentication disabled");
            }
        }

        // Check query params
        if let Some(token) = req
            .query_value::<String>(TOKEN_QUERY_PARAM_NAME)
            .and_then(Result::ok)
        {
            debug!("Found token query param");
            process_token(&token, "token query param", &mut biscuit, &mut should_save);
        }

        match biscuit {
            Some(biscuit) => {
                if should_save {
                    add_cookie(&biscuit, req.cookies());
                }
                Outcome::Success(Token { biscuit })
            },
            None => Outcome::Forward(Status::Unauthorized),
        }
    }
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
