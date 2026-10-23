//! Browser navigation after a completed same-origin authorization POST.
use axum::response::{IntoResponse, Response};
use url::Url;

/// Finish the same-origin POST before navigating to the provider. Browsers apply
/// `form-action` to HTTP redirects, so an external 303 cannot leave this page.
pub(super) fn authorization_navigation(url: &Url, back: &str) -> Response {
    let destination = webapp::html_escape::escape_attr(url.as_str());
    let back = webapp::html_escape::escape_attr(back);
    (
        [
            (axum::http::header::CACHE_CONTROL, "no-store"),
            (axum::http::header::REFERRER_POLICY, "no-referrer"),
            (axum::http::header::CONTENT_SECURITY_POLICY,
             "default-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"),
        ],
        axum::response::Html(format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <meta name=\"referrer\" content=\"no-referrer\">\
             <meta http-equiv=\"refresh\" content=\"0;url={destination}\">\
             <title>Continue authorization</title></head><body>\
             <h1>Opening provider authorization</h1>\
             <p>If the provider does not open, <a rel=\"noreferrer\" href=\"{destination}\">Continue to provider</a>.</p>\
             <p><a href=\"{back}\">Cancel and return to Brand</a></p></body></html>"
        )),
    ).into_response()
}
