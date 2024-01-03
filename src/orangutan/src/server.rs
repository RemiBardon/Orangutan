mod config;
mod generate;
mod helpers;
mod keys_reader;
mod object_reader;

use biscuit::builder::{Fact, Term};
use object_reader::{ObjectReader, ReadObjectResponse};
use rocket::fairing::AdHoc;
use rocket::{Either, post, Responder};
use rocket::form::Errors;
use rocket::http::CookieJar;
use rocket::http::{Status, Cookie, SameSite};
use rocket::http::uri::Origin;
use rocket::response::status::{BadRequest, NotFound};
use rocket::{Request, request, get, routes, catch, catchers, State};
use rocket::response::Redirect;
use rocket::request::FromRequest;
use rocket::outcome::Outcome;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use std::{fmt, fs, io};
use std::time::SystemTime;
use std::path::{PathBuf, Path};
use std::process::exit;
use tracing::{debug, error, trace};
use time::Duration;
use lazy_static::lazy_static;
use biscuit_auth as biscuit;
use biscuit::Biscuit;
use biscuit::macros::authorizer;
use urlencoding::decode;

use crate::config::*;
use crate::generate::*;
use crate::helpers::*;
use crate::keys_reader::KeysReader;

lazy_static! {
    static ref ROOT_KEY: biscuit::KeyPair = {
        let keys_reader = <dyn KeysReader>::detect();
        match keys_reader.get_root_biscuit_key() {
            Ok(public_key) => public_key,
            Err(err) => {
                error!("Error generating root Biscuit key: {}", err);
                exit(1);
            }
        }
    };
}

#[rocket::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber.");

    // env_logger::init();
    // env_logger::Builder::new()
    //     .target(env_logger::Target::Stdout)
    //     .init();

    if let Err(err) = throwing_main().await {
        error!("Error: {}", err);
        exit(1);
    }
}

async fn throwing_main() -> Result<(), Box<dyn std::error::Error>> {
    let rocket = rocket::build()
        .mount("/", routes![
            clear_cookies,
            handle_refresh_token,
            handle_request_authenticated,
            handle_request,
            get_user_info,
            update_content_github,
        ])
        .register("/", catchers![not_found])
        .manage(ObjectReader::new())
        .attach(AdHoc::on_liftoff("Liftoff website generation", |rocket| Box::pin(async move {
            if let Err(err) = liftoff() {
                error!("Error: {}", err);
                rocket.shutdown().notify();
            }
        })))
        .launch()
        .await?;

    Ok(())
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
            biscuit.authorizer().map_or_else(|e| format!("Error: {}", e).to_string(), |a| a.dump_code()),
        ),
        None => "Not authenticated".to_string(),
    }
}

#[get("/clear-cookies")]
fn clear_cookies(cookies: &CookieJar<'_>) -> &'static str {
    for cookie in cookies.iter().map(Clone::clone).collect::<Vec<_>>() {
        cookies.remove(cookie.clone());
    }

    "Success"
}

#[get("/<_path..>")]
fn handle_refresh_token(
    _path: PathBuf,
    origin: &Origin,
    cookies: &CookieJar<'_>,
    refreshed_token: RefreshedToken,
) -> Redirect {
    // Save token to a Cookie if necessary
    add_cookie_if_necessary(&refreshed_token.0, cookies);

    // Redirect to the same page without the refresh token query param
    let mut origin = origin.clone();
    // FIXME: Do not clear the whole query
    origin.clear_query();
    Redirect::found(origin.path().to_string())
}

#[get("/<_path..>", rank = 2)]
async fn handle_request_authenticated(
    _path: PathBuf,
    origin: &Origin<'_>,
    token: Token,
    cookies: &CookieJar<'_>,
    object_reader: &State<ObjectReader>,
) -> Result<Either<ReadObjectResponse, NotFound<()>>, object_reader::Error> {
    add_cookie_if_necessary(&token, cookies);
    _handle_request(origin, Some(token.biscuit), object_reader).await
}

#[get("/<_path..>", rank = 3)]
async fn handle_request(
    _path: PathBuf,
    origin: &Origin<'_>,
    object_reader: &State<ObjectReader>,
) -> Result<Either<ReadObjectResponse, NotFound<()>>, object_reader::Error> {
    _handle_request(origin, None, object_reader).await
}

/// TODO: [Validate webhook deliveries](https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries#validating-webhook-deliveries)
#[post("/update-content/github")]
fn update_content_github() -> Result<(), Error> {
    // Update repository
    pull_repository()
        .map_err(Error::CannotPullOutdatedRepository)?;

    // Remove outdated websites
    fs::remove_dir_all(DEST_DIR.as_path())
        .map_err(Error::CannotDeleteOutdatedWebsites)?;

    // Pre-generate default website as we will access it at some point anyway
    generate_default_website()
        .map_err(Error::WebsiteGenerationError)?;

    Ok(())
}

#[post("/update-content/<source>")]
fn update_content_other(
    source: &str,
) -> BadRequest<String> {
    BadRequest(format!("Source '{source}' is not supported."))
}

#[catch(404)]
fn not_found() -> &'static str {
    "This page doesn't exist or you are not allowed to see it."
}

async fn _handle_request<'r>(
    origin: &Origin<'_>,
    biscuit: Option<Biscuit>,
    object_reader: &State<ObjectReader>,
) -> Result<Either<ReadObjectResponse, NotFound<()>>, object_reader::Error> {
    // FIXME: Handle error
    let path = decode(origin.path().as_str()).unwrap().into_owned();
    trace!("GET {}", &path);

    let user_profiles: Vec<String> = biscuit.as_ref()
        .map(|b| b
            .authorizer().unwrap()
            .query_all("data($name) <- profile($name)").unwrap()
            .iter().map(|f: &Fact|
                match f.predicate.terms.get(0).unwrap() {
                    Term::Str(s) => s.clone(),
                    t => panic!("Term {t} should be of type String"),
                }
            )
            .collect()
        )
        .unwrap_or_default();
    let website_id = WebsiteId::from(&user_profiles);

    let stored_objects: Vec<String> = object_reader.list_objects(&path, &website_id)
        .map_err(|err| {
            error!("Error when listing objects matching '{}': {}", &path, err);
            err
        })?;
    let Some(object_key) = matching_files(&path, &stored_objects).first().map(|o| o.to_owned()) else {
        error!("No file matching '{}' found in stored objects", &path);
        return Ok(Either::Right(NotFound(())))
    };

    let allowed_profiles = allowed_profiles(&object_key);
    let Some(allowed_profiles) = allowed_profiles else {
        // If allowed profiles is empty, it means it's a static file
        trace!("File <{}> did not explicitly allow profiles, serving static file", &path);

        return Ok(Either::Left(object_reader.read_object(&object_key, &website_id).await))
    };
    debug!(
        "Page <{}> can be read by {}",
        &path,
        allowed_profiles.iter()
            .map(|p| format!("'{}'", p))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut profile: Option<String> = None;
    for allowed_profile in allowed_profiles {
        if allowed_profile == DEFAULT_PROFILE {
            profile = Some(allowed_profile);
        } else if let Some(ref biscuit) = biscuit {
            let authorizer = authorizer!(r#"
            operation("read");
            right({p}, "read");
            right("*", "read");

            allow if
              operation($op),
              profile($p),
              right($p, $op);
            "#, p = allowed_profile.clone());
            if biscuit.authorize(&authorizer).is_ok() {
                profile = Some(allowed_profile);
            }
        }
    }
    if profile.is_none() {
        debug!("No profile allowed in token");
        return Ok(Either::Right(NotFound(())))
    }

    Ok(Either::Left(object_reader.read_object(object_key, &website_id).await))
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
    should_save: bool,
}

#[derive(Debug)]
enum TokenError {
    // TODO: Re-enable Basic authentication
    // Invalid,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for Token {
    type Error = TokenError;

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        let mut biscuit: Option<Biscuit> = None;
        let mut should_save: bool = false;

        fn process_token(
            token: &str,
            token_source: &str,
            biscuit: &mut Option<Biscuit>,
            should_save: &mut bool
        ) {
            // Because tokens can be passed as URL query params,
            // they might have the "=" padding characters remove.
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
        if let Some(cookie) = request.cookies().get(TOKEN_COOKIE_NAME) {
            debug!("Found token cookie");
            let token: &str = cookie.value();
            // NOTE: We don't want to send a `Set-Cookie` header after finding a token in a cookie,
            //   so let's create a temporary value which prevents `process_token` from changing
            //   the global `should_save` value.
            let mut should_save = false;
            process_token(token, "token cookie", &mut biscuit, &mut should_save);
        }

        // Check authorization headers
        let authorization_headers: Vec<&str> = request.headers().get("Authorization").collect();
        debug!("{} 'Authorization' headers provided", authorization_headers.len());
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
        if let Some(token) = request.query_value::<String>(TOKEN_QUERY_PARAM_NAME).and_then(Result::ok) {
            debug!("Found token query param");
            process_token(&token, "token query param", &mut biscuit, &mut should_save);
        }

        match biscuit {
            Some(biscuit) => Outcome::Success(Token { biscuit, should_save }),
            None => Outcome::Forward(Status::Unauthorized),
        }
    }
}

struct RefreshedToken(Token);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for RefreshedToken {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        let Some(refresh_token) = request.query_value::<String>(REFRESH_TOKEN_QUERY_PARAM_NAME) else {
            debug!("Refresh token query param not found.");
            return Outcome::Forward(Status::Unauthorized)
        };
        let Ok(mut refresh_token) = refresh_token else {
            debug!("Error: Refresh token query param could not be decoded as `String`.");
            return Outcome::Forward(Status::Unauthorized)
        };

        debug!("Found refresh token query param");

        // If query contains `force=true`, `force` or `force=<anything>`, don't search for an existing token.
        // If instead it contains `force=false` or `force` is not present, search for a token to augment.
        let token = if should_force_token_refresh(request.query_value::<bool>(FORCE_QUERY_PARAM_NAME)) {
            None
        } else {
            Token::from_request(request).await.succeeded()
        };

        refresh_token = decode(&refresh_token).unwrap().to_string();
        // Because tokens can be passed as URL query params,
        // they might have the "=" padding characters remove.
        // We need to add them back.
        let refresh_token = add_padding(&refresh_token);
        match Biscuit::from_base64(refresh_token, ROOT_KEY.public()) {
            Ok(refresh_biscuit) => {
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
                    return Outcome::Forward(Status::Unauthorized)
                }

                trace!("Baking biscuit from refresh token");
                let source = refresh_biscuit.print_block_source(0).unwrap();
                let mut builder = Biscuit::builder();
                builder.add_code(source).unwrap();
                match (token, builder.build(&ROOT_KEY)) {
                    (None, Ok(new_biscuit)) => {
                        trace!("Created new biscuit from refresh token");
                        Outcome::Success(RefreshedToken(Token {
                            biscuit: new_biscuit,
                            should_save: true,
                        }))
                    },
                    (Some(Token { biscuit: acc, .. }), Ok(b)) => {
                        trace!("Making bigger biscuit from refresh token");
                        if let Some(new_biscuit) = merge_biscuits(&acc, &b) {
                            Outcome::Success(RefreshedToken(Token {
                                biscuit: new_biscuit,
                                should_save: true,
                            }))
                        } else {
                            Outcome::Forward(Status::InternalServerError)
                        }
                    },
                    (_, Err(err)) => {
                        debug!("Error: Could not append block to biscuit: {}", err);
                        Outcome::Forward(Status::InternalServerError)
                    },
                }
            },
            Err(err) => {
                debug!("Error decoding biscuit from base64: {}", err);
                Outcome::Forward(Status::Unauthorized)
            },
        }
    }
}

fn should_force_token_refresh(query_param_value: Option<Result<bool, Errors<'_>>>) -> bool {
    query_param_value.is_some_and(|v| v.unwrap_or(true))
}

fn add_cookie_if_necessary(token: &Token, cookies: &CookieJar<'_>) {
    if token.should_save {
        match token.biscuit.to_base64() {
            Ok(base64) => {
                cookies.add(Cookie::build((TOKEN_COOKIE_NAME, base64))
                    .path("/")
                    .max_age(Duration::days(365 * 5))
                    .http_only(true)
                    .secure(true)
                    .same_site(SameSite::Strict));
            },
            Err(err) => {
                error!("Error setting token cookie: {}", err);
            },
        }
    }
}

fn merge_biscuits(b1: &Biscuit, b2: &Biscuit) -> Option<Biscuit> {
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

fn matching_files<'a>(query: &str, stored_objects: &'a Vec<String>) -> Vec<&'a String> {
    stored_objects.into_iter()
        .filter(|p| {
            let query = query.strip_suffix("index.html").unwrap_or(query);
            let Some(mut p) = p.strip_prefix(query) else {
                return false
            };
            p = p.trim_start_matches('/');
            p = p.strip_prefix("index.html").unwrap_or(p);
            return p.is_empty() || p.starts_with('@')
        })
        .collect()
}

fn add_padding(base64_string: &str) -> String {
    // If the base64 string is already padded, don't do anything.
    if base64_string.ends_with("=") {
        return base64_string.to_string()
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
    CannotDeleteOutdatedWebsites(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::WebsiteGenerationError(err) => write!(f, "Website generation error: {err}"),
            Error::CannotPullOutdatedRepository(err) => write!(f, "Cannot pull outdated repository: {err}"),
            Error::CannotDeleteOutdatedWebsites(err) => write!(f, "Cannot delete outdated websites: {err}"),
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
        ].into_iter().map(|p| p.to_string()).collect::<Vec<String>>();

        assert_eq!(
            matching_files("", &stored_objects),
            vec![
                "/index.html@_default",
            ]
        );
        assert_eq!(
            matching_files("/", &stored_objects),
            vec![
                "/index.html@_default",
            ]
        );
        assert_eq!(
            matching_files("/index.html", &stored_objects),
            vec![
                "/index.html@_default",
            ]
        );

        assert_eq!(
            matching_files("/whatever", &stored_objects),
            vec![
                "/whatever/index.html@friends",
                "/whatever/index.html@family",
            ]
        );
        assert_eq!(
            matching_files("/whatever/", &stored_objects),
            vec![
                "/whatever/index.html@friends",
                "/whatever/index.html@family",
            ]
        );
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
        assert_eq!(
            matching_files("/whatever/a/b.html", &stored_objects),
            vec![
                "/whatever/a/b.html@_default",
            ]
        );
    }

    #[test]
    fn test_other_extensions() {
        let stored_objects = vec![
            "/style.css@_default",
            "/anything.custom@friends",
            "/anything.custom@family",
        ].into_iter().map(|p| p.to_string()).collect::<Vec<String>>();

        assert_eq!(
            matching_files("/style.css", &stored_objects),
            vec![
                "/style.css@_default",
            ]
        );
        assert_eq!(
            matching_files("/anything.custom", &stored_objects),
            vec![
                "/anything.custom@friends",
                "/anything.custom@family",
            ]
        );
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

    #[test]
    fn test_should_force_token_refresh() {
        assert_eq!(should_force_token_refresh(None), false);
        assert_eq!(should_force_token_refresh(Some(Ok(true))), true);
        assert_eq!(should_force_token_refresh(Some(Ok(false))), false);
        assert_eq!(should_force_token_refresh(Some(Err(Errors::new().with_name("yes")))), true);
        assert_eq!(should_force_token_refresh(Some(Err(Errors::new().with_name("no")))), true);
        assert_eq!(should_force_token_refresh(Some(Err(Errors::new().with_name("")))), true);
    }
}
