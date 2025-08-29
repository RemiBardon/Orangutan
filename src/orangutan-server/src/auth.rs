use std::time::SystemTime;

use biscuit_auth::macros::authorizer;
use orangutan_helpers::{config::DEFAULT_PROFILE, ReadAllowed};
use tracing::trace;

use crate::request_guards::Token;

pub fn is_authorized(
    token: Option<Token>,
    allowed_profiles: ReadAllowed,
) -> bool {
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
            // trace!(
            //     "Running authorizer '{}' on '{}'…",
            //     authorizer.dump_code(),
            //     biscuit.authorizer().unwrap().dump_code()
            // );
            if biscuit.authorize(&authorizer).is_ok() {
                profile = Some(allowed_profile);
            }
        }
    }

    profile.is_some()
}
