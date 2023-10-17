#![feature(proc_macro_hygiene, decl_macro, never_type)]

#[macro_use] extern crate rocket;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key};
use base64::{Engine as _, engine::general_purpose};
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
const ROOT_KEY_NAME: &'static str = "_biscuit_root";
const TOKEN_COOKIE_NAME: &'static str = "token";
const TOKEN_QUERY_PARAM_NAME: &'static str = "token";
const REFRESH_TOKEN_QUERY_PARAM_NAME: &'static str = "refresh_token";

lazy_static! {
    static ref MODE: Result<String, env::VarError> = env::var("MODE");
    static ref KEYS_MODE: Result<String, env::VarError> = env::var("KEYS_MODE");

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
        let keys_reader = <dyn KeysReader>::detect();
        match keys_reader.get_root_biscuit_key() {
            Ok(public_key) => public_key,
            Err(err) => {
                error!("Error generating root Biscuit key: {}", err);
                exit(1);
            }
        }
    };

    static ref TOKIO_RUNTIME: Runtime = Runtime::new().unwrap();
}

fn main() {
    // let subscriber = FmtSubscriber::builder()
    //     .with_max_level(Level::DEBUG)
    //     .finish();

    // tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber.");

    // env_logger::init();
    // env_logger::Builder::new()
    //     .target(env_logger::Target::Stdout)
    //     .init();

    if let Err(err) = throwing_main() {
        error!("Error: {}", err);
        exit(1);
    }
}

fn throwing_main() -> Result<(), std::io::Error> {
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
        .unwrap_or_else(|e| e)
}

#[get("/<_path..>", rank = 3)]
fn handle_request<'a>(_path: PathBuf, origin: &'a Origin<'a>) -> Response<'a> {
    _handle_request(origin, None)
        .unwrap_or_else(|e| e)
}

fn _handle_request<'a>(origin: &Origin<'_>, biscuit: Option<&Biscuit>) -> Result<Response<'a>, Response<'a>> {
    let path = decode(origin.path()).unwrap().into_owned();
    trace!("GET {}", &path);

    let object_reader = <dyn ObjectReader>::detect();

    let stored_objects: Vec<String>;
    match object_reader.list_objects(&path) {
        Ok(o) => stored_objects = o,
        Err(err) => {
            error!("Error when listing objects matching '{}': {}", &path, err);
            return Err(not_found())
        }
    }

    let matching_files = matching_files(&path, &stored_objects);

    let allowed_profiles = matching_files.iter().flat_map(|f| f.rsplit_terminator("@").next());
    if allowed_profiles.clone().next() == None {
        // If allowed profiles is empty, it means it's a static file
        return object_reader.read_object(&path)
            .map(|data| {
                Response::build().sized_body(Cursor::new(data)).finalize()
            });
        // let file_path = WEBSITE_DIR.join(path.strip_prefix("/").unwrap());
        // return match LocalObjectReader::serve_file(file_path) {
        //     Ok(data) => {
        //         Ok(Response::build().sized_body(Cursor::new(data)).finalize())
        //     },
        //     Err(err) => {
        //         debug!("Error reading static file: {:?}", err);
        //         Err(Response::build()
        //             .status(Status::InternalServerError)
        //             .sized_body(Cursor::new("Internal Server Error"))
        //             .finalize())
        //     },
        // }
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
        return Err(not_found())
    };

    // NOTE: Cannot use `if let Some(object_key)` as it causes
    //   `#[get] can only be used on functions`.
    let object_key: &String;
    match matching_files.iter().filter(|f| f.ends_with(&format!("@{}", profile))).next() {
        Some(key) => object_key = key,
        None => {
            debug!("No object matching '{}' in {:?}", &path, stored_objects);
            return Err(not_found())
        }
    }

    let data: Vec<u8> = object_reader.read_object(object_key)?;

    match decrypt(data, profile) {
        Ok(decrypted_data) => {
            Ok(Response::build().sized_body(Cursor::new(decrypted_data)).finalize())
        },
        Err(err) => {
            debug!("Error decrypting file: {:?}", err);
            Err(Response::build()
                .status(Status::InternalServerError)
                .sized_body(Cursor::new("Internal Server Error"))
                .finalize())
        },
    }
}

fn not_found<'a>() -> Response<'a> {
    Response::build()
        .status(Status::NotFound)
        // TODO: Re-enable Basic authentication
        // .raw_header("WWW-Authenticate", "Basic realm=\"This page is protected. Please log in.\"")
        .sized_body(Cursor::new("This page doesn't exist or you are not allowed to see it."))
        .finalize()
}

trait ObjectReader {
    fn list_objects(&self, prefix: &str) -> Result<Vec<String>, Error>;
    fn read_object<'a>(&self, object_key: &str) -> Result<Vec<u8>, Response<'a>>;
}

impl dyn ObjectReader {
    fn detect() -> Box<dyn ObjectReader> {
        match MODE.clone().unwrap_or("".to_string()).as_str() {
            "S3" => Box::new(S3ObjectReader {}),
            "ENV" | _ => Box::new(LocalObjectReader {}),
        }
    }
}

struct S3ObjectReader {}

impl ObjectReader for S3ObjectReader {
    fn list_objects(&self, prefix: &str) -> Result<Vec<String>, Error> {
        let s3_req = ListObjectsV2Request {
            bucket: BUCKET_NAME.to_string(),
            prefix: Some(prefix.to_string()),
            ..Default::default()
        };
        trace!("Listing objects with prefix '{}' from bucket '{}'…", prefix, s3_req.bucket);

        TOKIO_RUNTIME.block_on(S3_CLIENT.list_objects_v2(s3_req))
            .map(|output| {
                if let Some(objects) = output.contents {
                    debug!("Found objects: {:?}", objects);
                    objects.iter().flat_map(|o| o.key.clone()).collect()
                } else {
                    debug!("No object sent back from S3");
                    // FIXME: Return error
                    Vec::new()
                }
            })
            .map_err(|err| {
                error!("Error listing S3 objects: {}", err);
                Error::RusotoListObject(err)
            })
    }

    fn read_object<'a>(&self, object_key: &str) -> Result<Vec<u8>, Response<'a>> {
        let s3_req = GetObjectRequest {
            bucket: BUCKET_NAME.to_string(),
            key: object_key.to_string(),
            ..Default::default()
        };
        trace!("Getting object '{}' from bucket '{}'…", &s3_req.key, s3_req.bucket);

        TOKIO_RUNTIME.block_on(S3_CLIENT.get_object(s3_req))
            .map(|output| {
                let mut data: Vec<u8> = Vec::new();
                output.body.unwrap()
                    .into_blocking_read()
                    .read_to_end(&mut data)
                    .expect("Failed to read ByteStream");
                data
            })
            .map_err(|err| {
                error!("Error getting S3 object: {}", err);
                not_found()
            })
    }
}

struct LocalObjectReader {}

impl LocalObjectReader {
    fn serve_file<'a>(file_path: PathBuf) -> Result<Vec<u8>, Response<'a>> {
        if let Ok(mut file) = File::open(&file_path) {
            let mut data: Vec<u8> = Vec::new();
            if let Err(err) = file.read_to_end(&mut data) {
                debug!("Could not read <{}> from disk: {}", file_path.display(), err);
                return Err(not_found())
            }
            Ok(data)
        } else {
            debug!("Could not read <{}> from disk: Cannot open file", file_path.display());
            Err(not_found())
        }
    }
}

impl ObjectReader for LocalObjectReader {
    fn list_objects(&self, prefix: &str) -> Result<Vec<String>, Error> {
        trace!("Listing files with prefix '{}' in <{}>…", prefix, WEBSITE_DIR.display());
        Ok(find_all_files().iter()
            .map(|path|
                format!("/{}", path
                    .strip_prefix(WEBSITE_DIR.as_path())
                    .expect("Could not remove prefix")
                    .display())
            )
            .collect())
    }

    fn read_object<'a>(&self, object_key: &str) -> Result<Vec<u8>, Response<'a>> {
        let file_path = WEBSITE_DIR.join(object_key.strip_prefix("/").unwrap());
        trace!("Reading '{}' from disk at <{}>…", object_key, file_path.display());

        Self::serve_file(file_path)
    }
}

trait KeysReader {
    fn get_key(&self, key_name: &str) -> Result<Key<Aes256Gcm>, Error>;
    fn get_root_biscuit_key(&self) -> Result<biscuit::KeyPair, Error>;
}

impl dyn KeysReader {
    fn detect() -> Box<dyn KeysReader> {
        match KEYS_MODE.clone().unwrap_or("".to_string()).as_str() {
            "LOCAL" => Box::new(LocalKeysReader {}),
            "ENV" | _ => Box::new(EnvKeysReader {}),
        }
    }
}

struct EnvKeysReader {}

impl KeysReader for EnvKeysReader {
    fn get_key(&self, key_name: &str) -> Result<Key<Aes256Gcm>, Error> {
        let mut keys = KEYS.lock().unwrap();
        if let Some(key) = keys.get(key_name) {
            trace!("Read key '{}' from cache", key_name);
            return Ok(*key);
        }

        let env_var_name = format!("KEY_{}", key_name);
        trace!("Reading key '{}' from environment ({})…", key_name, env_var_name);
        env::var(env_var_name)
            .map_err(Error::Env)
            .and_then(|key_base64| {
                // FIXME: Handle error
                let key = general_purpose::STANDARD.decode(key_base64).unwrap();

                let key_bytes: [u8; 32] = key.as_slice().try_into().unwrap();
                let key: Key<Aes256Gcm> = key_bytes.into();
                keys.insert(key_name.to_string(), key);
                Ok(key)
            })
    }

    fn get_root_biscuit_key(&self) -> Result<biscuit::KeyPair, Error> {
        let key_name = ROOT_KEY_NAME;

        let env_var_name = format!("KEY_{}", key_name);
        trace!("Reading key '{}' from environment ({})…", key_name, env_var_name);
        env::var(env_var_name)
            .map_err(Error::Env)
            .and_then(|key_bytes| {
                let key = biscuit::PrivateKey::from_bytes_hex(&key_bytes).map_err(Error::BiscuitFormat)?;
                Ok(biscuit::KeyPair::from(&key))
            })
    }
}

struct LocalKeysReader {}

impl LocalKeysReader {
    fn key_file(&self, key_name: &str) -> PathBuf {
        KEYS_DIR.join(format!("{}.key", key_name))
    }
}

impl KeysReader for LocalKeysReader {
    fn get_key(&self, key_name: &str) -> Result<Key<Aes256Gcm>, Error> {
        let mut keys = KEYS.lock().unwrap();
        if let Some(key) = keys.get(key_name) {
            trace!("Read key '{}' from cache", key_name);
            return Ok(*key);
        }

        let key_file = self.key_file(key_name);

        if key_file.exists() {
            // If key file exists, read the file
            trace!("Reading key '{}' from <{}>…", key_name, key_file.display());
            let mut file = File::open(key_file).map_err(Error::IO)?;

            let mut buf: Vec<u8> = Vec::new();
            file.read_to_end(&mut buf).map_err(Error::IO)?;
            // FIXME: Handle error
            let key = general_purpose::STANDARD.decode(buf).unwrap();
            // FIXME: Handle error
            let key_bytes: [u8; 32] = key.as_slice().try_into().unwrap();

            let key: Key<Aes256Gcm> = key_bytes.into();
            keys.insert(key_name.to_string(), key);
            Ok(key)
        } else {
            // If key file does not exist, create a new key and save it to a new file
            trace!("Saving new key '{}' into <{}>…", key_name, key_file.display());
            let key = Aes256Gcm::generate_key(OsRng);

            // Encode key as base64
            let mut buf: Vec<u8> = Vec::new();
            // Make sure we'll have a slice big enough for base64 + padding
            buf.resize(key.len() * 4 / 3 + 4, 0);
            let bytes_written = general_purpose::STANDARD.encode_slice(key, &mut buf).unwrap();
            // shorten our vec down to just what was written
            buf.truncate(bytes_written);

            let mut file = File::create(&key_file).map_err(Error::IO)?;
            file.write_all(&buf).map_err(Error::IO)?;
            keys.insert(key_name.to_string(), key);
            Ok(key)
        }
    }

    fn get_root_biscuit_key(&self) -> Result<biscuit::KeyPair, Error> {
        let key_name = ROOT_KEY_NAME;

        let key_file = self.key_file(key_name);

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

fn decrypt(encrypted_data: Vec<u8>, key_name: &str) -> Result<Vec<u8>, Error> {
    let key = <dyn KeysReader>::detect().get_key(key_name)?;
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
    Env(env::VarError),
    AES(aes_gcm::Error),
    BiscuitFormat(biscuit::error::Format),
    RusotoListObject(rusoto_core::RusotoError<rusoto_s3::ListObjectsV2Error>)
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::IO(err) => err.fmt(f),
            Error::Env(err) => err.fmt(f),
            Error::AES(err) => err.fmt(f),
            Error::BiscuitFormat(err) => err.fmt(f),
            Error::RusotoListObject(err) => err.fmt(f),
        }
    }
}

fn add_padding(base64_string: &str) -> String {
    // If base64 string is already padded, don't do anything
    if base64_string.ends_with("=") {
        return base64_string.to_string()
    }

    // Calculate the number of padding characters needed
    let padding_count = 4 - (base64_string.len() % 4);
    
    // Create a new string with the required padding characters
    let padded_string = format!("{}{}", base64_string, "=".repeat(padding_count));

    padded_string
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
