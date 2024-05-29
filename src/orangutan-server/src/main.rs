mod config;

use std::{
    ops::Deref,
    path::Path,
    process::exit,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use biscuit::{
    builder::{Fact, Term},
    macros::authorizer,
    Biscuit,
};
use biscuit_auth as biscuit;
use chrono::{DateTime, Utc};
use lazy_static::lazy_static;
use object_reader::{ObjectReader, ReadObjectResponse};
use orangutan_helpers::{
    data_file,
    generate::{self, *},
    read_allowed,
    readers::{keys_reader::*, object_reader},
    website_id::WebsiteId,
};
use rocket::{
    catch, catchers,
    fairing::AdHoc,
    fs::NamedFile,
    get,
    http::{uri::Origin, Cookie, CookieJar, SameSite, Status},
    outcome::Outcome,
    post, request,
    request::FromRequest,
    response::{self, status::BadRequest, Redirect, Responder},
    routes, Request, State,
};
use time::Duration;
use tracing::{debug, error, trace, Level};
use tracing_subscriber::FmtSubscriber;

use crate::config::*;

lazy_static! {
    static ref ROOT_KEY: biscuit::KeyPair = {
        let keys_reader = <dyn KeysReader>::detect();
        match keys_reader.get_root_biscuit_key() {
            Ok(public_key) => public_key,
            Err(err) => {
                error!("Error generating root Biscuit key: {err}");
                exit(1);
            },
        }
    };
    /// A list of runtime errors, used to show error logs in an admin page
    /// without having to open the cloud hosting provider's logs.
    ///
    /// // NOTE: `Arc` prevents race conditions
    static ref ERRORS: Arc<Mutex<Vec<(DateTime<Utc>, String)>>> = Arc::new(Mutex::new(Vec::new()));
}

#[rocket::launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", routes![
            clear_cookies,
            handle_refresh_token,
            handle_request,
            get_user_info,
            update_content_github,
            update_content_other,
            errors,
        ])
        .register("/", catchers![unauthorized, not_found])
        .manage(ObjectReader::new())
        .attach(AdHoc::on_liftoff("Tracing subsciber", |_| {
            Box::pin(async move {
                let subscriber = FmtSubscriber::builder()
                    .with_max_level(Level::TRACE)
                    .finish();
                tracing::subscriber::set_global_default(subscriber)
                    .expect("Failed to set tracing subscriber.");
            })
        }))
        .attach(AdHoc::on_liftoff("Website generation", |rocket| {
            Box::pin(async move {
                if let Err(err) = liftoff() {
                    // We drop the error to get a Rocket-formatted panic.
                    drop(err);
                    rocket.shutdown().notify();
                }
            })
        }))
}

fn liftoff() -> Result<(), Error> {
    clone_repository().map_err(Error::WebsiteGenerationError)?;
    generate_default_website().map_err(Error::WebsiteGenerationError)?;
    Ok(())
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

#[get("/clear-cookies")]
fn clear_cookies(cookies: &CookieJar<'_>) -> &'static str {
    for cookie in cookies.iter().map(Clone::clone) {
        cookies.remove(cookie);
    }

    "Success"
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

#[get("/<_..>?<refresh_token>")]
fn handle_refresh_token(
    origin: &Origin,
    cookies: &CookieJar<'_>,
    refresh_token: &str,
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

#[get("/<_..>")]
async fn handle_request(
    origin: &Origin<'_>,
    token: Option<Token>,
    object_reader: &State<ObjectReader>,
) -> Result<Option<ReadObjectResponse>, Error> {
    // FIXME: Handle error
    let path = urlencoding::decode(origin.path().as_str())
        .unwrap()
        .into_owned();
    trace!("GET {}", &path);

    let user_profiles: Vec<String> = token.as_ref().map(Token::profiles).unwrap_or_default();
    debug!("User has profiles {user_profiles:?}");
    let website_id = WebsiteId::from(&user_profiles);

    let stored_objects: Vec<String> =
        object_reader
            .list_objects(&path, &website_id)
            .map_err(|err| Error::CannotListObjects {
                path: path.to_owned(),
                err,
            })?;
    let Some(object_key) = matching_files(&path, &stored_objects)
        .first()
        .map(|o| o.to_owned())
    else {
        error(format!(
            "No file matching '{}' found in stored objects",
            &path
        ));
        return Ok(None);
    };

    let allowed_profiles = allowed_profiles(&object_key);
    let Some(allowed_profiles) = allowed_profiles else {
        // If allowed profiles is empty, it means it's a static file
        trace!(
            "File <{}> did not explicitly allow profiles, serving static file",
            &path
        );

        return Ok(Some(
            object_reader.read_object(&object_key, &website_id).await,
        ));
    };
    debug!(
        "Page <{}> can be read by {}",
        &path,
        allowed_profiles
            .iter()
            .map(|p| format!("'{}'", p))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut profile: Option<String> = None;
    let biscuit = token.map(|t| t.biscuit);
    for allowed_profile in allowed_profiles {
        trace!("Checking if profile '{allowed_profile}' exists in token…");
        if allowed_profile == DEFAULT_PROFILE {
            profile = Some(allowed_profile);
        } else if let Some(ref biscuit) = biscuit {
            let authorizer = authorizer!(
                r#"
                operation("read");
                time({now});
                right({p}, "read");
                right("*", "read");

                allow if
                operation($op),
                profile($p),
                right($p, $op);
                "#,
                p = allowed_profile.clone(),
                now = SystemTime::now()
            );
            trace!(
                "Running authorizer '{}' on '{}'…",
                authorizer.dump_code(),
                biscuit.authorizer().unwrap().dump_code()
            );
            if biscuit.authorize(&authorizer).is_ok() {
                profile = Some(allowed_profile);
            }
        }
    }
    if profile.is_none() {
        debug!("No profile allowed in token");
        return Ok(None);
    }

    Ok(Some(
        object_reader.read_object(object_key, &website_id).await,
    ))
}

/// TODO: [Validate webhook deliveries](https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#validating-webhook-deliveries)
#[post("/update-content/github")]
fn update_content_github() -> Result<(), Error> {
    // Update repository
    pull_repository().map_err(Error::CannotPullOutdatedRepository)?;

    // Remove outdated websites
    let state = trash_outdated_websites().map_err(Error::CannotTrashOutdatedWebsites)?;

    // Pre-generate default website as we will access it at some point anyway
    match generate_default_website().map_err(Error::WebsiteGenerationError) {
        Err(err) => {
            error(format!("{err}"));
            recover_trash(state).map_err(Error::CannotRecoverTrash)
        },
        Ok(()) => empty_trash(state).map_err(Error::CannotEmptyTrash),
    }
}

#[post("/update-content/<source>")]
fn update_content_other(source: &str) -> BadRequest<String> {
    BadRequest(format!("Source '{source}' is not supported."))
}

async fn _not_found() -> Result<NamedFile, &'static str> {
    let website_id = WebsiteId::default();
    let website_dir = match generate_website_if_needed(&website_id) {
        Ok(dir) => dir,
        Err(err) => {
            error(format!("Could not get default website directory: {}", err));
            return Err("This page doesn't exist or you are not allowed to see it.");
        },
    };
    let file_path = website_dir.join(NOT_FOUND_FILE);
    NamedFile::open(file_path.clone()).await.map_err(|err| {
        error(format!(
            "Could not read \"not found\" file at <{}>: {}",
            file_path.display(),
            err
        ));
        "This page doesn't exist or you are not allowed to see it."
    })
}

#[catch(401)]
async fn unauthorized() -> Result<NamedFile, &'static str> {
    _not_found().await
}

#[catch(404)]
async fn not_found() -> Result<NamedFile, &'static str> {
    _not_found().await
}

fn allowed_profiles<'r>(path: &String) -> Option<Vec<String>> {
    let path = path.rsplit_once("@").unwrap_or((path, "")).0;
    let data_file = data_file(&Path::new(path).to_path_buf());
    read_allowed(&data_file)
}

// #[derive(Responder)]
// #[response(status = 404)]
// struct NotFound {
//     inner: String,
// }

// impl NotFound {
//     fn new() -> Self {
//         // TODO: Re-enable Basic authentication
//         // .raw_header("WWW-Authenticate", "Basic realm=\"This page is protected. Please log in.\"")
//         NotFound {
//             inner: "This page doesn't exist or you are not allowed to see it.".to_string(),
//         }
//     }
// }

struct Token {
    biscuit: Biscuit,
}

impl Token {
    fn profiles(&self) -> Vec<String> {
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
enum TokenError {
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

fn profiles(biscuit: &Biscuit) -> Vec<String> {
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

fn add_cookie(
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

fn matching_files<'a>(
    query: &str,
    stored_objects: &'a Vec<String>,
) -> Vec<&'a String> {
    stored_objects
        .into_iter()
        .filter(|p| {
            let query = query.strip_suffix("index.html").unwrap_or(query);
            let Some(mut p) = p.strip_prefix(query) else {
                return false;
            };
            p = p.trim_start_matches('/');
            p = p.strip_prefix("index.html").unwrap_or(p);
            return p.is_empty() || p.starts_with('@');
        })
        .collect()
}

fn add_padding(base64_string: &str) -> String {
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

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("Website generation error: {0}")]
    WebsiteGenerationError(generate::Error),
    #[error("Cannot pull outdated repository: {0}")]
    CannotPullOutdatedRepository(generate::Error),
    #[error("Error when listing objects matching '{path}': {err}")]
    CannotListObjects {
        path: String,
        err: object_reader::Error,
    },
    #[error("Cannot trash outdated websites: {0}")]
    CannotTrashOutdatedWebsites(generate::Error),
    #[error("Cannot recover trash: {0}")]
    CannotRecoverTrash(generate::Error),
    #[error("Cannot empty trash: {0}")]
    CannotEmptyTrash(generate::Error),
}

#[rocket::async_trait]
impl<'r> Responder<'r, 'static> for Error {
    fn respond_to(
        self,
        _: &'r Request<'_>,
    ) -> response::Result<'static> {
        error(format!("{self}"));
        Err(Status::InternalServerError)
    }
}

fn error(err: String) {
    ERRORS.lock().unwrap().push((Utc::now(), err.to_owned()));
    error!(err);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_html() {
        let stored_objects = vec![
            "/index.html@_default",
            "/whatever/index.html@friends",
            "/whatever/index.html@family",
            "/whatever/p.html@_default",
            "/whatever/index.htmlindex.html@_default",
            "/whatever/other-page/index.html@_default",
            "/whatever/a/b.html@_default",
        ]
        .into_iter()
        .map(|p| p.to_string())
        .collect::<Vec<String>>();

        assert_eq!(matching_files("", &stored_objects), vec![
            "/index.html@_default",
        ]);
        assert_eq!(matching_files("/", &stored_objects), vec![
            "/index.html@_default",
        ]);
        assert_eq!(matching_files("/index.html", &stored_objects), vec![
            "/index.html@_default",
        ]);

        assert_eq!(matching_files("/whatever", &stored_objects), vec![
            "/whatever/index.html@friends",
            "/whatever/index.html@family",
        ]);
        assert_eq!(matching_files("/whatever/", &stored_objects), vec![
            "/whatever/index.html@friends",
            "/whatever/index.html@family",
        ]);
        assert_eq!(
            matching_files("/whatever/index.html", &stored_objects),
            vec![
                "/whatever/index.html@friends",
                "/whatever/index.html@family",
            ]
        );

        assert_eq!(
            matching_files("/whatever/a", &stored_objects),
            Vec::<&str>::new()
        );
        assert_eq!(
            matching_files("/whatever/a/b", &stored_objects),
            Vec::<&str>::new()
        );
        assert_eq!(matching_files("/whatever/a/b.html", &stored_objects), vec![
            "/whatever/a/b.html@_default",
        ]);
    }

    #[test]
    fn test_other_extensions() {
        let stored_objects = vec![
            "/style.css@_default",
            "/anything.custom@friends",
            "/anything.custom@family",
        ]
        .into_iter()
        .map(|p| p.to_string())
        .collect::<Vec<String>>();

        assert_eq!(matching_files("/style.css", &stored_objects), vec![
            "/style.css@_default",
        ]);
        assert_eq!(matching_files("/anything.custom", &stored_objects), vec![
            "/anything.custom@friends",
            "/anything.custom@family",
        ]);
    }

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
