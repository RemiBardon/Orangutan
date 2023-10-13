#![feature(proc_macro_hygiene, decl_macro, never_type)]

#[macro_use] extern crate rocket;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key};
// TODO: Re-enable Basic authentication
// use base64::{Engine as _, engine::general_purpose};
extern crate time;
use rocket::http::hyper::header::{Location, CacheControl, Pragma, CacheDirective};
use time::Duration;
use rocket::http::{Status, Cookie, Cookies, SameSite};
use rocket::http::uri::Origin;
use rocket::{Request, request, Outcome, Response};
use rocket::request::FromRequest;
use rusoto_core::{credential::StaticProvider, HttpClient, Region};
use rusoto_s3::{S3, S3Client, GetObjectRequest, ListObjectsV2Request};
use tokio::runtime::Runtime;
use std::collections::HashMap;
use std::io::{Read, Write, Cursor};
use std::time::SystemTime;
use std::{env, io, fmt, fs};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Mutex;
use log::{debug, error, trace};
#[macro_use]
extern crate lazy_static;
extern crate biscuit_auth as biscuit;
use biscuit::Biscuit;
use biscuit::macros::authorizer;
use urlencoding::decode;

const DEFAULT_PROFILE: &'static str = "_default";
const BUCKET_NAME: &'static str = "orangutan";
const ROOT_KEY_NAME: &'static str = "_biscuit-root";
const TOKEN_COOKIE_NAME: &'static str = "token";
const TOKEN_QUERY_PARAM_NAME: &'static str = "token";
const REFRESH_TOKEN_QUERY_PARAM_NAME: &'static str = "refresh_token";
// TODO: Make this a command-line argument
const DRY_RUN: bool = true;
const LOCAL: bool = true;

lazy_static! {
    static ref BASE_DIR: &'static Path = Path::new(".orangutan");
    static ref WEBSITE_DIR: PathBuf = BASE_DIR.join("website");
    static ref KEYS_DIR: PathBuf = BASE_DIR.join("keys");
    static ref KEYS: Mutex<HashMap<String, Key<Aes256Gcm>>> = Mutex::new(HashMap::new());

    static ref S3_CLIENT: S3Client = {
        let region = Region::Custom {
            name: "custom-region".to_string(),
            endpoint: env::var("S3_REGION_ENDPOINT").expect("env vars not set"),
        };
        let credentials_provider = StaticProvider::new_minimal(
            env::var("S3_KEY_ID").expect("env vars not set"),
            env::var("S3_ACCESS_KEY").expect("env vars not set"),
        );
        S3Client::new_with(
            HttpClient::new().expect("Failed to create HTTP client"),
            credentials_provider,
            region,
        )
    };

    static ref ROOT_KEY: biscuit::KeyPair = {
        match get_root_key() {
            Ok(public_key) => public_key,
            Err(err) => {
                error!("Error generating root Biscuit key: {}", err);
                exit(1);
            }
        }
    };

    static ref TOKIO_RUNTIME: Runtime = Runtime::new().unwrap();
}

#[tokio::main]
async fn main() {
    // let subscriber = FmtSubscriber::builder()
    //     .with_max_level(Level::DEBUG)
    //     .finish();

    // tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber.");

    // env_logger::init();
    // env_logger::Builder::new()
    //     .target(env_logger::Target::Stdout)
    //     .init();

    if let Err(err) = throwing_main().await {
        error!("Error: {}", err);
        exit(1);
    }
}

async fn throwing_main() -> Result<(), std::io::Error> {
    rocket::ignite()
        .mount("/", routes![
            get_index,
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

#[get("/<_path..>")]
fn handle_refresh_token<'a>(
    _path: PathBuf,
    origin: &Origin,
    mut cookies: Cookies,
    refreshed_token: RefreshedToken,
) -> Response<'a> {
    // Save token to a Cookie if necessary
    add_cookie_if_necessary(&refreshed_token.0, &mut cookies);

    // Redirect to the same page without the refresh token query param
    // FIXME: Do not clear the whole query
    let mut origin = origin.clone();
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
    mut cookies: Cookies
) -> Response<'a> {
    add_cookie_if_necessary(&token, &mut cookies);
    _handle_request(origin, Some(&token.biscuit))
}

#[get("/<_path..>", rank = 3)]
fn handle_request<'a>(_path: PathBuf, origin: &Origin) -> Response<'a> {
    _handle_request(origin, None)
}

fn _handle_request<'a>(origin: &Origin, biscuit: Option<&Biscuit>) -> Response<'a> {
    let path = decode(origin.path()).unwrap().into_owned();
    trace!("GET {}", &path);

    fn serve_file<'a>(file_path: PathBuf) -> Result<Vec<u8>, Response<'a>> {
        if let Ok(mut file) = File::open(&file_path) {
            let mut data: Vec<u8> = Vec::new();
            if let Err(err) = file.read_to_end(&mut data) {
                return Err(Response::build()
                    .sized_body(Cursor::new(format!("Dry run failed: Could not read <{}> from disk: {}", file_path.display(), err)))
                    .finalize())
            }
            return Ok(data)
        } else {
            return Err(Response::build()
                .sized_body(Cursor::new(format!("Dry run failed: Could not read <{}> from disk: Cannot open file", file_path.display())))
                .finalize())
        }
    }

    let stored_objects: Vec<String>;
    match list_objects(&path) {
        Ok(o) => stored_objects = o,
        Err(err) => {
            error!("Error when listing objects matching '{}': {}", &path, err);
            return Response::build()
                .status(Status::NotFound)
                .sized_body(Cursor::new("Object not found"))
                .finalize()
        }
    }

    let matching_files = matching_files(&path, &stored_objects);

    let allowed_profiles = matching_files.iter().flat_map(|f| f.rsplit_terminator("@").next());
    if allowed_profiles.clone().next() == None {
        // If allowed profiles is empty, it means it's a static file
        let file_path = WEBSITE_DIR.join(path.strip_prefix("/").unwrap());
        return match serve_file(file_path) {
            Ok(data) => {
                Response::build().sized_body(Cursor::new(data)).finalize()
            },
            Err(err) => {
                debug!("Error reading static file: {:?}", err);
                Response::build()
                    .status(Status::InternalServerError)
                    .sized_body(Cursor::new("Internal Server Error"))
                    .finalize()
            },
        }
    }
    debug!(
        "Page <{}> can be read by {}",
        &path,
        allowed_profiles.clone()
            .map(|p| format!("'{}'", p))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut profile: Option<&str> = None;
    for allowed_profile in allowed_profiles {
        if allowed_profile == DEFAULT_PROFILE {
            profile = Some(allowed_profile);
        } else if let Some(ref biscuit) = biscuit {
            let authorizer = authorizer!(r#"
            operation("read");
            right({allowed_profile}, "read");
            right("*", "read");

            allow if
              operation($op),
              profile($p),
              right($p, $op);
            "#);
            if biscuit.authorize(&authorizer).is_ok() {
                profile = Some(allowed_profile);
            }
        }
    }
    let Some(profile) = profile else {
        debug!("Basic authentication disabled");
        return Response::build()
            .status(Status::Unauthorized)
            // TODO: Re-enable Basic authentication
            // .raw_header("WWW-Authenticate", "Basic realm=\"This page is protected. Please log in.\"")
            .sized_body(Cursor::new("This page doesn't exist or you are not allowed to see it."))
            .finalize()
    };

    // NOTE: Cannot use `if let Some(object_key)` as it causes
    //   `#[get] can only be used on functions`.
    let object_key: &String;
    match matching_files.iter().filter(|f| f.ends_with(&format!("@{}", profile))).next() {
        Some(key) => object_key = key,
        None => {
            debug!("No object matching '{}' in {:?}", &path, stored_objects);
            return Response::build()
                .status(Status::NotFound)
                .sized_body(Cursor::new("Object not found"))
                .finalize()
        }
    }

    let s3_req = GetObjectRequest {
        bucket: BUCKET_NAME.to_string(),
        key: object_key.to_owned(),
        ..Default::default()
    };
    let data: Vec<u8>;

    if DRY_RUN || LOCAL {
        debug!("[DRY_RUN] Get object '{}' from bucket '{}'", &s3_req.key, s3_req.bucket);

        let file_path = WEBSITE_DIR.join(&s3_req.key.strip_prefix("/").unwrap());
        debug!("[DRY_RUN] Reading '{}' from disk at <{}>", &s3_req.key, file_path.display());

        match serve_file(file_path) {
            Ok(_data) => data = _data,
            Err(response) => return response,
        }
    } else {
        match TOKIO_RUNTIME.block_on(async {
            S3_CLIENT.get_object(s3_req).await.map(|output| {
                let mut data: Vec<u8> = Vec::new();
                output.body.unwrap()
                    .into_blocking_read()
                    .read_to_end(&mut data)
                    .expect("Failed to read ByteStream");
                data
            })
        }) {
            Ok(_data) => data = _data,
            Err(err) => {
                debug!("Error getting S3 object: {}", err);
                return Response::build()
                    .status(Status::NotFound)
                    .sized_body(Cursor::new("Object not found"))
                    .finalize()
            },
        }
    }

    match decrypt(data, profile) {
        Ok(decrypted_data) => {
            Response::build().sized_body(Cursor::new(decrypted_data)).finalize()
        },
        Err(err) => {
            debug!("Error decrypting file: {:?}", err);
            Response::build()
                .status(Status::InternalServerError)
                .sized_body(Cursor::new("Internal Server Error"))
                .finalize()
        },
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
                // TODO: Re-enable Basic authentication
                // debug!("Basic Authorization provided");

                // let credentials_base64 = authorization.trim_start_matches("Basic ");

                // let credentials_bytes: Vec<u8>;
                // match general_purpose::STANDARD.decode(credentials_base64) {
                //     Ok(bytes) => credentials_bytes = bytes,
                //     Err(err) => {
                //         debug!("Error: Basic credentials cannot be decoded from base64: {} ({})", err, credentials_base64);
                //         return Outcome::Failure((Status::Unauthorized, TokenError::Invalid))
                //     },
                // }

                // let credentials: String;
                // match String::from_utf8(credentials_bytes) {
                //     Ok(credentials_str) => credentials = credentials_str,
                //     Err(err) => {
                //         debug!("Error: Basic credentials cannot be decoded from base64: {} ({})", err, credentials_base64);
                //         return Outcome::Failure((Status::Unauthorized, TokenError::Invalid))
                //     },
                // }

                // // Split the credentials into username and password
                // let parts: Vec<&str> = credentials.splitn(2, ':').collect();
                // if parts.len() != 2 {
                //     debug!("Error: Basic auth header not formatted as `username:password`: {}", credentials);
                // }

                // let username = parts[0];
                // let password = parts[1];

                // // FIXME: Check password

                // debug!("Logged in as '{}'", username);

                // match &biscuit {
                //     None => {
                //         match biscuit!(r#"
                //         profile({username});
                //         "#).build(&ROOT_KEY) {
                //             Ok(new_biscuit) => {
                //                 trace!("Creating biscuit from Basic credentials");
                //                 biscuit = Some(new_biscuit);
                //                 should_save = true;
                //             },
                //             Err(err) => {
                //                 debug!("Error: Could not create biscuit from Basic credentials: {}", err);
                //             },
                //         }
                //     },
                //     Some(acc) => {
                //         trace!("Making bigger biscuit from Basic credentials");

                //         let source = (0..acc.block_count())
                //             .map(|n| acc.print_block_source(n).unwrap())
                //             .collect::<Vec<String>>()
                //             .join("\n\n");

                //         let mut builder = Biscuit::builder();
                //         builder.add_code(source).unwrap();
                //         builder.add_fact(fact!("profile({username})")).unwrap();
                //         match builder.build(&ROOT_KEY) {
                //             Ok(b) => {
                //                 biscuit = Some(b);
                //                 should_save = true;
                //             },
                //             Err(err) => {
                //                 debug!("Error: Could not append block to biscuit: {}", err);
                //             },
                //         }
                //     },
                // }
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

        let token = Token::from_request(request).succeeded();

        refresh_token = decode(&refresh_token).unwrap().to_string();
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

fn add_cookie_if_necessary(token: &Token, cookies: &mut Cookies) {
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

// #[get("/login")]
// fn login_middleware(request: &Request<Body>) -> &'static str {
//     trace!("login_middleware");

//     if request.uri().path() != "/login" {
//         return None
//     }

//     if request.headers().get(AUTHORIZATION.as_str())
//         .is_some_and(|v| v.to_str().unwrap().starts_with("Bearer "))
//     {
//         let url = "/";
//         trace!("Redirecting to <{}>…", url);
//         return Some(Response::builder()
//             .status(StatusCode::FOUND) // 302 Found (Temporary Redirect)
//             .header(LOCATION, url)
//             // Add Cache-Control and Pragma headers to prevent caching
//             // .header(CACHE_CONTROL, "no-cache")
//             // .header(PRAGMA, "no-cache");
//             .body(Body::empty())
//             .map_err(Error::HyperHTTP))
//     }

//     Some(Response::builder()
//         // .status(StatusCode::UNAUTHORIZED)
//         .body(Body::from(r#"
//         <!DOCTYPE html>
//         <html lang="en">
//         <head>
//             <meta charset="UTF-8">
//             <meta name="viewport" content="width=device-width, initial-scale=1.0">
//             <title>Login</title>
//         </head>
//         <body>
//             <h1>Login</h1>
//             <form method="POST" action="/login">
//                 <label for="username">Username:</label>
//                 <input type="text" id="username" name="username" required><br><br>
                
//                 <label for="password">Password:</label>
//                 <input type="password" id="password" name="password" required><br><br>
                
//                 <input type="submit" value="Login">
//             </form>
//         </body>
//         </html>
//         "#))
//         .map_err(Error::HyperHTTP))
// }

// #[post("/login")]
// fn login(request: &Request<Body>) -> &'static str {
//     if request.headers().get(AUTHORIZATION.as_str())
//         .is_some_and(|v| v.to_str().unwrap().starts_with("Bearer "))
//     {
//         let url = "/";
//         trace!("Redirecting to <{}>…", url);
//         return Some(Response::builder()
//             .status(StatusCode::FOUND) // 302 Found (Temporary Redirect)
//             .header(LOCATION, url)
//             // Add Cache-Control and Pragma headers to prevent caching
//             // .header(CACHE_CONTROL, "no-cache")
//             // .header(PRAGMA, "no-cache");
//             .body(Body::empty())
//             .map_err(Error::HyperHTTP))
//     }

//     todo!()
// }

fn list_objects(prefix: &str) -> Result<Vec<String>, Error> {
    let s3_req = ListObjectsV2Request {
        bucket: BUCKET_NAME.to_string(),
        prefix: Some(prefix.to_string()),
        ..Default::default()
    };

    if DRY_RUN || LOCAL {
        debug!("[DRY_RUN] Listing objects with prefix '{}' from bucket '{}'", s3_req.prefix.unwrap(), s3_req.bucket);
        debug!("[DRY_RUN] Reading files in <{}> instead", WEBSITE_DIR.display());
        Ok(find_all_files().iter()
            .map(|path|
                format!("/{}", path
                    .strip_prefix(WEBSITE_DIR.as_path())
                    .expect("Could not remove prefix")
                    .display())
            )
            .collect())
    } else {
        TOKIO_RUNTIME.block_on(async {
            match S3_CLIENT.list_objects_v2(s3_req).await {
                Ok(output) => {
                    if let Some(objects) = output.contents {
                        debug!("Found objects: {:?}", objects);
                        Ok(objects.iter().flat_map(|o| o.key.clone()).collect())
                    } else {
                        debug!("No object sent back from S3");
                        // FIXME: Return error
                        Ok(Vec::new())
                    }
                },
                Err(err) => {
                    debug!("Error listing S3 objects: {}", err);
                    Err(Error::RusotoListObject(err))
                },
            }
        })
    }
}

fn find_all_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    find(&WEBSITE_DIR, &mut files);
    files
}

fn find(dir: &PathBuf, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        if let Ok(entry) = entry {
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            } else if path.is_dir() {
                find(&path, files);
            }
        }
    }
}

fn get_key(key_name: &str) -> Result<Key<Aes256Gcm>, io::Error> {
    let mut keys = KEYS.lock().unwrap();
    if let Some(key) = keys.get(key_name) {
        trace!("Read key '{}' from cache", key_name);
        return Ok(*key);
    }

    let key_file = key_file(key_name);

    if key_file.exists() {
        // If key file exists, read the file
        trace!("Reading key '{}' from <{}>…", key_name, key_file.display());
        let mut file = File::open(key_file)?;
        let mut key_bytes = [0u8; 32];
        file.read_exact(&mut key_bytes)?;
        let key: Key<Aes256Gcm> = key_bytes.into();
        keys.insert(key_name.to_string(), key);
        Ok(key)
    } else {
        // If key file does not exist, create a new key and save it to a new file
        trace!("Saving new key '{}' into <{}>…", key_name, key_file.display());
        let key = Aes256Gcm::generate_key(OsRng);
        let mut file = File::create(&key_file)?;
        file.write_all(&key)?;
        keys.insert(key_name.to_string(), key);
        Ok(key)
    }
}

fn get_root_key() -> Result<biscuit::KeyPair, Error> {
    let key_name = ROOT_KEY_NAME;
    let key_file = key_file(key_name);

    if key_file.exists() {
        // If key file exists, read the file
        trace!("Reading key '{}' from <{}>…", key_name, key_file.display());
        let mut file = File::open(key_file).map_err(Error::IO)?;
        let mut key_bytes = String::new();
        file.read_to_string(&mut key_bytes).map_err(Error::IO)?;
        let key = biscuit::PrivateKey::from_bytes_hex(&key_bytes).map_err(Error::BiscuitFormat)?;
        Ok(biscuit::KeyPair::from(&key))
    } else {
        // If key file does not exist, create a new key and save it to a new file
        trace!("Saving new key '{}' into <{}>…", key_name, key_file.display());
        let key_pair = biscuit::KeyPair::new();
        let mut file = File::create(&key_file).map_err(Error::IO)?;
        file.write_all(key_pair.private().to_bytes_hex().as_bytes()).map_err(Error::IO)?;
        Ok(key_pair)
    }
}

fn key_file(key_name: &str) -> PathBuf {
    KEYS_DIR.join(format!("{}.key", key_name))
}

fn decrypt(encrypted_data: Vec<u8>, key_name: &str) -> Result<Vec<u8>, Error> {
    let key = get_key(key_name).map_err(Error::IO)?;
    let cipher = Aes256Gcm::new(&key);

    // Separate the nonce and ciphertext
    let nonce_size = 12; // AES-GCM nonce size is typically 12 bytes
    let (nonce, ciphertext) = encrypted_data.split_at(nonce_size);

    // Decrypt the ciphertext
    let decrypted_data = cipher.decrypt(nonce.into(), ciphertext).map_err(Error::AES)?;

    // Implement your decryption logic here
    // This is just a placeholder
    Ok(decrypted_data)
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
            return p.starts_with('@')
        })
        .collect()
}

#[derive(Debug)]
enum Error {
    IO(io::Error),
    AES(aes_gcm::Error),
    BiscuitFormat(biscuit::error::Format),
    RusotoListObject(rusoto_core::RusotoError<rusoto_s3::ListObjectsV2Error>)
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::IO(err) => err.fmt(f),
            Error::AES(err) => err.fmt(f),
            Error::BiscuitFormat(err) => err.fmt(f),
            Error::RusotoListObject(err) => err.fmt(f),
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
}