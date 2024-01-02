use crate::config::*;
use crate::helpers::copy_directory;
use core::fmt;
use std::os::fd::FromRawFd;
use std::sync::{Mutex, MutexGuard, Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::io::Cursor;
use rocket::request::Request;
use rocket::response::{self, Response, Responder};
use rocket::http::ContentType;
use lazy_static::lazy_static;
use std::collections::HashSet;
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tracing::{info, debug, trace};

static HUGO_CONFIG_GENERATED: AtomicBool = AtomicBool::new(false);
static DATA_FILES_GENERATED: AtomicBool = AtomicBool::new(false);

lazy_static! {
    // NOTE: `Arc` prevents race conditions
    static ref GENERATED_WEBSITES: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
}

pub fn generate_default_website() -> Result<(), Error> {
    // Generate the website
    generate_website_if_needed(&WebsiteId::default())?;

    // Generate Orangutan data files
    generate_data_files_if_needed()?;

    Ok(())
}

pub fn clone_repository() -> Result<(), Error> {
    if WEBSITE_ROOT.is_dir() {
        return pull_repository()
    }

    _clone_repository()?;
    _init_submodules()?;
    Ok(())
}

fn _clone_repository() -> Result<(), Error> {
    let mut command = Command::new("git");
    command
        .args(vec!["clone", &WEBSITE_REPOSITORY, &WEBSITE_ROOT.display().to_string()])
        .args(vec!["--depth", "1"]);

    trace!("Running `{:?}`…", command);
    let output = command
        .output()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(Error::CommandExecutionFailed { command: format!("{:?}", command), output })
    }
}

fn _init_submodules() -> Result<(), Error> {
    let mut command = Command::new("git");
    command
        .args(vec!["-C", &WEBSITE_ROOT.display().to_string()])
        .args(vec!["submodule", "update", "--init"]);

    trace!("Running `{:?}`…", command);
    let output = command
        .output()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(Error::CommandExecutionFailed { command: format!("{:?}", command), output })
    }
}

pub fn pull_repository() -> Result<(), Error> {
    _pull_repository()?;
    _update_submodules()?;
    Ok(())
}

fn _pull_repository() -> Result<(), Error> {
    let mut command = Command::new("git");
    command
        .args(vec!["-C", &WEBSITE_ROOT.display().to_string()])
        .arg("pull");

    trace!("Running `{:?}`…", command);
    let output = command
        .output()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(Error::CommandExecutionFailed { command: format!("{:?}", command), output })
    }
}

fn _update_submodules() -> Result<(), Error> {
    let mut command = Command::new("git");
    command
        .args(vec!["-C", &WEBSITE_ROOT.display().to_string()])
        .args(vec!["submodule", "update", "--remote", "--recursive"]);

    trace!("Running `{:?}`…", command);
    let output = command
        .output()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(Error::CommandExecutionFailed { command: format!("{:?}", command), output })
    }
}

fn _copy_hugo_config() -> Result<(), Error> {
    debug!("Copying hugo config…");

    // Create config dir
    let config_dir = HUGO_CONFIG_DIR.join("_default");
    fs::create_dir_all(&config_dir)
        .map_err(Error::CannotCreateHugoConfigFile)?;
    debug!("Hugo config will be saved in <{}>", &config_dir.display());

    // Read current config
    let base_config = hugo(vec!["config"])?;

    // Write new config file
    let config_file = config_dir.join("hugo.toml");
    let res = File::create(config_file)
        .map_err(Error::CannotCreateHugoConfigFile)?
        .write_all(&base_config)
        .map_err(Error::CannotCreateHugoConfigFile)?;

    HUGO_CONFIG_GENERATED.store(true, Ordering::Relaxed);

    Ok(res)
}

fn gen_hugo_config(website_id: &WebsiteId) -> Result<(), Error> {
    // Create config dir
    let config_dir = HUGO_CONFIG_DIR.join(website_id.dir_name());
    fs::create_dir_all(&config_dir)
        .map_err(Error::CannotCreateHugoConfigFile)?;

    // Create new config
    let profiles: Vec<String> = website_id.profiles.iter().map(|s| s.clone()).collect();
    let profiles_json = serde_json::to_string(&profiles).unwrap();
    let config = format!("[Params]
  currentProfiles = {}
", profiles_json);

    // Write new config file
    let config_file = config_dir.join("hugo.toml");
    let res = File::create(config_file)
        .map_err(Error::CannotCreateHugoConfigFile)?
        .write_all(&config.as_bytes())
        .map_err(Error::CannotCreateHugoConfigFile)?;

    Ok(res)
}

fn copy_hugo_config_if_needed() -> Result<(), Error> {
    if HUGO_CONFIG_GENERATED.load(Ordering::Relaxed) {
        Ok(())
    } else {
        _copy_hugo_config()
    }
}

fn generate_website(
    id: &WebsiteId,
    destination: &PathBuf,
    generated_websites: &mut MutexGuard<'_, HashSet<PathBuf>>
) -> Result<(), Error> {
    info!("Generating website for {:?}…", id.profiles);
    debug!("Website for {:?} will be generated at <{}>", id.profiles, destination.display());

    copy_hugo_config_if_needed()?;
    gen_hugo_config(id)?;

    let config_dir = HUGO_CONFIG_DIR.display().to_string();
    let environment = id.dir_name();
    let mut params = vec![
        "--disableKinds", "RSS,sitemap",
        "--cleanDestinationDir",
        "--configDir", &config_dir,
        "--environment", &environment,
    ];
    if env::var("LOCALHOST") == Ok("true".to_string()) {
        params.append(&mut vec!["--baseURL", "http://localhost:8080"]);
    }
    let res = hugo_gen(params, destination.display().to_string())
        .map_err(|e| Error::CannotGenerateWebsite(Box::new(e)))?;

    // Temporary fix to avoid leakage of page existence and content
    // TODO(RemiBardon): Find a solution to avoid removing this file
    empty_index_json(destination).map_err(Error::CannotEmptyIndexJson)?;

    generated_websites.insert(destination.clone());

    Ok(res)
}

/// Generate the website
pub fn generate_website_if_needed(website_id: &WebsiteId) -> Result<PathBuf, Error> {
    let website_dir = website_dir(&website_id);

    let mut generated_websites = GENERATED_WEBSITES.lock().unwrap();
    if !generated_websites.contains(&website_dir) {
        generate_website(&website_id, &website_dir, &mut generated_websites)?;
    }

    Ok(website_dir)
}

fn _generate_data_files() -> Result<(), Error> {
    info!("Generating Orangutan data files…");

    // Copy some files if needed
    // FIXME: Do not hardcode "PaperMod"
    let shortcodes_dir = WEBSITE_ROOT.join("themes/PaperMod/layouts/shortcodes");
    let shortcodes_dest_dir_path = format!("themes/{}/layouts/shortcodes", THEME_NAME);
    let shortcodes_dest_dir = WEBSITE_ROOT.join(&shortcodes_dest_dir_path);
    trace!("Copying shortcodes from {} to {}…", shortcodes_dir.display(), shortcodes_dest_dir.display());
    copy_directory(&shortcodes_dir, &shortcodes_dest_dir).unwrap();

    let res = hugo_gen(
        vec!["--disableKinds", "RSS,sitemap,home", "--theme", THEME_NAME],
        WEBSITE_DATA_DIR.display().to_string()
    )?;

    DATA_FILES_GENERATED.store(true, Ordering::Relaxed);

    Ok(res)
}

pub fn generate_data_files_if_needed() -> Result<(), Error> {
    if DATA_FILES_GENERATED.load(Ordering::Relaxed) {
        Ok(())
    } else {
        _generate_data_files()
    }
}

pub fn hugo_gen(params: Vec<&str>, destination: String) -> Result<(), Error> {
    let mut command = Command::new("hugo");

    let website_root = WEBSITE_ROOT.display().to_string();
    let base_params: Vec<&str> = vec![
        "--source", website_root.as_str(),
        "--destination", destination.as_str(),
    ];
    let params = base_params.iter().chain(params.iter());
    command.args(params);

    trace!("Running `{:?}`…", command);
    let output = command
        .output()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(Error::CommandExecutionFailed { command: format!("{:?}", command), output })
    }
}

pub fn hugo(params: Vec<&str>) -> Result<Vec<u8>, Error> {
    let mut _command = Command::new("hugo");

    let website_root = WEBSITE_ROOT.display().to_string();
    let base_params: Vec<&str> = vec![
        "--source", website_root.as_str(),
    ];
    let params = base_params.iter().chain(params.iter());
    let command = _command.args(params);

    trace!("Running `{:?}`…", command);
    let output = command
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if !output.status.success() {
        return Err(Error::CommandExecutionFailed { command: format!("{:?}", command), output })
    }

    Ok(output.stdout.clone())
}

fn empty_index_json(website_dir: &PathBuf) -> Result<(), io::Error> {
    let index_json_path = website_dir.join("index.json");
    // Open the file in write mode, which will truncate the file if it already exists
    let mut file = File::create(index_json_path)?;
    file.write(b"[]")?;
    Ok(())
}

#[derive(Debug)]
pub enum Error {
    CannotExecuteCommand(String, io::Error),
    CommandExecutionFailed { command: String, output: std::process::Output },
    CannotGenerateWebsite(Box<Error>),
    CannotEmptyIndexJson(io::Error),
    CannotCreateHugoConfigFile(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CannotExecuteCommand(command, err) => write!(f, "Could not execute command `{command}`: {err}"),
            Error::CommandExecutionFailed { command, output } => write!(f, "Command `{command}` failed with exit code {:?}\nstdout: {}\nstderr: {}", output.status.code(), String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)),
            Error::CannotGenerateWebsite(err) => write!(f, "Could not generate website: {err}"),
            Error::CannotEmptyIndexJson(err) => write!(f, "Could not empty <index.json> file: {err}"),
            Error::CannotCreateHugoConfigFile(err) => write!(f, "Could create hugo config file: {err}"),
        }
    }
}

#[rocket::async_trait]
impl<'r> Responder<'r, 'static> for Error {
    fn respond_to(self, _: &'r Request<'_>) -> response::Result<'static> {
        let res = self.to_string();
        Response::build()
            .header(ContentType::Plain)
            .sized_body(res.len(), Cursor::new(res))
            .ok()
    }
}
