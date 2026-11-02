//! Per-host sign-in for brand hosts.
//!
//! Every identity provider knows one callback, `OAUTH_REDIRECT_URI`. A brand
//! host such as `iep.neonlaw.com` still keeps its own session: its
//! `/auth/login` sends the visitor to the callback origin with the host it came
//! from, and the callback origin hands a verified sign-in back to that host.
//! No provider console needs one redirect URI per host.
//!
//! The four steps:
//!
//! 1. The brand host's `/auth/login` sets a host-only nonce cookie and 303s to
//!    the callback origin with `return_host` and `handoff_nonce`
//!    ([`brand_host_login_redirect`]).
//! 2. The callback origin's `/auth/login` keeps both in a signed cookie when
//!    the host is one this deployment serves ([`remember_return_host`]).
//! 3. After the provider verifies the person, the callback origin redirects to
//!    the brand host's `/auth/handoff` with a signed, 60-second payload bound to
//!    that host and nonce ([`handoff_redirect`]).
//! 4. The brand host checks the signature, expiry, host, and its own nonce
//!    cookie, re-reads the role from the Person row, and sets its own session
//!    cookie ([`handoff`]).
//!
//! A captured handoff URL is useless in another browser: the nonce lives in an
//! `HttpOnly` cookie on the brand host, and the handoff clears it on use.

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use tower_cookies::{cookie::SameSite, Cookie, Cookies};
use uuid::Uuid;

use crate::oauth::{expired_cookie, post_login_landing, session_cookie, AuthState};
use crate::session::{now_unix_secs, random_token_32, SessionData};

/// The brand host's own cookie that binds a handoff to the browser that
/// started the sign-in.
pub const HANDOFF_NONCE_COOKIE: &str = "navigator_handoff_nonce";

/// The callback origin's signed record of which brand host to return to.
pub const RETURN_HOST_COOKIE: &str = "navigator_return_host";

/// How long a sign-in may take between leaving the brand host and returning.
const RETURN_HOST_TTL_SECS: i64 = 10 * 60;

/// How long the signed handoff payload stays redeemable.
const HANDOFF_TTL_SECS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReturnHost {
    host: String,
    nonce: String,
    exp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Handoff {
    host: String,
    nonce: String,
    person_id: Uuid,
    sub: String,
    email: Option<String>,
    provider: Option<String>,
    landing: String,
    exp: i64,
}

/// Step 1: the 303 a brand host answers `/auth/login` with.
///
/// `host` is the brand host's `Host:` header as received, port included, so a
/// local listener gets its own port back.
#[must_use]
pub fn brand_host_login_redirect(origin: &url::Url, host: &str, path_and_query: &str) -> Response {
    let nonce = random_token_32();
    let params = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("return_host", host)
        .append_pair("handoff_nonce", &nonce)
        .finish();
    let separator = if path_and_query.contains('?') {
        '&'
    } else {
        '?'
    };
    // Concatenate the origin with the existing absolute path. URL joining
    // would read a leading `//` as a new, untrusted authority.
    let target = format!(
        "{}{path_and_query}{separator}{params}",
        origin.origin().ascii_serialization()
    );
    let mut cookie = Cookie::new(HANDOFF_NONCE_COOKIE, nonce);
    cookie.set_http_only(true);
    cookie.set_secure(origin.scheme() == "https");
    // Lax: the handoff arrives as a top-level navigation from the callback
    // origin, which carries a Lax cookie.
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/auth/handoff");
    cookie.set_max_age(tower_cookies::cookie::time::Duration::seconds(
        RETURN_HOST_TTL_SECS,
    ));
    let mut response = Redirect::to(&target).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie.to_string()) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

/// Step 2: keep a valid `return_host` for the sign-in in flight. An unknown
/// host is ignored, so the sign-in completes on the callback origin.
pub(crate) fn remember_return_host(
    s: &AuthState,
    cookies: &Cookies,
    host: Option<&str>,
    nonce: Option<&str>,
) {
    let (Some(host), Some(nonce)) = (host, nonce) else {
        return;
    };
    if nonce.len() < 16 || !s.canonical_host.is_handoff_host(host) {
        return;
    }
    let record = ReturnHost {
        host: host.to_owned(),
        nonce: nonce.to_owned(),
        exp: now_unix_secs() + RETURN_HOST_TTL_SECS,
    };
    let value = s
        .sessions
        .encode_signed_bytes(&serde_json::to_vec(&record).expect("return host serializes"));
    let mut cookie = Cookie::new(RETURN_HOST_COOKIE, value);
    cookie.set_http_only(true);
    cookie.set_secure(s.secure_cookies);
    // Apple returns by cross-site POST; Lax would drop the cookie there.
    if s.secure_cookies {
        cookie.set_same_site(SameSite::None);
    } else {
        cookie.set_same_site(SameSite::Lax);
    }
    cookie.set_path("/auth");
    cookie.set_max_age(tower_cookies::cookie::time::Duration::seconds(
        RETURN_HOST_TTL_SECS,
    ));
    cookies.add(cookie);
}

/// Step 3: when this sign-in started on a brand host, the redirect that hands
/// it back. `None` keeps the sign-in on the callback origin.
pub(crate) fn handoff_redirect(
    s: &AuthState,
    cookies: &Cookies,
    session: &SessionData,
    landing: &str,
) -> Option<Response> {
    let raw = cookies.get(RETURN_HOST_COOKIE)?.value().to_owned();
    cookies.add(scoped_expired_cookie(RETURN_HOST_COOKIE, "/auth"));
    let record: ReturnHost = serde_json::from_slice(&s.sessions.decode_signed_bytes(&raw)?).ok()?;
    if record.exp <= now_unix_secs() || !s.canonical_host.is_handoff_host(&record.host) {
        return None;
    }
    let payload = Handoff {
        host: record.host.clone(),
        nonce: record.nonce,
        person_id: session.person_id?,
        sub: session.sub.clone(),
        email: session.email.clone(),
        provider: session.provider.clone(),
        landing: landing.to_owned(),
        exp: now_unix_secs() + HANDOFF_TTL_SECS,
    };
    let token = s
        .sessions
        .encode_signed_bytes(&serde_json::to_vec(&payload).expect("handoff serializes"));
    let scheme = if is_loopback(&record.host) {
        "http"
    } else {
        "https"
    };
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("t", &token)
        .finish();
    Some(Redirect::to(&format!("{scheme}://{}/auth/handoff?{query}", record.host)).into_response())
}

#[derive(Debug, Deserialize)]
pub(crate) struct HandoffQuery {
    #[serde(default)]
    t: String,
}

/// Step 4: `GET /auth/handoff` on the brand host.
pub(crate) async fn handoff(
    State(s): State<AuthState>,
    cookies: Cookies,
    headers: HeaderMap,
    Query(q): Query<HandoffQuery>,
) -> Response {
    let refuse = || {
        (
            StatusCode::BAD_REQUEST,
            [(header::REFERRER_POLICY, "no-referrer")],
            "This sign-in could not be completed. Start again from the sign-in page.",
        )
            .into_response()
    };
    let nonce = cookies
        .get(HANDOFF_NONCE_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    cookies.add(scoped_expired_cookie(HANDOFF_NONCE_COOKIE, "/auth/handoff"));
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let Some(payload) = s
        .sessions
        .decode_signed_bytes(&q.t)
        .and_then(|bytes| serde_json::from_slice::<Handoff>(&bytes).ok())
    else {
        return refuse();
    };
    let bound = payload.exp > now_unix_secs()
        && host.is_some_and(|host| host.eq_ignore_ascii_case(&payload.host))
        && nonce.is_some_and(|nonce| {
            crate::oauth::constant_time_eq(nonce.as_bytes(), payload.nonce.as_bytes())
        });
    if !bound {
        return refuse();
    }
    // The role and admission come from the Person row now, never from the
    // payload: a role change between the two hosts takes effect here.
    let person = match store::persons::find_by_id(&s.surreal, payload.person_id).await {
        Ok(Some(person)) => person,
        Ok(None) => return refuse(),
        Err(error) => {
            tracing::warn!(%error, "auth: handoff person lookup failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                webapp::error_pages::server_error(),
            )
                .into_response();
        }
    };
    if !store::persons::is_admitted(&s.surreal, person.id)
        .await
        .unwrap_or(false)
    {
        return (
            StatusCode::FORBIDDEN,
            webapp::error_pages::sign_in_not_provisioned(),
        )
            .into_response();
    }
    let mut session = SessionData::fresh(payload.sub, person.role);
    session.email = payload.email;
    session.person_id = Some(person.id);
    session.provider = payload.provider;
    cookies.add(session_cookie(
        s.sessions.encode(&session),
        s.secure_cookies,
    ));
    let landing = post_login_landing(person.role, &payload.landing);
    (
        [(header::REFERRER_POLICY, "no-referrer")],
        Redirect::to(&landing),
    )
        .into_response()
}

fn scoped_expired_cookie(name: &'static str, path: &'static str) -> Cookie<'static> {
    let mut cookie = expired_cookie(name);
    cookie.set_path(path);
    cookie
}

/// A loopback host, with or without a port: the local development listeners.
pub(crate) fn is_loopback(host: &str) -> bool {
    host.parse::<axum::http::uri::Authority>()
        .is_ok_and(|authority| matches!(authority.host(), "localhost" | "127.0.0.1" | "[::1]"))
}

#[cfg(test)]
mod tests {
    use super::is_loopback;

    #[test]
    fn loopback_hosts_are_recognised_with_and_without_ports() {
        for host in [
            "localhost",
            "localhost:4001",
            "127.0.0.1:20430",
            "[::1]:3000",
        ] {
            assert!(is_loopback(host), "{host}");
        }
        for host in ["iep.neonlaw.com", "localhost.example.com", "10.0.0.1"] {
            assert!(!is_loopback(host), "{host}");
        }
    }
}
