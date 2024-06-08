// prose-pod-api
//
// Copyright: 2023–2024, Rémi Bardon <remi@remibardon.name>
// License: Mozilla Public License v2.0 (MPL v2.0)

pub mod auth_routes;
pub mod debug_routes;
pub mod main_route;
pub mod update_content_routes;

use rocket::Route;

pub(super) fn routes() -> Vec<Route> {
    vec![
        main_route::routes(),
        auth_routes::routes(),
        update_content_routes::routes(),
        debug_routes::routes(),
    ]
    .concat()
}

#[cfg(feature = "templating")]
pub(super) fn templates() -> Vec<(&'static str, &'static str)> {
    vec![
        vec![("base.html", include_str!("templates/base.html.tera"))],
        debug_routes::templates(),
    ]
    .concat()
}
