use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key};
use base64::{Engine as _, engine::general_purpose};
use hyper::body::HttpBody;
use hyper::{Body, Request, Response, Server, StatusCode};
use hyper::header::{HeaderValue, WWW_AUTHENTICATE, LOCATION, AUTHORIZATION};
use hyper::http::uri::Authority;
use hyper::service::{make_service_fn, service_fn};
use rusoto_core::{credential::StaticProvider, HttpClient, Region};
use rusoto_s3::{S3, S3Client, GetObjectRequest};
use tokio::io::AsyncReadExt;
use std::collections::HashMap;
use std::convert::Infallible;
use std::io::{Read, Write};
use std::{env, io, fmt};
use std::fs::File;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Mutex;
use tracing_subscriber::FmtSubscriber;
use tracing::{Level, debug, error, info, trace};
#[macro_use]
extern crate lazy_static;
extern crate biscuit_auth as biscuit;
use biscuit::Biscuit;
use biscuit::macros::biscuit;

lazy_static! {
    static ref BASE_DIR: &'static Path = Path::new(".orangutan");
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
    static ref BUCKET_NAME: &'static str = "orangutan";

    static ref ROOT_KEY_NAME: &'static str = "_biscuit-root";
    static ref ROOT_KEY: biscuit::KeyPair = {
        match get_root_key() {
            Ok(public_key) => public_key,
            Err(err) => {
                error!("Error generating root Biscuit key: {}", err);
                exit(1);
            }
        }
    };

    // TODO: Make this a command-line argument
    static ref DRY_RUN: bool = true;
}

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber.");

    if let Err(err) = throwing_main().await {
        error!("Error: {}", err);
        exit(1);
    }
}

async fn throwing_main() -> Result<(), std::io::Error> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));

    let make_svc = make_service_fn(|_conn| async {
        // let a = with_middleware(basic_auth_middleware, handle_request);
        Ok::<_, Infallible>(service_fn(|req: Request<Body>| async {
            debug!("Received {:?}", req);

            // Apply middlewares to the incoming request
            if let Some(Ok(response)) = basic_auth_middleware(&req) {
                return Ok(response)
            }
            if let Some(Ok(response)) = login_middleware(&req) {
                return Ok(response)
            }

            // Call the inner handler with the modified request
            handle_request(req).await
        }))
    });

    let server = Server::bind(&addr).serve(make_svc);

    info!("Server started on <{}>", addr);

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }

    Ok(())
}

// fn with_middleware(
//     middleware: impl Fn(&Request<Body>) -> Option<Result<Response<Body>, Error>>,
//     inner: impl Fn(Request<Body>) -> Result<Response<Body>, Infallible>,
// ) -> impl Fn(Request<Body>) -> dyn Future<Output = Result<Response<Body>, Infallible>> {
//     let a = |req: Request<Body>| async {
//         // Apply the middleware to the incoming request
//         if let Some(Ok(response)) = middleware(&req) {
//             return Ok(response)
//         }

//         // Call the inner handler with the modified request
//         inner(req).await
//     };
//     let b = a(Request::default());
//     a
// }

async fn handle_request(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    let path = req.uri().path();
    trace!("GET {}", path);

    let stored_objects: Vec<String>;
    match list_objects(path) {
        Ok(o) => stored_objects = o,
        Err(err) => {
            error!("Error when listing objects matching '{}': {}", path, err);
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("Object not found"))
                .unwrap())
        }
    }

    let matching_files = matching_files(path, stored_objects);

    let Some(object_key) = matching_files.first() else {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Object not found"))
            .unwrap())
    };

    let s3_req = GetObjectRequest {
        bucket: BUCKET_NAME.to_string(),
        key: object_key.to_owned(),
        ..Default::default()
    };

    if *DRY_RUN {
        debug!("[DRY_RUN] Get object '{}' from bucket '{}'", s3_req.key, s3_req.bucket);
        Ok(Response::new(Body::from("Dry run")))
    } else {
        match S3_CLIENT.get_object(s3_req).await {
            Ok(output) => {
                let mut data: Vec<u8> = Vec::new();
                output.body.unwrap()
                    .into_async_read()
                    .read_to_end(&mut data)
                    .await
                    .expect("Failed to read ByteStream");
                match decrypt(data, "_default") {
                    Ok(decrypted_data) => {
                        Ok(Response::new(Body::from(decrypted_data)))
                    }
                    Err(err) => {
                        debug!("Error decrypting file: {:?}", err);
                        Ok(Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from("Internal Server Error"))
                            .unwrap())
                    }
                }
            }
            Err(_) => Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("Object not found"))
                .unwrap())
        }
    }
}

fn list_objects(_prefix: &str) -> Result<Vec<String>, Error> {
    // FIXME: Implement
    Ok(vec![
        "/index.html@_default".to_string(),
        "/whatever/index.html@family".to_string(),
        "/whatever/index.html@family".to_string(),
        "/whatever/p.html@_default".to_string(),
        "/whatever/index.htmlindex.html@_default".to_string(),
        "/whatever/other-page/index.html@_default".to_string(),
        "/whatever/a/b.html@_default".to_string(),
    ])
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
    let key_name = *ROOT_KEY_NAME;
    let key_file = key_file(key_name);

    if key_file.exists() {
        // If key file exists, read the file
        trace!("Reading key '{}' from <{}>…", key_name, key_file.display());
        let mut file = File::open(key_file).map_err(Error::IO)?;
        let mut key_bytes = [0u8; 32];
        file.read_exact(&mut key_bytes).map_err(Error::IO)?;
        let key = biscuit::PrivateKey::from_bytes(&key_bytes).map_err(Error::BiscuitFormat)?;
        Ok(biscuit::KeyPair::from(&key))
    } else {
        // If key file does not exist, create a new key and save it to a new file
        trace!("Saving new key '{}' into <{}>…", key_name, key_file.display());
        let key_pair = biscuit::KeyPair::new();
        let mut file = File::create(&key_file).map_err(Error::IO)?;
        file.write_all(&key_pair.private().to_bytes()).map_err(Error::IO)?;
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

fn matching_files<'a>(query: &str, stored_objects: Vec<String>) -> Vec<String> {
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

fn basic_auth_middleware(request: &Request<Body>) -> Option<Result<Response<Body>, Error>> {
    trace!("basic_auth_middleware");

    fn redirect_to_login(request: &Request<Body>) -> Option<Result<Response<Body>, Error>> {
        if request.uri().path() == "/login" {
            return None
        } else {
            let redirect_to = request.uri().to_string();
            debug!("Redirecting to <{}>…", redirect_to);
            return Some(Response::builder()
                .body(Body::from(format!(r#"
                <head>
                    <meta http-equiv="Refresh" content="0; URL=/login?redirect={}" />
                </head>
                "#, urlencoding::encode(redirect_to.as_str()))))
                .map_err(Error::HyperHTTP))
        }
    }
    fn authorization_header<'a>(request: &'a Request<Body>) -> Option<&'a HeaderValue> {
        trace!("Checking 'Authorization' header…");
        let header = request.headers().get("Authorization");
        if header.is_none() { debug!("'Authorization' header not set"); }
        header
    }
    fn uri_authority<'a>(request: &'a Request<Body>) -> Option<&'a Authority> {
        trace!("Checking URI authority…");
        let authority = request.uri().authority();
        if authority.is_none() { debug!("URI authority not found"); }
        authority
    }

    let credentials: String;
    if let Some(authorization) = authorization_header(request) {
        // Redirect to `/login` if needed, to handle authentication there
        if let Some(res) = redirect_to_login(request) { return Some(res) }

        let Ok(authorization) = authorization.to_str() else {
            debug!("Authorization header cannot be converted to String");
            return Some(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Body::from("Authorization header cannot be converted to String"))
                .map_err(Error::HyperHTTP))
        };

        // The `Authorization` header should start with "Basic "
        if !authorization.starts_with("Basic ") {
            debug!("Authorization not Basic, nothing to do");
            return None
        }

        let credentials_base64 = authorization.trim_start_matches("Basic ");

        let credentials_bytes: Vec<u8>;
        match general_purpose::STANDARD.decode(credentials_base64) {
            Ok(bytes) => credentials_bytes = bytes,
            Err(err) => {
                debug!("Error: Basic credentials cannot be decoded from base64: {} ({})", err, credentials_base64);
                return Some(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body(Body::from("Error: Basic credentials cannot be decoded from base64"))
                    .map_err(Error::HyperHTTP))
            },
        }

        match String::from_utf8(credentials_bytes) {
            Ok(credentials_str) => credentials = credentials_str,
            Err(err) => {
                debug!("Error: Basic credentials cannot be decoded from base64: {} ({})", err, credentials_base64);
                return Some(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body(Body::from("Basic credentials cannot be decoded from base64"))
                    .map_err(Error::HyperHTTP))
            },
        }
    } else if let Some(authority) = uri_authority(request) {
        // Redirect to `/login` if needed, to handle authentication there
        if let Some(res) = redirect_to_login(request) { return Some(res) }

        // Get the authority part from the URI
        // NOTE: `request.uri().authority()` doesn't work as expected, hence the workaround
        let split: Vec<&str> = authority.as_str().splitn(2, "@").collect();
        if split.len() != 2 {
            debug!("URI authorization not set");
            return None
        };

        credentials = split[0].to_string();
    } else {
        return None
    }

    // TODO: Add an assertion to ensure `redirect_to_login` has been called

    // Split the credentials into username and password
    let parts: Vec<&str> = credentials.splitn(2, ':').collect();
    if parts.len() != 2 {
        debug!("Error: Basic auth header not formatted as `username:password`: {}", credentials);
        return Some(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("Error: Basic auth header not formatted as `username:password`"))
            .map_err(Error::HyperHTTP))
    }

    let username = parts[0];
    let password = parts[1];

    // FIXME: Check password

    debug!("Logged in as '{}'", username);

    let biscuit: Biscuit;
    match biscuit!(r#"
        profile({username});
        "#).build(&ROOT_KEY) {
        Ok(b) => biscuit = b,
        Err(err) => {
            debug!("Error: Could not create biscuit: {}", err);
            return Some(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Error: Could not create token"))
                .map_err(Error::HyperHTTP))
        }
    }

    let biscuit_base64: String;
    match biscuit.to_base64() {
        Ok(base64) => biscuit_base64 = base64,
        Err(err) => {
            debug!("Error: Could not convert biscuit to base64: {}", err);
            return Some(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Error: Could not create token"))
                .map_err(Error::HyperHTTP))
        }
    }

    debug!("Created Biscuit {}", biscuit_base64);

    let redirect_to = request.headers().get("redirect")
        .map(|h| h.to_str().unwrap())
        .unwrap_or("/");

    Some(Response::builder()
        .header(AUTHORIZATION, format!("Bearer {}", biscuit_base64))
        .body(Body::from(format!(r#"
        <head>
            <meta http-equiv="Refresh" content="0; URL={}" />
        </head>
        "#, redirect_to)))
        .map_err(Error::HyperHTTP))
}

fn login_middleware(request: &Request<Body>) -> Option<Result<Response<Body>, Error>> {
    trace!("login_middleware");

    if request.uri().path() != "/login" {
        return None
    }

    if request.headers().get(AUTHORIZATION.as_str())
        .is_some_and(|v| v.to_str().unwrap().starts_with("Bearer "))
    {
        let url = "/";
        trace!("Redirecting to <{}>…", url);
        return Some(Response::builder()
            .status(StatusCode::FOUND) // 302 Found (Temporary Redirect)
            .header(LOCATION, url)
            // Add Cache-Control and Pragma headers to prevent caching
            // .header(CACHE_CONTROL, "no-cache")
            // .header(PRAGMA, "no-cache");
            .body(Body::empty())
            .map_err(Error::HyperHTTP))
    }

    Some(Response::builder()
        // .status(StatusCode::UNAUTHORIZED)
        .body(Body::from(r#"
        <!DOCTYPE html>
        <html lang="en">
        <head>
            <meta charset="UTF-8">
            <meta name="viewport" content="width=device-width, initial-scale=1.0">
            <title>Login</title>
        </head>
        <body>
            <h1>Login</h1>
            <form method="POST" action="/login">
                <label for="username">Username:</label>
                <input type="text" id="username" name="username" required><br><br>
                
                <label for="password">Password:</label>
                <input type="password" id="password" name="password" required><br><br>
                
                <input type="submit" value="Login">
            </form>
        </body>
        </html>
        "#))
        .map_err(Error::HyperHTTP))
}

fn enforce_bearer_auth(request: &Request<Body>) -> Option<Result<Response<Body>, Error>> {
    trace!("enforce_bearer_auth");

    fn authorization_header<'a>(request: &'a Request<Body>) -> Option<&'a HeaderValue> {
        trace!("Checking 'Authorization' header…");
        let header = request.headers().get("Authorization");
        if header.is_none() { debug!("'Authorization' header not set"); }
        header
    }
    fn token_param<'a>(request: &'a Request<Body>) -> Option<&'a str> {
        request.uri().query()
    }

    let token: &str;
    if let Some(authorization) = authorization_header(request) {
        let Ok(authorization) = authorization.to_str() else {
            debug!("Authorization header cannot be converted to String");
            return Some(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Body::from("Authorization header cannot be converted to String"))
                .map_err(Error::HyperHTTP))
        };

        // The `Authorization` header should start with "Bearer "
        if !authorization.starts_with("Bearer ") {
            debug!("Authorization not Bearer");
            return Some(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Body::from("Bearer authorization required"))
                .map_err(Error::HyperHTTP))
        }

        token = authorization.trim_start_matches("Bearer ");
    } else {
        return Some(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::empty())
            .map_err(Error::HyperHTTP))
    }

    match Biscuit::from(token, ROOT_KEY.public()) {
        Ok(biscuit) => debug!("{:?}", biscuit),
        Err(err) => {
            debug!("Error reading token: {}", err);
            return Some(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Body::from("Invalid Bearer token"))
                .map_err(Error::HyperHTTP))
        },
    }

    Some(Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::empty())
        .map_err(Error::HyperHTTP))
}

#[derive(Debug)]
enum Error {
    IO(io::Error),
    AES(aes_gcm::Error),
    BiscuitFormat(biscuit::error::Format),
    HyperHTTP(hyper::http::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::IO(err) => err.fmt(f),
            Error::AES(err) => err.fmt(f),
            Error::BiscuitFormat(err) => err.fmt(f),
            Error::HyperHTTP(err) => err.fmt(f),
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
            matching_files("", stored_objects.clone()),
            vec![
                "/index.html@_default",
            ]
        );
        assert_eq!(
            matching_files("/", stored_objects.clone()),
            vec![
                "/index.html@_default",
            ]
        );
        assert_eq!(
            matching_files("/index.html", stored_objects.clone()),
            vec![
                "/index.html@_default",
            ]
        );

        assert_eq!(
            matching_files("/whatever", stored_objects.clone()),
            vec![
                "/whatever/index.html@friends",
                "/whatever/index.html@family",
            ]
        );
        assert_eq!(
            matching_files("/whatever/", stored_objects.clone()),
            vec![
                "/whatever/index.html@friends",
                "/whatever/index.html@family",
            ]
        );
        assert_eq!(
            matching_files("/whatever/index.html", stored_objects.clone()),
            vec![
                "/whatever/index.html@friends",
                "/whatever/index.html@family",
            ]
        );

        assert_eq!(
            matching_files("/whatever/a", stored_objects.clone()),
            Vec::<&str>::new()
        );
        assert_eq!(
            matching_files("/whatever/a/b", stored_objects.clone()),
            Vec::<&str>::new()
        );
        assert_eq!(
            matching_files("/whatever/a/b.html", stored_objects.clone()),
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
            matching_files("/style.css", stored_objects.clone()),
            vec![
                "/style.css@_default",
            ]
        );
        assert_eq!(
            matching_files("/anything.custom", stored_objects.clone()),
            vec![
                "/anything.custom@friends",
                "/anything.custom@family",
            ]
        );
    }
}
