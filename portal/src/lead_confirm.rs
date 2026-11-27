//! Confirming a lead's address, which signs the lead in as a Client.
//!
//! Every accepted lead submission is answered with an email carrying a signed,
//! 24-hour link. The link names the lead and the address it was sent to; it is
//! signed with the session key, so it is unforgeable without that key, and it
//! is stateless, so a resubmission simply sends a fresh one.
//!
//! Following the link proves the visitor holds the mailbox. It makes the lead
//! a Client (`store::leads::convert`, which links an existing Person holding
//! the same address instead of creating one), marks the address confirmed,
//! and signs that Client in. A lawyer-tier Person is never signed in by an
//! email link: firm accounts sign in only through the configured providers,
//! so their link confirms the lead and sends them to the sign-in page.
//!
//! A session opened here records the `email` door, so it is never mistaken
//! for a Google sign-in: only Google earns the complimentary consultation
//! (`crate::purchases`).

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use store::persons::Role;
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::{AppState, SessionData, SessionStore};

/// The confirmation route.
pub const CONFIRM_PATH: &str = "/leads/confirm";
/// The provider slug a confirmation-link session records. It is not a
/// [`crate::oauth::ProviderId`]: no IdP holds an SSO session for it.
pub const EMAIL_LINK_PROVIDER: &str = "email";
/// The template slug recorded on the sent email.
pub const TEMPLATE_SLUG: &str = "lead_confirm";
const PURPOSE: &str = "lead_confirm";
const TTL_SECS: i64 = 24 * 60 * 60;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Claim {
    purpose: String,
    lead: Uuid,
    email: String,
    exp: i64,
}

/// Mint the signed token for `lead_id`, valid for 24 hours from `now`.
#[must_use]
pub fn token(sessions: &SessionStore, lead_id: Uuid, email: &str, now: i64) -> String {
    let claim = Claim {
        purpose: PURPOSE.to_string(),
        lead: lead_id,
        email: email.trim().to_lowercase(),
        exp: now + TTL_SECS,
    };
    sessions.encode_signed_bytes(&serde_json::to_vec(&claim).expect("claim serializes"))
}

/// Read a token back: `None` when it is forged, malformed, for another
/// purpose, or expired at `now`.
fn read(sessions: &SessionStore, raw: &str, now: i64) -> Option<Claim> {
    let claim: Claim = serde_json::from_slice(&sessions.decode_signed_bytes(raw)?).ok()?;
    (claim.purpose == PURPOSE && claim.exp > now).then_some(claim)
}

/// Email the confirmation link for an accepted lead. Best effort: a failed
/// send is logged by lead id, never by address, and the visitor sees the same
/// neutral page either way.
pub async fn send(state: &AppState, lead_id: Uuid, email: &str) {
    let base_url = workflows::email::base_url_from_env();
    let confirm_url = format!(
        "{}{CONFIRM_PATH}?token={}",
        base_url.trim_end_matches('/'),
        token(&state.sessions, lead_id, email, Utc::now().timestamp()),
    );
    let lead_confirm = &workflows::email::lead_confirm::render_lead_confirm_body;
    let mut message = crate::email::OutboundEmail::new(
        email.to_string(),
        workflows::email::lead_confirm::lead_confirm_subject(),
        lead_confirm(email, &confirm_url),
    );
    message.html_body = Some(workflows::email::lead_confirm::render_lead_confirm_html(
        email,
        &confirm_url,
        &base_url,
    ));
    message.template_slug = Some(TEMPLATE_SLUG.to_string());
    message.from = views::brand::request_sender_email();
    if let Err(error) = state.email.send(message).await {
        tracing::warn!(error = %error, %lead_id, "lead confirmation email failed");
    }
}

/// Answer an accepted lead with its confirmation link, off the request path
/// so the visitor's redirect never waits on mail. The spawned send keeps the
/// request's Brand and mail sender, which a new task would otherwise lose.
pub fn spawn_send(state: AppState, lead_id: Uuid, email: String) {
    tokio::spawn(views::brand::carry_request_scope(async move {
        send(&state, lead_id, &email).await;
    }));
}

/// The public confirmation route. It sits outside the session boundary: the
/// signed token is the credential.
pub fn routes() -> Router<AppState> {
    Router::new().route(CONFIRM_PATH, get(confirm))
}

#[derive(Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: String,
}

async fn confirm(
    State(state): State<AppState>,
    cookies: Cookies,
    Query(query): Query<TokenQuery>,
) -> Response {
    let Some(claim) = read(&state.sessions, &query.token, Utc::now().timestamp()) else {
        return webapp::auth_pages::invalid_link().into_response();
    };
    let lead = match store::leads::find(&state.application_store, claim.lead).await {
        Ok(Some(lead)) if lead.email_lower == claim.email => lead,
        Ok(_) => return webapp::auth_pages::invalid_link().into_response(),
        Err(error) => return server_error(&error, claim.lead),
    };
    if lead.status == store::leads::LeadStatus::Unsubscribed.as_str() {
        audit(lead.id, None, "rejected_unsubscribed");
        return webapp::auth_pages::invalid_link().into_response();
    }
    let person_id = match lead.person_id {
        Some(person_id) => person_id,
        None => match store::leads::convert(&state.application_store, lead.id).await {
            Ok(converted) => match converted.person_id {
                Some(person_id) => person_id,
                None => return server_error(&"converted lead has no Person", lead.id),
            },
            Err(error) => return server_error(&error, lead.id),
        },
    };
    let person = match store::persons::set_email_confirmed(
        &state.application_store,
        person_id,
        true,
    )
    .await
    {
        Ok(Some(person)) => person,
        Ok(None) => return server_error(&"lead Person is missing", lead.id),
        Err(error) => return server_error(&error, lead.id),
    };
    let admitted = store::persons::is_admitted(&state.application_store, person.id)
        .await
        .unwrap_or(false);
    if person.role != Role::Client || !admitted {
        audit(lead.id, Some(person.id), "confirmed_without_session");
        return Redirect::to("/auth/login?notice=email_confirmed").into_response();
    }
    let mut session =
        SessionData::fresh(format!("{EMAIL_LINK_PROVIDER}:{}", person.id), Role::Client);
    session.email = Some(person.email.clone());
    session.person_id = Some(person.id);
    session.provider = Some(EMAIL_LINK_PROVIDER.to_string());
    cookies.add(crate::oauth::session_cookie(
        state.sessions.encode(&session),
        crate::secure_cookies(&state),
    ));
    audit(lead.id, Some(person.id), "signed_in");
    Redirect::to(crate::purchases::PURCHASES_PATH).into_response()
}

fn server_error(error: &dyn std::fmt::Display, lead_id: Uuid) -> Response {
    tracing::error!(error = %error, %lead_id, "lead confirmation failed");
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        webapp::error_pages::server_error(),
    )
        .into_response()
}

fn audit(lead_id: Uuid, person_id: Option<Uuid>, outcome: &str) {
    tracing::info!(
        target: "audit",
        %lead_id,
        person_id = person_id.map(|id| id.to_string()).unwrap_or_default(),
        outcome,
        "lead confirmation"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_reads_back_until_it_expires_and_never_once_tampered() {
        let sessions = SessionStore::new(b"a-synthetic-session-key-of-adequate-length".to_vec());
        let lead = Uuid::new_v4();
        let raw = token(&sessions, lead, "Visitor@Example.com", 1_000);
        let claim = read(&sessions, &raw, 1_001).expect("fresh token reads");
        assert_eq!(claim.lead, lead);
        assert_eq!(claim.email, "visitor@example.com");
        assert!(read(&sessions, &raw, 1_000 + TTL_SECS).is_none(), "expired");
        assert!(
            read(&sessions, &format!("x{raw}"), 1_001).is_none(),
            "tampered"
        );
        let other = SessionStore::new(b"another-synthetic-session-key-entirely".to_vec());
        assert!(read(&other, &raw, 1_001).is_none(), "another key");
        let session_cookie = sessions.encode(&SessionData::fresh("sub", Role::Client));
        assert!(
            read(&sessions, &session_cookie, 1_001).is_none(),
            "a session is no token"
        );
    }
}
