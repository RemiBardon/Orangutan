use std::{
    collections::HashSet,
    env,
    fs::{self, File},
    io::{self, BufRead as _, BufReader, Write},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use lazy_static::lazy_static;
use tracing::{debug, error, info, trace};

use crate::{config::*, copy_directory, website_id::*};

static HUGO_CONFIG_GENERATED: AtomicBool = AtomicBool::new(false);
static DATA_FILES_GENERATED: AtomicBool = AtomicBool::new(false);

lazy_static! {
    // NOTE: `Arc` prevents race conditions
    static ref GENERATED_WEBSITES: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    static ref TRASH_DIR: PathBuf = TMP_DIR.join("trash");
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
        return pull_repository();
    }

    _clone_repository()?;
    _init_submodules()?;
    Ok(())
}

fn _clone_repository() -> Result<(), Error> {
    let mut command = Command::new("git");
    command
        .args(vec![
            "clone",
            &WEBSITE_REPOSITORY,
            &WEBSITE_ROOT.display().to_string(),
        ])
        .args(vec!["--depth", "1"]);

    trace!("Running `{:?}`…", command);
    let status = command
        .status()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::CommandExecutionFailed {
            command: format!("{:?}", command),
            code: status.code(),
        })
    }
}

fn _init_submodules() -> Result<(), Error> {
    let mut command = Command::new("git");
    command
        .args(vec!["-C", &WEBSITE_ROOT.display().to_string()])
        .args(vec!["submodule", "update", "--init"]);

    trace!("Running `{:?}`…", command);
    let status = command
        .status()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::CommandExecutionFailed {
            command: format!("{:?}", command),
            code: status.code(),
        })
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
        .args(vec!["pull", "--rebase"]);

    trace!("Running `{:?}`…", command);
    let status = command
        .status()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::CommandExecutionFailed {
            command: format!("{:?}", command),
            code: status.code(),
        })
    }
}

fn _update_submodules() -> Result<(), Error> {
    let mut command = Command::new("git");
    command
        .args(vec!["-C", &WEBSITE_ROOT.display().to_string()])
        .args(vec!["submodule", "update", "--remote", "--recursive"]);

    trace!("Running `{:?}`…", command);
    let status = command
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::CommandExecutionFailed {
            command: format!("{:?}", command),
            code: status.code(),
        })
    }
}

fn read_file_lines_as_hex_(file: File) -> io::Result<HashSet<Vec<u8>>> {
    let reader = BufReader::new(file);

    let mut set = HashSet::new();

    for line in reader.lines() {
        let line = line?;
        if !line.is_empty() {
            set.insert(hex::decode(&line).unwrap_or_else(|err| {
                error!("Could not parse `{line}` as hexadecimal: {err}");
                line.into_bytes()
            }));
        }
    }

    Ok(set)
}

// NOTE: This is just a hotfix. I had to quickly revoke a token. I'll improve this one day.
pub fn read_revoked_tokens() -> Result<HashSet<Vec<u8>>, Error> {
    let revoked_tokens_file_path = WEBSITE_ROOT.join("revoked_tokens.txt");
    let Ok(revoked_tokens_file) = File::open(&revoked_tokens_file_path) else {
        info!(
            "Revoked tokens file not found at <{}>. Considering no revoked token.",
            revoked_tokens_file_path.display(),
        );
        return Ok(HashSet::new());
    };
    let revoked_tokens = read_file_lines_as_hex_(revoked_tokens_file)?;
    info!("Found {} revoked token(s).", revoked_tokens.len());
    Ok(revoked_tokens)
}

fn _copy_hugo_config() -> Result<(), Error> {
    debug!("Copying hugo config…");

    // Copy config dir
    // TODO: Support config that is not directory-based
    let source = WEBSITE_ROOT.join("config");
    let dest = HUGO_CONFIG_DIR.join("_default");
    copy_directory(source.as_path(), dest.as_path()).map_err(Error::CannotCreateHugoConfigFile)?;
    debug!("Hugo config will be saved in <{}>", &dest.display());

    HUGO_CONFIG_GENERATED.store(true, Ordering::Relaxed);

    Ok(())
}

fn gen_hugo_config(website_id: &WebsiteId) -> Result<(), Error> {
    // Create config dir
    let config_dir = HUGO_CONFIG_DIR.join(website_id.dir_name());
    fs::create_dir_all(&config_dir).map_err(Error::CannotCreateHugoConfigFile)?;

    // Create new config
    let profiles: Vec<String> = website_id.profiles.iter().map(|s| s.clone()).collect();
    let profiles_json = serde_json::to_string(&profiles).unwrap();
    let config = format!(
        "[Params]
  currentProfiles = {}
",
        profiles_json
    );

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
    generated_websites: &mut MutexGuard<'_, HashSet<PathBuf>>,
) -> Result<(), Error> {
    info!("Generating website for {:?}…", id.profiles);
    debug!(
        "Website for {:?} will be generated at <{}>",
        id.profiles,
        destination.display()
    );

    copy_hugo_config_if_needed()?;
    gen_hugo_config(id)?;

    let config_dir = HUGO_CONFIG_DIR.display().to_string();
    let environment = id.dir_name();
    let mut params = vec![
        "--disableKinds",
        "RSS,sitemap",
        "--cleanDestinationDir",
        "--configDir",
        &config_dir,
        "--environment",
        &environment,
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
    trace!(
        "Copying shortcodes from {} to {}…",
        shortcodes_dir.display(),
        shortcodes_dest_dir.display()
    );
    copy_directory(&shortcodes_dir, &shortcodes_dest_dir).unwrap();

    let res = hugo_gen(
        vec![
            "--disableKinds",
            "RSS,sitemap,home",
            "--theme",
            THEME_NAME,
        ],
        WEBSITE_DATA_DIR.display().to_string(),
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

pub fn hugo_gen(
    params: Vec<&str>,
    destination: String,
) -> Result<(), Error> {
    let website_root = WEBSITE_ROOT.display().to_string();
    let base_params: Vec<&str> = vec![
        "--source",
        website_root.as_str(),
        "--destination",
        destination.as_str(),
    ];
    hugo(
        base_params.into_iter().chain(params.into_iter()).collect(),
        false,
    )?;

    Ok(())
}

fn hugo(
    params: Vec<&str>,
    pipe_stdout: bool,
) -> Result<Output, Error> {
    let mut command = Command::new("hugo");

    let website_root = WEBSITE_ROOT.display().to_string();
    let base_params: Vec<&str> = vec!["--source", website_root.as_str()];
    let params = base_params.iter().chain(params.iter());
    command.args(params);

    // `Stdio::piped()` is the default when using `.output()`,
    // so we must override it the other way around
    command.stderr(Stdio::inherit());
    if !pipe_stdout {
        command.stdout(Stdio::inherit());
    }

    trace!("Running `{:?}`…", command);
    let output = command
        .output()
        .map_err(|e| Error::CannotExecuteCommand(format!("{:?}", command), e))?;

    if output.status.success() {
        Ok(output.clone())
    } else {
        Err(Error::CommandExecutionFailed {
            command: format!("{:?}", command),
            code: output.status.code(),
        })
    }
}

fn empty_index_json(website_dir: &PathBuf) -> Result<(), io::Error> {
    let index_json_path = website_dir.join("index.json");
    // Open the file in write mode, which will truncate the file if it already exists
    let mut file = File::create(index_json_path)?;
    file.write(b"[]")?;
    Ok(())
}

pub struct State {
    hugo_config_generated: bool,
    data_files_generated: bool,
    generated_websites: HashSet<PathBuf>,
    used_profiles: Option<&'static HashSet<String>>,
}

pub fn create_tmp_dir() -> Result<(), Error> {
    trace!("Creating temporary directory at <{}>…", TMP_DIR.display());
    fs::create_dir_all(TMP_DIR.as_path())?;

    Ok(())
}

pub fn trash_outdated_websites() -> Result<State, Error> {
    trace!("Trashing outdated websites…");

    // Empty the trash (it's at least `HEAD~2` so we can safely delete it)
    // NOTE: This whould not be necessary since the directory should be deleted
    //   but there might be edge cases where it's still there and the next
    //   `fs::rename` will fail if it's the case.
    if TRASH_DIR.exists() {
        fs::remove_dir_all(TRASH_DIR.as_path())?;
    }

    // Remove outdated websites
    fs::rename(DEST_DIR.as_path(), TRASH_DIR.as_path())?;

    // Save caches (in case we need to recover)
    let state = State {
        hugo_config_generated: HUGO_CONFIG_GENERATED.load(Ordering::Relaxed),
        data_files_generated: DATA_FILES_GENERATED.load(Ordering::Relaxed),
        generated_websites: GENERATED_WEBSITES.lock().unwrap().to_owned(),
        used_profiles: super::USED_PROFILES.lock().unwrap().to_owned(),
    };

    // Clear caches
    HUGO_CONFIG_GENERATED.store(false, Ordering::Relaxed);
    DATA_FILES_GENERATED.store(false, Ordering::Relaxed);
    GENERATED_WEBSITES.lock().unwrap().clear();
    *super::USED_PROFILES.lock().unwrap() = None;

    Ok(state)
}

pub fn recover_trash(state: State) -> Result<(), Error> {
    trace!("Recovering trash…");

    // Reload files
    fs::rename(TRASH_DIR.as_path(), DEST_DIR.as_path())?;

    // Relaod caches
    HUGO_CONFIG_GENERATED.store(state.hugo_config_generated, Ordering::Relaxed);
    DATA_FILES_GENERATED.store(state.data_files_generated, Ordering::Relaxed);
    *GENERATED_WEBSITES.lock().unwrap() = state.generated_websites;
    *super::USED_PROFILES.lock().unwrap() = state.used_profiles;

    Ok(())
}

/// NOTE: Needs a `State` to take ownership and make sure we don't keep outdated information.
pub fn empty_trash(_state: State) -> Result<(), Error> {
    trace!("Emptying trash…");

    fs::remove_dir_all(TRASH_DIR.as_path())?;

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Could not execute command `{0}`: {1}")]
    CannotExecuteCommand(String, io::Error),
    #[error("Command `{command}` failed with exit code {code:?}")]
    CommandExecutionFailed { command: String, code: Option<i32> },
    #[error("Could not generate website: {0}")]
    CannotGenerateWebsite(Box<Error>),
    #[error("Could not empty <index.json> file: {0}")]
    CannotEmptyIndexJson(io::Error),
    #[error("Could create hugo config file: {0}")]
    CannotCreateHugoConfigFile(io::Error),
    #[error("IO error: {0}")]
    IOError(#[from] io::Error),
}
