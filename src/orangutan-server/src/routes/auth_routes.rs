use std::{collections::HashSet, sync::RwLock, time::SystemTime};

use biscuit_auth::{macros::authorizer, Biscuit};
use lazy_static::lazy_static;
use rocket::{
    get,
    http::{uri::Origin, CookieJar, Status},
    response::Redirect,
    routes, Route,
};
use tracing::{debug, trace};

use crate::{
    config::*,
    error,
    request_guards::Token,
    util::{add_cookie, add_padding},
};

lazy_static! {
    pub static ref REVOKED_TOKENS: RwLock<HashSet<Vec<u8>>> = RwLock::default();
}

pub(super) fn routes() -> Vec<Route> {
    routes![handle_refresh_token]
}

#[get("/<_..>?<refresh_token>&<force>")]
fn handle_refresh_token(
    origin: &Origin,
    cookies: &CookieJar<'_>,
    refresh_token: &str,
    token: Option<Token>,
    force: Option<bool>,
) -> Result<Redirect, Status> {
    // URL-decode the string.
    let mut refresh_token: String = urlencoding::decode(refresh_token).unwrap().to_string();

    // Because tokens can be passed as URL query params,
    // they might have the "=" padding characters removed.
    // We need to add them back.
    refresh_token = add_padding(&refresh_token);

    let refresh_biscuit: Biscuit = match Biscuit::from_base64(refresh_token, ROOT_KEY.public()) {
        Ok(biscuit) => biscuit,
        Err(err) => {
            debug!("Error decoding biscuit from base64: {}", err);
            return Err(Status::Unauthorized);
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
        return Err(Status::Forbidden);
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
        return Err(Status::Unauthorized);
    }

    fn redirect_to_same_page_without_query_param(origin: &Origin) -> Result<Redirect, Status> {
        let query_segs: Vec<String> = origin
            .query()
            .unwrap()
            .raw_segments()
            .filter(|s| !s.starts_with(format!("{REFRESH_TOKEN_QUERY_PARAM_NAME}=").as_str()))
            .map(ToString::to_string)
            .collect();
        match Origin::parse_owned(format!("{}?{}", origin.path(), query_segs.join("&"))) {
            Ok(redirect_to) => {
                debug!("Redirecting to <{redirect_to}> from <{origin}>…");
                Ok(Redirect::found(redirect_to.path().to_string()))
            },
            Err(err) => {
                error(format!("{err}"));
                Err(Status::InternalServerError)
            },
        }
    }

    if let Some(token) = token {
        if token.profiles().contains(&"*".to_owned()) && !force.unwrap_or(false) {
            // NOTE: If a super admin generates an access link and accidentally opens it,
            //   they loose their super admin profile. Then we must regenerate a super admin
            //   access link and send it to the super admin's device, which increases the potential
            //   for such a sensitive link to be intercepted. As a safety measure, we don't do anything
            //   if a super admin uses a refresh token link.
            return redirect_to_same_page_without_query_param(origin);
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
            return Err(Status::InternalServerError);
        },
    };
    debug!("Successfully created new biscuit from refresh token");

    // Save token to a HTTP Cookie
    add_cookie(&new_biscuit, cookies);

    // Redirect to the same page without the refresh token query param
    redirect_to_same_page_without_query_param(origin)
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
