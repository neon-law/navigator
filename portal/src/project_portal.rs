//! `/app/projects/{code}/portal` — the retired Portal mount.
//!
//! A Project once carried a Vite bundle built from its repository's `portal/`
//! directory, published to the applications bucket and streamed here. Every
//! Project repository now commits only source — `navigator.yaml`, Markdown, and
//! YAML — and the matter show page renders its declared Matter Pages directly.
//! Nothing publishes a bundle any more, so nothing here reads one: this module
//! holds no storage handle at all, which is what keeps a bundle left behind in
//! the bucket from ever reaching a browser again.
//!
//! # Every Portal path redirects to the matter show page
//!
//! The bare mount, its trailing-slash root, and every path beneath it answer
//! `301 Moved Permanently` to `/app/projects/{code}`. A redirect rather than a
//! 404 because these URLs were sent to clients — in email, in Slack, in
//! bookmarks — and the matter show page is where each of them now belongs.
//!
//! The redirect discloses nothing: it is computed from the request path alone,
//! before any Project is looked up, and the show page it lands on runs its own
//! participation check. A code that cannot name a Project is answered with a
//! plain 404 instead, so the `Location` header is only ever a well-formed
//! `/app/projects/<code>` path.

use axum::extract::Path;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

/// The retired Portal mount.
///
/// `portal` is a literal rather than a parameter, so this registration beats
/// the declared Matter Page route at `/app/projects/{project_code}/{page_slug}`
/// and a page can never be served under the old mount.
pub const PROJECT_PORTAL_PATH: &str = "/app/projects/{project_code}/portal";

/// The trailing-slash root the old bundle was served from.
const PROJECT_PORTAL_ROOT: &str = "/app/projects/{project_code}/portal/";

/// Any path beneath the retired mount — a former bundle asset or page.
const PROJECT_PORTAL_ASSET: &str = "/app/projects/{project_code}/portal/{*asset}";

/// Mount the retired Portal paths.
///
/// The auth and policy layers match every other `/app` surface, so an
/// anonymous browser still crosses the login door first and lands on the show
/// page after signing in.
pub fn router(
    sessions: crate::session::SessionStore,
    policy: crate::policy::PolicyClient,
    auth: crate::auth::AuthConfig,
) -> Router {
    Router::new()
        .route(PROJECT_PORTAL_PATH, get(redirect_to_matter))
        .route(PROJECT_PORTAL_ROOT, get(redirect_to_matter))
        .route(PROJECT_PORTAL_ASSET, get(redirect_asset_to_matter))
        .route_layer(axum::middleware::from_fn_with_state(
            (sessions, policy),
            crate::policy::require_policy,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::require_auth,
        ))
}

async fn redirect_to_matter(Path(code): Path<String>) -> Response {
    matter_redirect(&code)
}

async fn redirect_asset_to_matter(Path((code, _asset)): Path<(String, String)>) -> Response {
    matter_redirect(&code)
}

/// The matter show page path a retired Portal URL redirects to, or `None`
/// when the code cannot name a Project.
fn matter_path(code: &str) -> Option<String> {
    store::projects::is_valid_code(code).then(|| format!("/app/projects/{code}"))
}

fn matter_redirect(code: &str) -> Response {
    match matter_path(code).and_then(|target| HeaderValue::from_str(&target).ok()) {
        Some(location) => (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, location)],
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{matter_path, PROJECT_PORTAL_ASSET, PROJECT_PORTAL_PATH, PROJECT_PORTAL_ROOT};
    use cloud::workspace::PORTAL_MOUNT_SEGMENT;

    /// The retired mount still differs in shape from the matter show page, so
    /// the redirect can never shadow the page it points at.
    #[test]
    fn the_retired_mount_cannot_shadow_the_matter_show_page() {
        assert_eq!(PROJECT_PORTAL_PATH, "/app/projects/{project_code}/portal");
        assert_eq!(
            crate::dioxus_app::PROJECT_DETAIL_PATH,
            "/app/projects/{project_code}"
        );
        assert_eq!(
            PROJECT_PORTAL_PATH.rsplit('/').next(),
            Some(PORTAL_MOUNT_SEGMENT)
        );
        assert_eq!(PROJECT_PORTAL_ROOT, format!("{PROJECT_PORTAL_PATH}/"));
        assert_eq!(
            PROJECT_PORTAL_ASSET,
            format!("{PROJECT_PORTAL_PATH}/{{*asset}}")
        );
    }

    /// Only a well-formed Project code becomes a `Location`.
    #[test]
    fn only_a_valid_code_becomes_a_redirect_target() {
        assert_eq!(
            matter_path("libra-formation").as_deref(),
            Some("/app/projects/libra-formation")
        );
        for bad in ["", "Not_A_Code", "..", "a/b", "new"] {
            assert_eq!(matter_path(bad), None, "{bad:?}");
        }
    }
}
