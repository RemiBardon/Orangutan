use std::{path::Path, time::SystemTime};

use biscuit_auth::macros::authorizer;
use object_reader::{ObjectReader, ReadObjectResponse};
use orangutan_helpers::{data_file, read_allowed, readers::object_reader, website_id::WebsiteId};
use rocket::{get, http::uri::Origin, routes, Route, State};
use tracing::{debug, trace};

use crate::{config::*, request_guards::Token, util::error};

pub(super) fn routes() -> Vec<Route> {
    routes![handle_request]
}

#[get("/<_..>")]
async fn handle_request(
    origin: &Origin<'_>,
    token: Option<Token>,
    object_reader: &State<ObjectReader>,
) -> Result<Option<ReadObjectResponse>, crate::Error> {
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

fn allowed_profiles<'r>(path: &String) -> Option<Vec<String>> {
    let path = path.rsplit_once("@").unwrap_or((path, "")).0;
    let data_file = data_file(&Path::new(path).to_path_buf());
    read_allowed(&data_file)
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

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Error when listing objects matching '{path}': {err}")]
    CannotListObjects {
        path: String,
        err: object_reader::Error,
    },
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
}
