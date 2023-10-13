use rusoto_core::{credential::StaticProvider, ByteStream, HttpClient, Region, RusotoError};
use rusoto_s3::{S3, S3Client, PutObjectRequest, PutObjectError};
use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::exit;
use tracing_subscriber::FmtSubscriber;
use tracing::{Level, error, info};
#[macro_use]
extern crate lazy_static;

lazy_static! {
    static ref BASE_DIR: &'static Path = Path::new(".orangutan");
    static ref WEBSITE_DIR: PathBuf = BASE_DIR.join("website");

    // TODO: Make this a command-line argument
    static ref DRY_RUN: bool = false;
}

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber.");

    if let Err(err) = throwing_main().await {
        error!("Error: {}", err);
        exit(1);
    }
}

async fn throwing_main() -> Result<(), RusotoError<PutObjectError>> {
    // TODO: Source `.orangutan/.env`

    let region = Region::Custom {
        name: "custom-region".to_string(),
        endpoint: env::var("S3_REGION_ENDPOINT").expect("env vars not set"),
    };
    let credentials_provider = StaticProvider::new_minimal(
        env::var("S3_KEY_ID").expect("env vars not set"),
        env::var("S3_ACCESS_KEY").expect("env vars not set"),
    );
    let s3_client = S3Client::new_with(
        HttpClient::new().expect("Failed to create HTTP client"),
        credentials_provider,
        region,
    );
    let bucket_name = "orangutan";

    for path in find_all_files() {
        let object_name = format!("/{}", path
            .strip_prefix(WEBSITE_DIR.as_path())
            .expect("Could not remove prefix")
            .display());
        info!("Processing '{}'…", object_name);

        let s3_req = PutObjectRequest {
            bucket: bucket_name.to_string(),
            key: object_name,
            body: Some(bytestream(&path)),
            ..Default::default()
        };
        if *DRY_RUN {
            info!("[DRY_RUN] Upload object '{}' into bucket '{}'", s3_req.key, s3_req.bucket);
        } else {
            s3_client.put_object(s3_req).await?;
        }
    }

    Ok(())
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

fn bytestream(file_path: &PathBuf) -> ByteStream {
    // Open the file
    let mut file = File::open(file_path).expect("Failed to open file");

    // Read the file's contents into a Vec<u8>
    let mut file_content = Vec::new();
    file.read_to_end(&mut file_content).expect("Failed to read file");

    // Create a ByteStream from the Vec<u8>
    ByteStream::from(file_content)
}
