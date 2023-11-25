#![feature(proc_macro_hygiene, decl_macro, never_type, exit_status_error)]

#[macro_use] extern crate rocket;

mod config;
mod generate;
mod helpers;
mod keys_reader;
mod object_reader;

extern crate time;
use biscuit::builder::{Fact, Term};
use object_reader::ObjectReader;
use rocket::http::hyper::header::{Location, CacheControl, Pragma, CacheDirective};
use time::Duration;
use rocket::http::{Status, Cookie, Cookies, SameSite, RawStr};
use rocket::http::uri::Origin;
use rocket::{Request, request, Outcome, Response};
use rocket::request::FromRequest;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use std::fmt;
use std::io::Cursor;
use std::time::SystemTime;
use std::path::{PathBuf, Path};
use std::process::exit;
use tracing::{debug, error, trace};
#[macro_use]
extern crate lazy_static;
extern crate biscuit_auth as biscuit;
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

fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber.");

    // env_logger::init();
    // env_logger::Builder::new()
    //     .target(env_logger::Target::Stdout)
    //     .init();

    if let Err(err) = throwing_main() {
        error!("Error: {}", err);
        exit(1);
    }
}

fn throwing_main() -> Result<(), Error> {
    // Generate the website
    generate_website_if_needed(&WebsiteId::default())
        .map_err(Error::WebsiteGenerationError)?;

    // Generate Orangutan data files
    generate_data_files_if_needed()
        .map_err(Error::CannotGenerateDataFiles)?;

    rocket::ignite()
        .mount("/", routes![
            get_index,
            clear_cookies,
            handle_refresh_token,
            handle_request_authenticated,
            handle_request,
            get_user_info,
        ])
        .launch();

    Ok(())
}

#[get("/")]
fn get_index<'a>(origin: &Origin) -> Response<'a> {
    _handle_request(origin, None)
        .unwrap_or_else(|e| e)
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
fn clear_cookies(mut cookies: Cookies) -> &str {
    for cookie in cookies.iter().map(Clone::clone).collect::<Vec<_>>() {
        cookies.remove(cookie.clone());
    }

    "Success"
}

#[get("/<_path..>")]
fn handle_refresh_token<'a>(
    _path: PathBuf,
    origin: &Origin,
    cookies: Cookies,
    refreshed_token: RefreshedToken,
) -> Response<'a> {
    // Save token to a Cookie if necessary
    add_cookie_if_necessary(&refreshed_token.0, cookies);

    // Redirect to the same page without the refresh token query param
    let mut origin = origin.clone();
    // FIXME: Do not clear the whole query
    origin.clear_query();
    Response::build()
        .status(Status::Found) // 302 Found (Temporary Redirect)
        .header(Location(origin.to_string()))
        // Add Cache-Control and Pragma headers to prevent caching
        .header(CacheControl(vec![CacheDirective::NoCache]))
        .header(Pragma::NoCache)
        .finalize()
}

#[get("/<_path..>", rank = 2)]
fn handle_request_authenticated<'a>(
    _path: PathBuf,
    origin: &Origin,
    token: Token,
    cookies: Cookies
) -> Response<'a> {
    add_cookie_if_necessary(&token, cookies);
    _handle_request(origin, Some(&token.biscuit))
        .unwrap_or_else(|e| e)
}

#[get("/<_path..>", rank = 3)]
fn handle_request<'a>(_path: PathBuf, origin: &'a Origin<'a>) -> Response<'a> {
    _handle_request(origin, None)
        .unwrap_or_else(|e| e)
}

fn _handle_request<'a>(
    origin: &Origin<'_>,
    biscuit: Option<&Biscuit>
) -> Result<Response<'a>, Response<'a>> {
    // FIXME: Handle error
    let path = decode(origin.path()).unwrap().into_owned();
    trace!("GET {}", &path);

    let user_profiles: Vec<String> = biscuit
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

    let object_reader = <dyn ObjectReader>::detect();

    let stored_objects: Vec<String> = match object_reader.list_objects(&path, &website_id) {
        Ok(o) => o,
        Err(err) => {
            error!("Error when listing objects matching '{}': {}", &path, err);
            return Err(not_found())
        }
    };
    let Some(object_key) = matching_files(&path, &stored_objects).first().map(|o| o.to_owned()) else {
        error!("No file matching '{}' found in stored objects", &path);
        return Err(not_found())
    };

    let allowed_profiles = allowed_profiles(&object_key);
    let Some(allowed_profiles) = allowed_profiles else {
        // If allowed profiles is empty, it means it's a static file
        trace!("File <{}> did not explicitly allow profiles, serving static file", &path);
        return object_reader.read_object(&object_key, &website_id)
            .map(|data| {
                Response::build().sized_body(Cursor::new(data)).finalize()
            })
            .ok_or(not_found());
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
        return Err(not_found())
    }

    match object_reader.read_object(object_key, &website_id) {
        Some(data) => Ok(Response::build()
            .sized_body(Cursor::new(data))
            .finalize()
        ),
        None => Err(not_found()),
    }
}

fn allowed_profiles<'a>(path: &String) -> Option<Vec<String>> {
    let path = path.rsplit_once("@").unwrap_or((path, "")).0;
    let data_file = data_file(&Path::new(path).to_path_buf());
    read_allowed(&data_file)
}

fn not_found<'a>() -> Response<'a> {
    let object_reader = <dyn ObjectReader>::detect();

    match object_reader.read_object(&NOT_FOUND_FILE, &WebsiteId::default()) {
        Some(data) => Response::build()
            .sized_body(Cursor::new(data))
            .finalize(),
        None => Response::build()
            .status(Status::NotFound)
            // TODO: Re-enable Basic authentication
            // .raw_header("WWW-Authenticate", "Basic realm=\"This page is protected. Please log in.\"")
            .sized_body(Cursor::new("This page doesn't exist or you are not allowed to see it."))
            .finalize(),
    }
}

struct Token {
    biscuit: Biscuit,
    should_save: bool,
}

#[derive(Debug)]
enum TokenError {
    // TODO: Re-enable Basic authentication
    // Invalid,
}

impl<'a, 'r> FromRequest<'a, 'r> for Token {
    type Error = TokenError;

    fn from_request(request: &'a Request<'r>) -> request::Outcome<Self, Self::Error> {
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
        if let Some(token) = request.get_query_value::<String>(TOKEN_QUERY_PARAM_NAME).and_then(Result::ok) {
            debug!("Found token query param");
            process_token(&token, "token query param", &mut biscuit, &mut should_save);
        }

        match biscuit {
            Some(biscuit) => Outcome::Success(Token { biscuit, should_save }),
            None => Outcome::Forward(()),
        }
    }
}

struct RefreshedToken(Token);

impl<'a, 'r> FromRequest<'a, 'r> for RefreshedToken {
    type Error = !;

    fn from_request(request: &'a Request<'r>) -> request::Outcome<Self, Self::Error> {
        let Some(refresh_token) = request.get_query_value::<String>(REFRESH_TOKEN_QUERY_PARAM_NAME) else {
            debug!("Refresh token query param not found.");
            return Outcome::Forward(())
        };
        let Ok(mut refresh_token) = refresh_token else {
            debug!("Error: Refresh token query param could not be decoded as `String`.");
            return Outcome::Forward(())
        };

        debug!("Found refresh token query param");

        // If query contains `force=true`, `force` or `force=<anything>`, don't search for an existing token.
        // If instead it contains `force=false` or `force` is not present, search for a token to augment.
        let token = if should_force_token_refresh(request.get_query_value::<bool>(FORCE_QUERY_PARAM_NAME)) {
            None
        } else {
            Token::from_request(request).succeeded()
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
                    return Outcome::Forward(())
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
                            Outcome::Forward(())
                        }
                    },
                    (_, Err(err)) => {
                        debug!("Error: Could not append block to biscuit: {}", err);
                        Outcome::Forward(())
                    },
                }
            },
            Err(err) => {
                debug!("Error decoding biscuit from base64: {}", err);
                Outcome::Forward(())
            },
        }
    }
}

fn should_force_token_refresh(query_param_value: Option<Result<bool, &RawStr>>) -> bool {
    query_param_value.is_some_and(|v| v.unwrap_or(true))
}

fn add_cookie_if_necessary(token: &Token, mut cookies: Cookies) {
    if token.should_save {
        match token.biscuit.to_base64() {
            Ok(base64) => {
                cookies.add(Cookie::build(TOKEN_COOKIE_NAME, base64)
                    .path("/")
                    .max_age(Duration::days(365 * 5))
                    .http_only(true)
                    .secure(true)
                    .same_site(SameSite::Strict)
                    .finish());
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

#[derive(Debug)]
enum Error {
    WebsiteGenerationError(generate::Error),
    CannotGenerateDataFiles(generate::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::WebsiteGenerationError(err) => write!(f, "Website generation error: {err}"),
            Error::CannotGenerateDataFiles(err) => write!(f, "Could not generate data files: {err}"),
        }
    }
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
        assert_eq!(should_force_token_refresh(Some(Err(RawStr::from_str("yes")))), true);
        assert_eq!(should_force_token_refresh(Some(Err(RawStr::from_str("no")))), true);
        assert_eq!(should_force_token_refresh(Some(Err(RawStr::from_str("")))), true);
    }
}
