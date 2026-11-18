//! Signup admission before collecting an email address or password.
//!
//! Only a short-lived eligibility decision is kept in a signed cookie. The
//! submitted age is neither persisted nor logged. A refusal survives reload
//! and back-button attempts for the cookie's lifetime.

use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use chrono::Utc;
use serde::Deserialize;
use tower_cookies::Cookies;

use crate::oauth::AuthState;

const COOKIE: &str = "navigator_signup_age";
const TTL_MINUTES: i64 = 30;

fn decision(s: &AuthState, cookies: &Cookies) -> Option<bool> {
    let bytes = s
        .sessions
        .decode_signed_bytes(cookies.get(COOKIE)?.value())?;
    let value = std::str::from_utf8(&bytes).ok()?;
    let (decision, expires) = value.split_once(':')?;
    if expires.parse::<i64>().ok()? <= Utc::now().timestamp() {
        return None;
    }
    match decision {
        "eligible" => Some(true),
        "blocked" => Some(false),
        _ => None,
    }
}

pub(crate) fn eligible(s: &AuthState, cookies: &Cookies) -> bool {
    decision(s, cookies) == Some(true)
}

pub(crate) fn response(s: &AuthState, cookies: &Cookies) -> Response {
    let blocked = decision(s, cookies) == Some(false);
    let csrf = crate::password_reset::mint_csrf(s, cookies);
    (
        if blocked {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::OK
        },
        webapp::auth_pages::signup_age(&csrf, blocked),
    )
        .into_response()
}

#[derive(Deserialize)]
pub(crate) struct AgeForm {
    age: u8,
    #[serde(default)]
    csrf_token: String,
}

pub(crate) async fn submit(
    State(s): State<AuthState>,
    cookies: Cookies,
    Form(form): Form<AgeForm>,
) -> Response {
    if !crate::password_reset::verify_csrf(&s, &cookies, &form.csrf_token) {
        return (StatusCode::BAD_REQUEST, "invalid or missing CSRF token").into_response();
    }
    if decision(&s, &cookies) == Some(false) {
        return response(&s, &cookies);
    }
    if form.age > 120 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let decision = if form.age >= 13 {
        "eligible"
    } else {
        "blocked"
    };
    let expires = Utc::now().timestamp() + TTL_MINUTES * 60;
    let mut cookie = crate::password_reset::csrf_cookie_with_ttl(
        COOKIE,
        s.sessions
            .encode_signed_bytes(format!("{decision}:{expires}").as_bytes()),
        s.secure_cookies,
        TTL_MINUTES,
    );
    // Apple returns by cross-site POST; its callback needs the same decision.
    if s.secure_cookies {
        cookie.set_same_site(tower_cookies::cookie::SameSite::None);
    }
    cookies.add(cookie);
    Redirect::to("/auth/signup").into_response()
}
