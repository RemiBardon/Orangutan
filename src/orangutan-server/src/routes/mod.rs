// prose-pod-api
//
// Copyright: 2023–2024, Rémi Bardon <remi@remibardon.name>
// License: Mozilla Public License v2.0 (MPL v2.0)

pub mod debug_routes;
pub mod main_route;
pub mod update_content_routes;

use axum::Router;

use crate::AppState;

pub(super) fn router() -> Router<AppState> {
    Router::<AppState>::new()
        .merge(main_route::router())
        .merge(update_content_routes::router())
        .merge(debug_routes::router())
}

#[cfg(feature = "templating")]
pub(super) fn templates() -> Vec<(&'static str, &'static str)> {
    vec![
        vec![("base.html", include_str!("templates/base.html.tera"))],
        debug_routes::templates(),
    ]
    .concat()
}
