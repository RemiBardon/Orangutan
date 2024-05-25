mod config;

use std::fmt;
use std::ops::Deref;
use std::path::Path;
use std::process::exit;
use std::time::SystemTime;

use biscuit::builder::{Fact, Term};
use biscuit::macros::authorizer;
use biscuit::Biscuit;
use biscuit_auth as biscuit;
use lazy_static::lazy_static;
use object_reader::{ObjectReader, ReadObjectResponse};
use orangutan_helpers::generate::{self, *};
use orangutan_helpers::readers::keys_reader::*;
use orangutan_helpers::readers::object_reader;
use orangutan_helpers::website_id::WebsiteId;
use orangutan_helpers::{data_file, read_allowed};
use rocket::fairing::AdHoc;
use rocket::fs::NamedFile;
use rocket::http::uri::Origin;
use rocket::http::{Cookie, CookieJar, SameSite, Status};
use rocket::outcome::Outcome;
use rocket::request::FromRequest;
use rocket::response::status::BadRequest;
use rocket::response::Redirect;
use rocket::{catch, catchers, get, post, request, routes, Request, Responder, State};
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
                error!("Error generating root Biscuit key: {}", err);
                exit(1);
            },
        }
    };
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
            error!("Error: Could not append block to biscuit: {err}");
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
        Err(e) => {
            error!("{e}");
            Err(Status::InternalServerError)
        },
    }
}

#[get("/<_..>")]
async fn handle_request(
    origin: &Origin<'_>,
    token: Option<Token>,
    object_reader: &State<ObjectReader>,
) -> Result<Option<ReadObjectResponse>, object_reader::Error> {
    let biscuit = token.map(|t| t.biscuit);

    // FIXME: Handle error
    let path = urlencoding::decode(origin.path().as_str())
        .unwrap()
        .into_owned();
    trace!("GET {}", &path);

    let user_profiles: Vec<String> = biscuit
        .as_ref()
        .map(|b| {
            b.authorizer()
                .unwrap()
                .query_all("data($name) <- profile($name)")
                .unwrap()
                .iter()
                .map(|f: &Fact| match f.predicate.terms.get(0).unwrap() {
                    Term::Str(s) => s.clone(),
                    t => panic!("Term {t} should be of type String"),
                })
                .collect()
        })
        .unwrap_or_default();
    debug!("User has profiles {user_profiles:?}");
    let website_id = WebsiteId::from(&user_profiles);

    let stored_objects: Vec<String> =
        object_reader
            .list_objects(&path, &website_id)
            .map_err(|err| {
                error!("Error when listing objects matching '{}': {}", &path, err);
                err
            })?;
    let Some(object_key) = matching_files(&path, &stored_objects)
        .first()
        .map(|o| o.to_owned())
    else {
        error!("No file matching '{}' found in stored objects", &path);
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
            error!("{err}");
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
            error!("Could not get default website directory: {}", err);
            return Err("This page doesn't exist or you are not allowed to see it.");
        },
    };
    let file_path = website_dir.join(NOT_FOUND_FILE);
    NamedFile::open(file_path.clone()).await.map_err(|err| {
        error!(
            "Could not read \"not found\" file at <{}>: {}",
            file_path.display(),
            err
        );
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
            error!("Error setting token cookie: {}", err);
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

#[derive(Debug, Responder)]
#[response(status = 500)]
enum Error {
    WebsiteGenerationError(generate::Error),
    CannotPullOutdatedRepository(generate::Error),
    CannotTrashOutdatedWebsites(generate::Error),
    CannotRecoverTrash(generate::Error),
    CannotEmptyTrash(generate::Error),
}

impl fmt::Display for Error {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Error::WebsiteGenerationError(err) => write!(f, "Website generation error: {err}"),
            Error::CannotPullOutdatedRepository(err) => {
                write!(f, "Cannot pull outdated repository: {err}")
            },
            Error::CannotTrashOutdatedWebsites(err) => {
                write!(f, "Cannot trash outdated websites: {err}")
            },
            Error::CannotRecoverTrash(err) => {
                write!(f, "Cannot recover trash: {err}")
            },
            Error::CannotEmptyTrash(err) => {
                write!(f, "Cannot empty trash: {err}")
            },
        }
    }
}

impl std::error::Error for Error {}

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
