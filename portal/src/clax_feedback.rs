//! In-app problem reports through a CLax report queue.
//!
//! A lawyer-tier session on an `/app` page gets the vendored CLax overlay
//! (`/public/vendor/clax/overlay.js`). The overlay needs a reporter token; this
//! module mints one for the signed-in person by signing a request with the
//! queue's service grant. The grant's key stays on the server. The overlay is
//! served from our own origin, so `script-src` stays `'self'`; only
//! `connect-src` names the CLax instance, and only when the queue is
//! configured.
//!
//! Configuration, all from the environment. The feature is off unless
//! `CLAX_FEEDBACK_ENABLED=true`, because the vendored overlay is LGPL-2.1-only:
//! - `CLAX_QUEUE`, `CLAX_QUEUE_TOKEN`, `CLAX_QUEUE_KEY`: the queue ID and the
//!   service grant's `cxq_` token and base64url PKCS#8 Ed25519 key.
//! - `CLAX_INSTANCE`: the CLax origin, `https://clax.dev` by default.
//! - `CLAX_PAGE_ORIGIN`: this deployment's own exact origin, which the queue
//!   lists. Never taken from the request, so a caller cannot name another one.

use axum::extract::Extension;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::pkcs8::DecodePrivateKey;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::session::SessionData;

/// Path of the token route the overlay's `auth` callback calls.
pub const TOKEN_PATH: &str = "/app/api/clax/token";
/// Where the vendored overlay and its loader are served.
pub const LOADER_SRC: &str = "/public/js/clax-feedback.js";

/// The feature flag. Anything but `true` leaves reports off everywhere.
pub const FLAG: &str = "CLAX_FEEDBACK_ENABLED";
const DEFAULT_INSTANCE: &str = "https://clax.dev";
const VERSION_LINE: &str = "clax-ed25519 v2";
/// The instance asks for the signed `t` within 120 seconds of its clock.
const NO_AGENT_HEADERS: &str = "-\n-\n-\n-\n-\n";

/// Whether the deployment opted in with `CLAX_FEEDBACK_ENABLED=true`.
#[must_use]
pub fn enabled() -> bool {
    std::env::var(FLAG).is_ok_and(|v| v.trim() == "true")
}

/// Where the vendored overlay lives under the static mount.
const OVERLAY_DIR: &str = "/public/vendor/clax/";

/// Whether `path` names a file that exists only for the overlay.
fn is_overlay_asset(path: &str) -> bool {
    path == LOADER_SRC || path.starts_with(OVERLAY_DIR)
}

/// Serves the overlay's files only when the flag is on. With it off the
/// LGPL overlay and its loader answer 404, so nothing of it reaches a browser.
pub async fn gate(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if is_overlay_asset(req.uri().path()) && !enabled() {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(req).await
}

/// A configured queue and its service grant.
#[derive(Clone)]
pub struct ClaxConfig {
    pub instance: String,
    pub queue: String,
    pub page_origin: String,
    token: String,
    key: SigningKey,
}

impl ClaxConfig {
    /// The configuration in the environment, or `None` when reports are off
    /// or the grant is unusable (a bad key turns the feature off rather than
    /// failing the page).
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        // Off unless a deployment opts in: the vendored overlay is LGPL-2.1-only.
        if !enabled() {
            return None;
        }
        Self::parse(
            &var("CLAX_INSTANCE").unwrap_or_else(|| DEFAULT_INSTANCE.to_string()),
            &var("CLAX_QUEUE")?,
            &var("CLAX_PAGE_ORIGIN")?,
            &var("CLAX_QUEUE_TOKEN")?,
            &var("CLAX_QUEUE_KEY")?,
        )
    }

    fn parse(instance: &str, queue: &str, page_origin: &str, token: &str, key: &str) -> Option<Self> {
        let instance = instance.trim().trim_end_matches('/');
        if !instance.starts_with("https://") && !instance.starts_with("http://localhost") {
            return None;
        }
        let der = URL_SAFE_NO_PAD.decode(key.trim()).ok()?;
        let key = SigningKey::from_pkcs8_der(&der).ok()?;
        Some(Self {
            instance: instance.to_string(),
            queue: queue.trim().to_string(),
            page_origin: page_origin.trim().trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            key,
        })
    }

    /// The origin `connect-src` admits for the overlay's reporter routes.
    #[must_use]
    pub fn connect_origin(&self) -> &str {
        &self.instance
    }

    fn host(&self) -> &str {
        self.instance
            .split_once("://")
            .map_or(self.instance.as_str(), |(_, rest)| rest)
    }

    /// The `Clax-Signature` value for one request (`clax-ed25519 v2`).
    fn signature(&self, path: &str, now: i64, nonce: &str, body: &[u8]) -> String {
        let hex = |bytes: &[u8]| hex_lower(&Sha256::digest(bytes));
        let canonical = [
            VERSION_LINE,
            "POST",
            path,
            self.host(),
            &now.to_string(),
            nonce,
            &hex(body),
            &hex(self.token.as_bytes()),
            &hex(NO_AGENT_HEADERS.as_bytes()),
        ]
        .join("\n");
        let signed = self.key.sign(canonical.as_bytes());
        format!("t={now}, n={nonce}, s={}", URL_SAFE_NO_PAD.encode(signed.to_bytes()))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[derive(Deserialize)]
struct MintInput {
    public_key: String,
}

#[derive(Serialize)]
struct MintBody<'a> {
    reporter_id: &'a str,
    origin: &'a str,
    public_key: &'a str,
}

fn refuse(status: StatusCode, code: &str) -> Response {
    let mut response = (
        status,
        Json(serde_json::json!({ "error": { "code": code, "message": code } })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `POST /app/api/clax/token {public_key}` — a reporter token for the
/// signed-in lawyer-tier person, bound to their browser's key.
///
/// The reporter ID is the person's `persons.id`, never an email address. A
/// refusal from the instance other than a bad key or a limit is the grant's
/// own fault, so it is answered as `feedback_unavailable` and logged.
async fn mint_token(
    session: Option<Extension<SessionData>>,
    Json(input): Json<MintInput>,
) -> Response {
    let Some(config) = ClaxConfig::from_env() else {
        return refuse(StatusCode::NOT_FOUND, "not_found");
    };
    let Some(Extension(session)) = session else {
        return refuse(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let Some(person_id) = session.person_id.filter(|_| session.role.is_lawyer_tier()) else {
        return refuse(StatusCode::FORBIDDEN, "forbidden");
    };
    if input.public_key.is_empty() || input.public_key.len() > 64 {
        return refuse(StatusCode::BAD_REQUEST, "invalid_key");
    }
    let reporter_id = person_id.to_string();
    let body = serde_json::to_vec(&MintBody {
        reporter_id: &reporter_id,
        origin: &config.page_origin,
        public_key: &input.public_key,
    })
    .expect("a mint body serialises");
    let path = format!("/api/queues/{}/reporter-tokens", config.queue);
    let nonce = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>());
    let signature = config.signature(&path, chrono::Utc::now().timestamp(), &nonce, &body);
    let sent = reqwest::Client::new()
        .post(format!("{}{path}", config.instance))
        .header(header::AUTHORIZATION, format!("Clax-Queue {}", config.token))
        .header("clax-signature", signature)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await;
    let response = match sent {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "clax mint request failed");
            return refuse(StatusCode::SERVICE_UNAVAILABLE, "feedback_unavailable");
        }
    };
    let status = response.status();
    let answer = response.json::<serde_json::Value>().await.ok();
    if status.is_success() {
        if let Some(answer) = answer {
            let mut ok = Json(answer).into_response();
            ok.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            return ok;
        }
    }
    let code = answer
        .as_ref()
        .and_then(|a| a["error"]["code"].as_str())
        .unwrap_or("unknown")
        .to_string();
    if matches!(code.as_str(), "invalid_key" | "limit_reached") {
        let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_REQUEST);
        return refuse(status, &code);
    }
    tracing::warn!(%status, %code, "clax refused the service grant");
    refuse(StatusCode::SERVICE_UNAVAILABLE, "feedback_unavailable")
}

/// The token route, mounted behind the session boundary and the policy gate.
pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new().route(TOKEN_PATH, post(mint_token))
}

/// The loader tag for a lawyer-tier session, or `None` when reports are off.
///
/// The queue, instance and the person's ID are not secret to that person; the
/// CSRF token is the session's own and rides the tag so the loader can call
/// [`TOKEN_PATH`] from a cookie session.
#[must_use]
pub fn loader_tag(session: &SessionData) -> Option<String> {
    let config = ClaxConfig::from_env()?;
    let person_id = session.person_id.filter(|_| session.role.is_lawyer_tier())?;
    Some(format!(
        "<script src=\"{LOADER_SRC}\" defer data-instance=\"{}\" data-queue=\"{}\" \
         data-user=\"{person_id}\" data-csrf=\"{}\"></script>",
        html_attr(&config.instance),
        html_attr(&config.queue),
        html_attr(&session.csrf_token),
    ))
}

fn html_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::pkcs8::EncodePrivateKey;
    use ed25519_dalek::Verifier;

    fn key_b64(seed: u8) -> String {
        let key = SigningKey::from_bytes(&[seed; 32]);
        URL_SAFE_NO_PAD.encode(key.to_pkcs8_der().unwrap().as_bytes())
    }

    fn config() -> ClaxConfig {
        ClaxConfig::parse(
            "https://clax.dev/",
            "Q1",
            "https://staging.example.com",
            "cxq_token",
            &key_b64(7),
        )
        .expect("a valid grant parses")
    }

    #[test]
    fn a_signature_verifies_over_the_documented_canonical_lines() {
        let c = config();
        let body = br#"{"reporter_id":"p"}"#;
        let header = c.signature("/api/queues/Q1/reporter-tokens", 1_700_000_000, "AAAAAAAAAAAAAAAAAAAAAA", body);
        let s = header.rsplit("s=").next().unwrap();
        let sig = ed25519_dalek::Signature::from_slice(&URL_SAFE_NO_PAD.decode(s).unwrap()).unwrap();
        let hex = |b: &[u8]| hex_lower(&Sha256::digest(b));
        let canonical = format!(
            "clax-ed25519 v2\nPOST\n/api/queues/Q1/reporter-tokens\nclax.dev\n1700000000\nAAAAAAAAAAAAAAAAAAAAAA\n{}\n{}\n{}",
            hex(body),
            hex(b"cxq_token"),
            hex(b"-\n-\n-\n-\n-\n"),
        );
        c.key.verifying_key().verify(canonical.as_bytes(), &sig).unwrap();
        assert!(header.starts_with("t=1700000000, n=AAAAAAAAAAAAAAAAAAAAAA, s="));
    }

    #[tokio::test]
    async fn the_overlay_is_not_served_with_the_flag_off() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let app = Router::new()
            .route("/public/vendor/clax/overlay.js", axum::routing::get(|| async { "x" }))
            .route("/public/js/other.js", axum::routing::get(|| async { "y" }))
            .layer(axum::middleware::from_fn(gate));
        let get = |path: &str| Request::builder().uri(path.to_string()).body(Body::empty()).unwrap();
        // The flag is unset in tests; the overlay and loader are withheld, nothing else is.
        assert_eq!(app.clone().oneshot(get("/public/vendor/clax/overlay.js")).await.unwrap().status(), 404);
        assert_eq!(app.clone().oneshot(get(LOADER_SRC)).await.unwrap().status(), 404);
        assert_eq!(app.oneshot(get("/public/js/other.js")).await.unwrap().status(), 200);
    }

    #[test]
    fn an_unusable_grant_turns_the_feature_off() {
        assert!(ClaxConfig::parse("https://clax.dev", "Q1", "https://x", "t", "not-a-key").is_none());
        assert!(ClaxConfig::parse("http://clax.dev", "Q1", "https://x", "t", &key_b64(1)).is_none());
    }

    #[test]
    fn attribute_values_are_html_escaped() {
        assert_eq!(html_attr("a\"<b>&"), "a&quot;&lt;b&gt;&amp;");
    }
}
