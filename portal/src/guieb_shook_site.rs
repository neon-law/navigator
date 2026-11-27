//! The Guieb & Shook website, served from the firm's approved static export.
//!
//! The firm prepares its website outside Navigator and approves it file by file, so on the Guieb & Shook host this
//! layer serves every page, stylesheet, script, font and image byte for byte from the folder named by
//! [`SITE_DIR_ENV`]. Navigator adds nothing to those pages and makes one substitution: a signed-in visitor's
//! "Sign in" link reads "Projects", the same switch [`crate::dioxus_app::public_utility_links`] makes on every
//! Navigator-rendered page.
//!
//! A page's address has no `.html`: `/attorneys/waleed-amer` answers with `attorneys/waleed-amer.html`, and a
//! request that names the file redirects to that address.
//!
//! A path with no file in the folder falls through to Navigator, so `/auth`, `/app` and every other route of its
//! own keep working. Two exceptions sit between: the site's videos stay out of Git and the image and live in the
//! public assets bucket under [`VIDEO_BUCKET_PREFIX`], answered at their site address; and the addresses the
//! brand's Navigator-rendered pages used redirect to the matching static page. Unset, the layer passes every
//! request through and the brand renders its Navigator pages.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use tower::ServiceExt as _;
use tower_http::services::ServeFile;
use views::brand::BrandKey;

use crate::session::{SessionStore, SESSION_COOKIE_NAME};

/// The folder holding the export: `index.html` at its root, the pages and `assets/` beneath.
pub const SITE_DIR_ENV: &str = "NAVIGATOR_GUIEB_SHOOK_SITE_DIR";

/// Site address of the videos; a video not in the folder is read from the bucket.
const VIDEO_PREFIX: &str = "/assets/video/";

/// Where the site's videos live in the public assets bucket, as the bucket route addresses them.
pub const VIDEO_BUCKET_PREFIX: &str = "/assets/img/guieb-shook/site/video/";

const SIGN_IN: &str = r#"<a href="/auth/login">Sign in</a>"#;
const PROJECTS: &str = r#"<a href="/app/projects">Projects</a>"#;

/// The export's location and the session store that tells a signed-in visitor apart.
#[derive(Clone)]
pub struct GuiebShookSite {
    root: Arc<PathBuf>,
    sessions: SessionStore,
    /// Navigator's public-assets bucket route, which answers a video at its bucket address.
    bucket: Router,
}

impl GuiebShookSite {
    /// The site named by [`SITE_DIR_ENV`], or `None` when it is unset or holds no `index.html`.
    #[must_use]
    pub fn from_env(sessions: SessionStore, bucket: Router) -> Option<Self> {
        let root = PathBuf::from(std::env::var_os(SITE_DIR_ENV)?);
        if !root.join("index.html").is_file() {
            tracing::warn!(
                ?root,
                "Guieb & Shook site folder has no index.html; serving the Navigator pages"
            );
            return None;
        }
        tracing::info!(
            ?root,
            "serving the Guieb & Shook site from its static export"
        );
        Some(Self::new(root, sessions, bucket))
    }

    #[must_use]
    pub fn new(root: PathBuf, sessions: SessionStore, bucket: Router) -> Self {
        Self {
            root: Arc::new(root),
            sessions,
            bucket,
        }
    }

    /// The file a request path names, if the folder holds it: `/` is `index.html`, `/our-story` is
    /// `our-story.html`, any other file answers at its own name except a page, and a path with an empty, hidden,
    /// `.` or `..` segment names nothing.
    fn file_for(&self, path: &str) -> Option<PathBuf> {
        let relative = path.strip_prefix('/')?;
        if relative.is_empty() {
            let index = self.root.join("index.html");
            return index.is_file().then_some(index);
        }
        let safe = relative.split('/').all(|segment| {
            !segment.is_empty() && !segment.starts_with('.') && !segment.contains('\\')
        });
        if !safe {
            return None;
        }
        let page = self.root.join(format!("{relative}.html"));
        if page.is_file() {
            return Some(page);
        }
        let file = self.root.join(relative);
        (file.is_file() && !is_page(&file)).then_some(file)
    }

    /// The address of a page requested by its file name: `/our-story.html` is `/our-story`, `/index.html` is `/`.
    fn page_address<'a>(&self, path: &'a str) -> Option<&'a str> {
        let address = match path.strip_suffix(".html")? {
            "/index" => "/",
            stem => stem,
        };
        self.file_for(address)
            .is_some_and(|file| is_page(&file))
            .then_some(address)
    }

    fn signed_in(&self, req: &Request) -> bool {
        req.extensions()
            .get::<tower_cookies::Cookies>()
            .and_then(|cookies| cookies.get(SESSION_COOKIE_NAME))
            .and_then(|cookie| self.sessions.decode(cookie.value()))
            .is_some_and(|session| {
                !session.is_expired() && !session.is_legacy_unscoped_ci() && session.scope.is_none()
            })
    }

    /// Where a former address of the brand's Navigator pages now lives in the static site.
    fn former_address(path: &str) -> Option<&'static str> {
        let under = |prefix: &str| path == prefix || path.starts_with(&format!("{prefix}/"));
        Some(match path {
            "/contact/sent" | "/leads" => "/contact",
            "/team/waleed" => "/attorneys/waleed-amer",
            "/team/nick" => "/attorneys/nicholas-shook",
            "/privacy" => "/legal/privacy-policy",
            "/terms" => "/legal/disclaimer",
            "/services" | "/testimonials" | "/llms.txt" => "/",
            _ if under("/team") => "/#attorneys",
            _ if under("/blog") || under("/design") || under("/templates") => "/",
            _ => return None,
        })
    }

    async fn page(&self, file: &Path, signed_in: bool) -> Response {
        let Ok(html) = tokio::fs::read_to_string(file).await else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let html = if signed_in {
            html.replace(SIGN_IN, PROJECTS)
        } else {
            html
        };
        let csp = HeaderValue::from_str(&page_policy(&html))
            .expect("the policy is built from ASCII hashes");
        (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                ),
                // The Sign in link depends on the visitor, so no shared cache may keep a page.
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("private, no-cache"),
                ),
                (header::VARY, HeaderValue::from_static("Cookie")),
                (header::CONTENT_SECURITY_POLICY, csp),
            ],
            html,
        )
            .into_response()
    }
}

/// Serve the Guieb & Shook site on its host; pass everything else to Navigator.
pub async fn serve(
    State(site): State<Option<GuiebShookSite>>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(site) = site.filter(|_| {
        req.extensions().get::<BrandKey>() == Some(&BrandKey::GuiebShook)
            && matches!(*req.method(), Method::GET | Method::HEAD)
    }) else {
        return next.run(req).await;
    };
    let path = req.uri().path().to_owned();
    if let Some(address) = site.page_address(&path) {
        let target = match req.uri().query() {
            Some(query) => format!("{address}?{query}"),
            None => address.to_string(),
        };
        return (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, target)]).into_response();
    }
    if let Some(file) = site.file_for(&path) {
        if is_page(&file) {
            // Decided before the read: a borrowed request is not `Send` across the await.
            let signed_in = site.signed_in(&req);
            return site.page(&file, signed_in).await;
        }
        let mut response = match ServeFile::new(file).oneshot(req).await {
            Ok(response) => response.map(Body::new),
            Err(never) => match never {},
        };
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, crate::STATIC_CACHE_CONTROL);
        return response;
    }
    if let Some(video) = path.strip_prefix(VIDEO_PREFIX).filter(|name| {
        Path::new(name)
            .extension()
            .is_some_and(|extension| extension == "mp4")
            && !name.contains('/')
            && !name.starts_with('.')
    }) {
        let Ok(uri) = Uri::try_from(format!("{VIDEO_BUCKET_PREFIX}{video}")) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        // A fresh request, not this one: Navigator's own `/assets/{*key}` route has already matched it, and the
        // path parameters that match left in its extensions would reach the bucket route's `Path` extractor beside
        // its own, which then refuses the request (500).
        let (parts, body) = req.into_parts();
        let mut bucket_req = Request::new(body);
        *bucket_req.method_mut() = parts.method;
        *bucket_req.uri_mut() = uri;
        *bucket_req.version_mut() = parts.version;
        *bucket_req.headers_mut() = parts.headers;
        return match site.bucket.oneshot(bucket_req).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
    }
    if let Some(target) = GuiebShookSite::former_address(&path) {
        return (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, target)]).into_response();
    }
    next.run(req).await
}

fn is_page(file: &Path) -> bool {
    file.extension()
        .is_some_and(|extension| extension == "html")
}

/// The page's Content-Security-Policy: same-origin everything, the page's own inline scripts and event handlers by
/// hash, and Plausible, which the approved pages load themselves.
fn page_policy(html: &str) -> String {
    let hash = |code: &str| {
        let digest = Sha256::digest(code.as_bytes());
        format!(
            " 'sha256-{}'",
            base64::engine::general_purpose::STANDARD.encode(digest)
        )
    };
    let mut scripts = String::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let after = &rest[start..];
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let Some(close) = after.find("</script>") else {
            break;
        };
        if !after[..tag_end].contains(" src=") {
            scripts.push_str(&hash(&after[tag_end + 1..close]));
        }
        rest = &after[close..];
    }
    let mut handlers: Vec<&str> = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find(" on") {
        let after = &rest[start + 3..];
        let name_len = after.chars().take_while(char::is_ascii_lowercase).count();
        rest = after;
        if name_len > 0 && after[name_len..].starts_with("=\"") {
            let value = &after[name_len + 2..];
            if let Some(end) = value.find('"') {
                if !handlers.contains(&&value[..end]) {
                    handlers.push(&value[..end]);
                }
            }
        }
    }
    if !handlers.is_empty() {
        scripts.push_str(" 'unsafe-hashes'");
        for handler in handlers {
            scripts.push_str(&hash(handler));
        }
    }
    format!(
        "default-src 'self'; base-uri 'self'; object-src 'none'; frame-ancestors 'none'; img-src 'self' data:; \
         font-src 'self'; media-src 'self'; style-src 'self' 'unsafe-inline'; \
         script-src 'self'{scripts} https://plausible.io; connect-src 'self' https://plausible.io; \
         form-action 'self' mailto:"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;

    const HOME: &str = "<!doctype html><html><head><script>go()</script>\
        <script src=\"/assets/js/plausible.js\" defer></script></head><body>\
        <div class=\"nav-item\"><a href=\"/auth/login\">Sign in</a></div>\
        <img src=\"a.jpg\" onerror=\"this.remove()\"></body></html>";

    fn site_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), HOME).unwrap();
        std::fs::create_dir_all(dir.path().join("practice")).unwrap();
        std::fs::write(dir.path().join("practice/real-estate.html"), HOME).unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("assets/styles.css"), "body{}").unwrap();
        std::fs::write(dir.path().join(".hidden"), "secret").unwrap();
        dir
    }

    fn sessions() -> SessionStore {
        SessionStore::new(b"test-key-test-key-test-key-test-key".to_vec())
    }

    fn app(root: &Path, brand: BrandKey) -> Router {
        let bucket = Router::new().route(
            "/assets/{*key}",
            get(|axum::extract::Path(key): axum::extract::Path<String>| async move { key }),
        );
        let site = Some(GuiebShookSite::new(root.to_path_buf(), sessions(), bucket));
        Router::new()
            .route("/auth/login", get(|| async { "navigator sign-in" }))
            // As in production, Navigator's own asset route matches a video's site address before this layer runs.
            .route("/assets/{*key}", get(|| async { "navigator asset" }))
            .layer(axum::middleware::from_fn_with_state(site, serve))
            .layer(tower_cookies::CookieManagerLayer::new())
            .layer(axum::middleware::from_fn(
                move |mut req: Request, next: Next| async move {
                    req.extensions_mut().insert(brand);
                    next.run(req).await
                },
            ))
    }

    async fn get_text(
        app: &Router,
        path: &str,
        cookie: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let mut builder = Request::get(path);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn the_site_is_served_byte_for_byte_with_its_own_policy() {
        let dir = site_dir();
        let app = app(dir.path(), BrandKey::GuiebShook);
        let (status, headers, body) = get_text(&app, "/", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, HOME);
        let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        let go = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(b"go()"));
        assert!(csp.contains(&format!("'sha256-{go}'")), "{csp}");
        assert!(
            csp.contains("'unsafe-hashes'") && csp.contains("https://plausible.io"),
            "{csp}"
        );
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-cache");
        let (status, headers, body) = get_text(&app, "/assets/styles.css", None).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "body{}"));
        assert_eq!(headers[header::CACHE_CONTROL], crate::STATIC_CACHE_CONTROL);
    }

    #[tokio::test]
    async fn pages_answer_without_html_and_their_file_names_redirect_there() {
        let dir = site_dir();
        let app = app(dir.path(), BrandKey::GuiebShook);
        let (status, headers, body) = get_text(&app, "/practice/real-estate", None).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, HOME));
        assert!(headers.contains_key(header::CONTENT_SECURITY_POLICY));
        for (from, to) in [
            ("/practice/real-estate.html", "/practice/real-estate"),
            ("/index.html", "/"),
            ("/index.html?type=first", "/?type=first"),
        ] {
            let (status, headers, _) = get_text(&app, from, None).await;
            assert_eq!(
                (status, headers[header::LOCATION].to_str().unwrap()),
                (StatusCode::MOVED_PERMANENTLY, to),
                "{from}"
            );
        }
    }

    #[tokio::test]
    async fn a_signed_in_visitor_sees_projects_in_place_of_sign_in() {
        let dir = site_dir();
        let app = app(dir.path(), BrandKey::GuiebShook);
        let session =
            crate::session::SessionData::fresh("waleed@example.com", store::persons::Role::Lawyer);
        let cookie = format!("{SESSION_COOKIE_NAME}={}", sessions().encode(&session));
        let (_, _, body) = get_text(&app, "/", Some(&cookie)).await;
        assert_eq!(body, HOME.replace(SIGN_IN, PROJECTS));
        let (_, _, body) =
            get_text(&app, "/", Some(&format!("{SESSION_COOKIE_NAME}=forged"))).await;
        assert_eq!(body, HOME);
    }

    #[tokio::test]
    async fn navigator_routes_videos_and_former_addresses_pass_or_redirect() {
        let dir = site_dir();
        let app = app(dir.path(), BrandKey::GuiebShook);
        assert_eq!(
            get_text(&app, "/auth/login", None).await.2,
            "navigator sign-in"
        );
        let (status, _, key) = get_text(&app, "/assets/video/hero-1.mp4", None).await;
        assert_eq!(status, StatusCode::OK, "{key}");
        assert_eq!(
            format!("/assets/{key}"),
            format!("{VIDEO_BUCKET_PREFIX}hero-1.mp4")
        );
        for (from, to) in [
            ("/contact/sent", "/contact"),
            ("/team", "/#attorneys"),
            ("/team/nick", "/attorneys/nicholas-shook"),
            ("/privacy", "/legal/privacy-policy"),
            ("/blog/a-post", "/"),
        ] {
            let (status, headers, _) = get_text(&app, from, None).await;
            assert_eq!(
                (status, headers[header::LOCATION].to_str().unwrap()),
                (StatusCode::MOVED_PERMANENTLY, to)
            );
        }
        for unsafe_path in [
            "/.hidden",
            "/practice/../index.html",
            "/practice/missing",
            "/practice/missing.html",
            "/practice/real-estate/",
        ] {
            assert_eq!(
                get_text(&app, unsafe_path, None).await.0,
                StatusCode::NOT_FOUND,
                "{unsafe_path}"
            );
        }
    }

    #[tokio::test]
    async fn other_brands_never_see_the_site() {
        let dir = site_dir();
        let app = app(dir.path(), BrandKey::Neon);
        assert_eq!(get_text(&app, "/", None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(
            get_text(&app, "/contact", None).await.0,
            StatusCode::NOT_FOUND
        );
    }
}
