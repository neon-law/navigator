//! Firm inbox HTTP doors.
//!
//! Firm members assign a conversation from `/app/inbox`. Participating
//! Lawyers and Clerks read it on the Project and leave internal notes.
//! Only a participating Lawyer or Clerk posts a substantive external reply. Automatic
//! acknowledgments confirm receipt after the Firm inbox saves the message. SMS
//! webhooks are anonymous and authenticated by HMAC when the deployment sets a secret.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Extension;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::admin::AdminState;
use crate::inbound_email::InboundEmail;
use crate::session::SessionData;

const SMS_SIGNATURE: &str = "x-navigator-signature";

/// Start a message-keyed Restate workflow after the inbound Message has been
/// persisted. Repeated provider deliveries attach to the same Slack alert.
pub async fn start_inbound_message_alert(message_id: Uuid) -> Result<(), String> {
    let Some(ingress) = std::env::var("RESTATE_BROKER_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let request = workflows::InboundMessageAlertRequest { message_id };
    workflows::start_workflow(
        &ingress,
        std::env::var("RESTATE_AUTH_TOKEN").ok().as_deref(),
        "InboundMessageAlert",
        &message_id.to_string(),
        "run",
        &request,
        true,
    )
    .await
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// Record support intake in the shared email/SMS transcript. Public Neon Law
/// mailboxes resolve their owning Firm by brand when no explicit inbox Firm is
/// configured. Token-addressed Lawyer/Clerk replies keep their threading path.
///
/// # Errors
///
/// Propagates database failures or a missing Firm for a public support mailbox.
pub async fn ingest_support_email(
    db: &store::surreal::SurrealDb,
    email: &InboundEmail,
) -> Result<Option<store::inbox::IngestedInbound>, String> {
    ingest_support_email_for_firm(db, email, inbox_firm_id()).await
}

async fn ingest_support_email_for_firm(
    db: &store::surreal::SurrealDb,
    email: &InboundEmail,
    configured_firm: Option<Uuid>,
) -> Result<Option<store::inbox::IngestedInbound>, String> {
    let (receiving_mailbox, brand) = receiving_email_identity(email)?;
    let public_mailbox = brand.is_some();
    let owning_firm = if let Some(brand) = brand {
        store::firms::firm_id_for_brand_key(db, brand.as_str())
            .await
            .map_err(|error| error.to_string())?
    } else {
        None
    };
    let firm_id = match (configured_firm, owning_firm) {
        (Some(configured), Some(owner)) if configured != owner => {
            return Err("receiving Brand belongs to another Firm".into())
        }
        (_, Some(owner)) => owner,
        (Some(configured), None) if !public_mailbox => configured,
        (_, None) if public_mailbox => return Err("receiving Brand has no owning Firm".into()),
        _ => return Ok(None),
    };
    let from = extract_addr(&email.from);
    if let Some(person) = store::persons::find_by_email_ci(db, &from)
        .await
        .map_err(|error| error.to_string())?
    {
        if !public_mailbox && (person.role.is_lawyer_tier() || person.role.is_clerk()) {
            return Ok(None);
        }
    }
    let provider_message_id = provider_message_id(email);
    let email_conversation_id = linked_email_conversation(db, email, &from).await?;
    let account = std::env::var("NAVIGATOR_EMAIL_PROVIDER_ACCOUNT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "support".into());
    store::inbox::ingest_inbound(
        db,
        &store::inbox::IngestInbound {
            firm_id,
            brand_key: brand.map(views::brand::BrandKey::as_str),
            interaction_type: Some("message"),
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: store::inbox::channel::EMAIL,
            provider_account: &account,
            provider_message_id: &provider_message_id,
            provider_thread_ref: None,
            external_address: &from,
            external_name: None,
            subject: Some(email.subject.as_str()),
            body: email.text.as_str(),
            source_page: Some(&receiving_mailbox),
            source_channel: Some(store::inbox::channel::EMAIL),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id,
            attachment_asset_id: None,
            occurred_at: &chrono::Utc::now().to_rfc3339(),
        },
    )
    .await
    .map(Some)
    .map_err(|error| error.to_string())
}

fn receiving_email_identity(
    email: &InboundEmail,
) -> Result<(String, Option<views::brand::BrandKey>), String> {
    let recipients: Vec<String> = email
        .envelope
        .as_ref()
        .map(|envelope| {
            envelope
                .to
                .iter()
                .map(|address| extract_addr(address))
                .collect()
        })
        .filter(|to: &Vec<String>| !to.is_empty())
        .unwrap_or_else(|| vec![extract_addr(&email.to)]);
    let mut owned = recipients
        .iter()
        .filter_map(|address| receiving_brand(address).map(|brand| (address, brand)));
    let first = owned.next();
    if let Some((_, brand)) = first {
        if owned.any(|(_, other)| other != brand) {
            return Err("email has receiving accounts from multiple Brands".into());
        }
    }
    if let Some((address, brand)) = first {
        return Ok((address.clone(), Some(brand)));
    }
    // A forwarding rule can rewrite the envelope recipient (support@ becomes
    // the SendGrid parse address) while the To header keeps the public
    // mailbox the sender wrote to.
    let header_owned = email
        .to
        .split(',')
        .map(extract_addr)
        .find_map(|address| receiving_brand(&address).map(|brand| (address, Some(brand))));
    Ok(header_owned.unwrap_or_else(|| (recipients[0].clone(), None)))
}

fn receiving_brand(mailbox: &str) -> Option<views::brand::BrandKey> {
    use views::brand::{BrandKey, DEFAULT_BRANDING};
    if ["support@neonlaw.com", "staging@neonlaw.com"]
        .iter()
        .any(|address| mailbox.eq_ignore_ascii_case(address))
    {
        return Some(BrandKey::Neon);
    }
    BrandKey::ALL.iter().copied().find(|key| {
        mailbox.eq_ignore_ascii_case(key.resolve_branding(&DEFAULT_BRANDING).firm_email)
    })
}

fn provider_message_id(email: &InboundEmail) -> String {
    if let Some(id) = email.message_id.as_ref().filter(|id| !id.is_empty()) {
        return id.clone();
    }
    let mut digest = Sha256::new();
    let mut append = |bytes: &[u8]| {
        // Length framing preserves field boundaries without cloning MIME or attachments.
        digest.update(bytes.len().to_string().as_bytes());
        digest.update(b":");
        digest.update(bytes);
    };
    for bytes in [
        email.from.as_bytes(),
        email.to.as_bytes(),
        email.subject.as_bytes(),
        email.text.as_bytes(),
        email.headers.as_bytes(),
        email.raw.as_slice(),
    ] {
        append(bytes);
    }
    for attachment in &email.attachments {
        append(attachment.filename.as_bytes());
        append(attachment.content_type.as_bytes());
        append(&attachment.bytes);
    }
    workflows::scoped_email_digest("inbox", &email.to, digest.finalize().as_slice())
}

fn inbox_firm_id() -> Option<Uuid> {
    std::env::var("NAVIGATOR_INBOX_FIRM_ID")
        .ok()
        .and_then(|value| Uuid::parse_str(value.trim()).ok())
}

fn sms_provider_account() -> String {
    std::env::var("NAVIGATOR_SMS_PROVIDER_ACCOUNT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "sms".into())
}

async fn linked_email_conversation(
    db: &store::surreal::SurrealDb,
    email: &InboundEmail,
    from: &str,
) -> Result<Option<Uuid>, String> {
    if let Some(cfg) = crate::email_threads::ThreadConfig::from_env() {
        if let Some(token) = crate::email_threads::token_from_to(&email.to, &cfg.parse_host) {
            return store::email_conversations::by_token(db, &token)
                .await
                .map(|row| row.map(|conversation| conversation.id))
                .map_err(|error| error.to_string());
        }
    }
    store::email_conversations::by_external_email(db, from)
        .await
        .map(|row| row.map(|conversation| conversation.id))
        .map_err(|error| error.to_string())
}

pub(crate) fn extract_addr(header: &str) -> String {
    let start = header.rfind('<').map_or(0, |index| index + 1);
    let end = header.rfind('>').unwrap_or(header.len());
    header
        .get(start..end)
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[derive(Debug, Deserialize)]
pub struct AssignForm {
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
    project_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct NoteForm {
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
    body: String,
}

#[derive(Debug, Deserialize)]
pub struct ContactForm {
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
    email: String,
}

#[derive(Debug, Deserialize)]
pub struct NewTextForm {
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
    to: String,
    body: String,
    /// The sender's attestation that the recipient agreed to receive texts;
    /// an unchecked box is absent from the form post.
    #[serde(default)]
    consent: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReplyForm {
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
    body: String,
    #[serde(default)]
    visibility: Option<String>,
}

/// `POST /app/inbox/{conversation_id}/assign`
pub async fn assign_post(
    State(db): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(conversation_id): Path<Uuid>,
    axum::Form(form): axum::Form<AssignForm>,
) -> Response {
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let back = format!("/app/inbox/{conversation_id}");
    match store::inbox::attribute_to_project(&db, actor_id, conversation_id, form.project_id).await
    {
        Ok(assignment) => {
            let project = store::projects::find_by_id(&db, assignment.to_project_id)
                .await
                .ok()
                .flatten();
            match project {
                Some(project) => Redirect::to(&format!(
                    "/app/projects/{}/inbox/{conversation_id}",
                    project.code
                ))
                .into_response(),
                None => Redirect::to(&back).into_response(),
            }
        }
        Err(error) => redirect_error(&back, &error.to_string()),
    }
}

/// `POST /app/projects/{project_code}/inbox/{conversation_id}/notes`
pub async fn note_post(
    State(db): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path((project_code, conversation_id)): Path<(String, Uuid)>,
    axum::Form(form): axum::Form<NoteForm>,
) -> Response {
    let back = format!("/app/projects/{project_code}/inbox/{conversation_id}");
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match store::inbox::post_note(&db, actor_id, conversation_id, &form.body).await {
        Ok(_) => Redirect::to(&back).into_response(),
        Err(error) => redirect_error(&back, &error.to_string()),
    }
}

/// `POST /app/inbox/{conversation_id}/contact`
///
/// Records the text sender's number as the direct line of the Person with the
/// given email, so this and later texts from it name that Person.
pub async fn link_contact_post(
    State(state): State<AdminState>,
    session: Option<Extension<SessionData>>,
    Path(conversation_id): Path<Uuid>,
    axum::Form(form): axum::Form<ContactForm>,
) -> Response {
    let back = format!("/app/inbox/{conversation_id}");
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match store::inbox::link_contact_phone(&state.surreal, actor_id, conversation_id, &form.email)
        .await
    {
        Ok(_) => Redirect::to(&back).into_response(),
        Err(error) => redirect_error(&back, &error.to_string()),
    }
}

/// `POST /app/inbox/texts`
///
/// Opens a text conversation with a number that has not texted first, from the
/// deployment's receiving number, and sends the first message. The sender must
/// attest that the recipient agreed to receive texts.
pub async fn new_text_post(
    State(state): State<AdminState>,
    session: Option<Extension<SessionData>>,
    axum::Form(form): axum::Form<NewTextForm>,
) -> Response {
    let back = "/app/inbox";
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(Some(config)) = crate::twilio_sms::SmsIntakeConfig::from_env() else {
        return redirect_error(back, "Texting is not configured on this deployment");
    };
    let (firm_id, brand) = match crate::twilio_sms::receiving_firm(&state.surreal, &config).await {
        Ok(found) => found,
        Err(error) => return redirect_error(back, &error),
    };
    let queued = store::inbox::open_sms_conversation(
        &state.surreal,
        actor_id,
        firm_id,
        Some(brand.as_str()),
        &config.number,
        &form.to,
        &form.body,
        form.consent.as_deref() == Some("yes"),
    )
    .await;
    let (conversation_id, _) = match queued {
        Ok(opened) => opened,
        Err(store::inbox::InboxError::Denied) => {
            return redirect_error(
                back,
                "Enter an E.164 number, confirm the recipient agreed to texts, and use an Owner or Admin sign-in",
            )
        }
        Err(error) => return redirect_error(back, &error.to_string()),
    };
    let thread = format!("/app/inbox/{conversation_id}");
    if let Err(error) = dispatch_now(&state).await {
        return redirect_error(&thread, &error.to_string());
    }
    Redirect::to(&thread).into_response()
}

/// Dispatch every queued Message through the deployment's channel transports.
async fn dispatch_now(state: &AdminState) -> Result<(), store::inbox::InboxError> {
    let transport = crate::inbox_transport::ChannelTransport::new(
        state.email.clone(),
        std::sync::Arc::new(crate::inbox_sms::TwilioSmsTransport::new(
            crate::twilio_sms::SmsIntakeConfig::from_env()
                .ok()
                .flatten(),
        )),
    )
    .with_social(std::sync::Arc::new(
        crate::inbox_transport::SocialReplyTransport::new(
            state.surreal.clone(),
            state.runtime_kms.clone(),
        ),
    ));
    store::inbox::dispatch_queued(&state.surreal, &transport)
        .await
        .map(|_| ())
}

/// `POST /app/projects/{project_code}/inbox/{conversation_id}/replies`
///
/// Queues the reply, then dispatches it. A Lawyer or Clerk whose participation was
/// removed between the two calls is suppressed inside dispatch.
pub async fn reply_post(
    State(state): State<AdminState>,
    session: Option<Extension<SessionData>>,
    Path((project_code, conversation_id)): Path<(String, Uuid)>,
    axum::Form(form): axum::Form<ReplyForm>,
) -> Response {
    let back = format!("/app/projects/{project_code}/inbox/{conversation_id}");
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let queued = if let Some(visibility) = form.visibility.as_deref() {
        store::inbox::queue_social_reply(
            &state.surreal,
            actor_id,
            conversation_id,
            &form.body,
            visibility,
        )
        .await
    } else {
        store::inbox::queue_reply(
            &state.surreal,
            actor_id,
            conversation_id,
            &form.body,
            store::inbox::purpose::MATTER,
        )
        .await
    };
    if let Err(error) = queued {
        return redirect_error(&back, &error.to_string());
    }
    if let Err(error) = dispatch_now(&state).await {
        return redirect_error(&back, &error.to_string());
    }
    Redirect::to(&back).into_response()
}

/// `POST /app/inbox/{conversation_id}/replies`
///
/// Firm inbox replies are used for social engagement, whose Owner/Admin
/// authorization is separate from Project matter email/SMS dispatch.
pub async fn intake_reply_post(
    State(state): State<AdminState>,
    session: Option<Extension<SessionData>>,
    Path(conversation_id): Path<Uuid>,
    axum::Form(form): axum::Form<ReplyForm>,
) -> Response {
    let back = format!("/app/inbox/{conversation_id}");
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let is_sms = matches!(
        store::inbox::conversation_by_id(&state.surreal, conversation_id).await,
        Ok(Some(conversation)) if conversation.channel == store::inbox::channel::SMS
    );
    if is_sms {
        if let Err(error) =
            store::inbox::queue_sms_reply(&state.surreal, actor_id, conversation_id, &form.body)
                .await
        {
            return redirect_error(&back, &error.to_string());
        }
        if let Err(error) = dispatch_now(&state).await {
            return redirect_error(&back, &error.to_string());
        }
        return Redirect::to(&back).into_response();
    }
    let Some(visibility) = form.visibility.as_deref() else {
        return redirect_error(&back, "A social reply audience is required");
    };
    if let Err(error) = store::inbox::queue_social_reply(
        &state.surreal,
        actor_id,
        conversation_id,
        &form.body,
        visibility,
    )
    .await
    {
        return redirect_error(&back, &error.to_string());
    }
    if let Err(error) = dispatch_now(&state).await {
        return redirect_error(&back, &error.to_string());
    }
    Redirect::to(&back).into_response()
}

fn actor_id(session: Option<Extension<SessionData>>) -> Option<Uuid> {
    session.and_then(|extension| extension.person_id)
}

fn redirect_error(back: &str, message: &str) -> Response {
    Redirect::to(&format!(
        "{back}?error={}",
        crate::admin::encode_query_value(message)
    ))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct SmsInbound {
    pub provider_message_id: String,
    pub from: String,
    pub body: String,
    #[serde(default)]
    pub provider_account: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SmsStatus {
    pub provider_ref: String,
    pub state: String,
    #[serde(default)]
    pub provider_account: Option<String>,
}

/// `POST /webhook/sms/inbound/{secret}`
pub async fn sms_inbound(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    if !sms_authorized(&headers, &body) {
        return StatusCode::UNAUTHORIZED;
    }
    let Some(firm_id) = inbox_firm_id() else {
        return StatusCode::ACCEPTED;
    };
    let Ok(payload) = serde_json::from_slice::<SmsInbound>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let account = payload
        .provider_account
        .filter(|value| !value.is_empty())
        .unwrap_or_else(sms_provider_account);
    match store::inbox::ingest_inbound(
        &state.surreal,
        &store::inbox::IngestInbound {
            firm_id,
            brand_key: Some("neon"),
            interaction_type: Some("message"),
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: store::inbox::channel::SMS,
            provider_account: &account,
            provider_message_id: &payload.provider_message_id,
            provider_thread_ref: None,
            external_address: &payload.from,
            external_name: None,
            subject: None,
            body: &payload.body,
            source_page: None,
            source_channel: Some(store::inbox::channel::SMS),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachment_asset_id: None,
            occurred_at: &chrono::Utc::now().to_rfc3339(),
        },
    )
    .await
    {
        Ok(receipt) => {
            if start_inbound_message_alert(receipt.message_id)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE;
            }
            StatusCode::ACCEPTED
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// `POST /webhook/sms/status/{secret}`
pub async fn sms_status(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    if !sms_authorized(&headers, &body) {
        return StatusCode::UNAUTHORIZED;
    }
    let Some(firm_id) = inbox_firm_id() else {
        return StatusCode::ACCEPTED;
    };
    let Ok(payload) = serde_json::from_slice::<SmsStatus>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let account = payload
        .provider_account
        .filter(|value| !value.is_empty())
        .unwrap_or_else(sms_provider_account);
    match store::inbox::apply_delivery(
        &state.surreal,
        firm_id,
        store::inbox::channel::SMS,
        &account,
        &payload.provider_ref,
        &payload.state,
    )
    .await
    {
        Ok(_) => StatusCode::ACCEPTED,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn sms_authorized(headers: &HeaderMap, body: &[u8]) -> bool {
    let Ok(secret) = std::env::var("NAVIGATOR_SMS_WEBHOOK_SECRET") else {
        return true;
    };
    if secret.trim().is_empty() {
        return true;
    }
    let Some(provided) = headers
        .get(SMS_SIGNATURE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    crate::webhook_auth::verify_hmac_sha256_b64(secret.as_bytes(), body, provided)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, State};
    use axum::http::StatusCode;

    #[tokio::test]
    async fn unconfigured_support_mail_is_ignored() {
        let db = store::test_support::mem_surreal().await;
        ingest_support_email(&db, &InboundEmail::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn unconfigured_sms_webhooks_are_accepted() {
        let state = crate::test_support::app_state(store::test_support::mem_surreal().await).await;
        let inbound = sms_inbound(
            State(state.clone()),
            HeaderMap::new(),
            axum::body::Bytes::new(),
        )
        .await;
        assert_eq!(inbound, StatusCode::ACCEPTED);
        let status = sms_status(
            State(state),
            HeaderMap::new(),
            axum::body::Bytes::from_static(b"{}"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn configured_sms_inbound_ingests_text_and_rejects_invalid_json() {
        let db = store::test_support::mem_surreal().await;
        let firm = store::test_support::seed_firm_with_brand(&db, "neon").await;
        std::env::set_var("NAVIGATOR_INBOX_FIRM_ID", firm.to_string());
        let state = crate::test_support::app_state(db).await;
        let inbound = sms_inbound(
            State(state.clone()),
            HeaderMap::new(),
            axum::body::Bytes::from_static(
                br#"{"provider_message_id":"sms-1","from":"+15555550100","body":"Hello"}"#,
            ),
        )
        .await;
        assert_eq!(inbound, StatusCode::ACCEPTED);
        let invalid = sms_inbound(
            State(state.clone()),
            HeaderMap::new(),
            axum::body::Bytes::from_static(b"not-json"),
        )
        .await;
        assert_eq!(invalid, StatusCode::BAD_REQUEST);
        let status = sms_status(
            State(state),
            HeaderMap::new(),
            axum::body::Bytes::from_static(br#"{"provider_ref":"sms-1","state":"delivered"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        std::env::remove_var("NAVIGATOR_INBOX_FIRM_ID");
    }

    #[tokio::test]
    async fn anonymous_inbox_posts_are_not_found() {
        let db = store::test_support::mem_surreal().await;
        let assign = assign_post(
            State(db.clone()),
            None,
            Path(Uuid::nil()),
            axum::Form(AssignForm {
                _csrf: None,
                project_id: Uuid::nil(),
            }),
        )
        .await;
        assert_eq!(assign.status(), StatusCode::NOT_FOUND);
        let note = note_post(
            State(db),
            None,
            Path(("matter".into(), Uuid::nil())),
            axum::Form(NoteForm {
                _csrf: None,
                body: "Private".into(),
            }),
        )
        .await;
        assert_eq!(note.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn start_inbound_message_alert_is_ok_without_a_broker() {
        std::env::remove_var("RESTATE_BROKER_URL");
        start_inbound_message_alert(Uuid::now_v7()).await.unwrap();
    }

    #[tokio::test]
    async fn receiving_accounts_keep_two_brands_and_firms_separate() {
        let db = store::test_support::mem_surreal().await;
        let neon = store::test_support::seed_firm_with_brand(&db, "neon").await;
        let shook = store::test_support::seed_firm_with_brand(&db, "lawyer-shook").await;
        for (mailbox, firm, brand) in [
            ("support@neonlaw.com", neon, "neon"),
            ("contact@lawyershook.com", shook, "lawyer-shook"),
        ] {
            let email = InboundEmail {
                from: "sender@example.com".into(),
                to: mailbox.into(),
                text: "Synthetic inquiry".into(),
                message_id: Some("synthetic-provider-id".into()),
                ..InboundEmail::default()
            };
            let saved = ingest_support_email_for_firm(&db, &email, None)
                .await
                .unwrap()
                .unwrap();
            let thread = store::inbox::conversation_by_id(&db, saved.conversation_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(thread.firm_id, firm);
            assert_eq!(thread.brand_key.as_deref(), Some(brand));
            assert!(ingest_support_email_for_firm(
                &db,
                &email,
                Some(if firm == neon { shook } else { neon })
            )
            .await
            .is_err());
        }
    }

    #[test]
    fn email_brand_comes_from_the_receiving_account() {
        let email = InboundEmail {
            to: "Someone <support@neonlaw.com>".into(),
            ..InboundEmail::default()
        };
        assert_eq!(
            receiving_email_identity(&email).unwrap().1,
            Some(views::brand::BrandKey::Neon)
        );
        for brand in views::brand::BrandKey::ALL {
            let address = brand
                .resolve_branding(&views::brand::DEFAULT_BRANDING)
                .firm_email;
            if address != views::brand::DEFAULT_BRANDING.firm_email {
                assert_eq!(receiving_brand(address), Some(*brand));
            }
        }
    }

    #[test]
    fn rewritten_envelope_keeps_the_public_brand_from_the_to_header() {
        // Workspace rewrites support@ to the SendGrid parse address; the To
        // header still names the public mailbox the sender wrote to.
        let email = InboundEmail {
            to: "Neon Law Support <support@neonlaw.com>".into(),
            envelope: Some(crate::inbound_email::SmtpEnvelope {
                to: vec!["intake@parse.neonlaw.com".into()],
                from: None,
            }),
            ..InboundEmail::default()
        };
        let (mailbox, brand) = receiving_email_identity(&email).unwrap();
        assert_eq!(mailbox, "support@neonlaw.com");
        assert_eq!(brand, Some(views::brand::BrandKey::Neon));
    }

    #[test]
    fn email_recipients_from_different_brands_fail_closed() {
        let email = InboundEmail {
            to: "support@neonlaw.com".into(),
            envelope: Some(crate::inbound_email::SmtpEnvelope {
                to: vec![
                    "support@neonlaw.com".into(),
                    "contact@deleteyourdata.com".into(),
                ],
                from: None,
            }),
            ..InboundEmail::default()
        };
        assert!(receiving_email_identity(&email).is_err());
    }

    #[test]
    fn provider_message_id_prefers_the_header_and_hashes_otherwise() {
        let with_id = InboundEmail {
            message_id: Some("mid-1".into()),
            from: "a@example.com".into(),
            ..InboundEmail::default()
        };
        assert_eq!(provider_message_id(&with_id), "mid-1");
        let hashed = InboundEmail {
            from: "Reader <reader@example.com>".into(),
            to: "inbox@example.com".into(),
            subject: "Question".into(),
            text: "Body".into(),
            ..InboundEmail::default()
        };
        let first = provider_message_id(&hashed);
        let second = provider_message_id(&hashed);
        assert_eq!(first, second);
        assert_ne!(first, "mid-1");
        assert_eq!(
            extract_addr("Reader <reader@example.com>"),
            "reader@example.com"
        );
        assert_eq!(sms_provider_account(), "sms");
    }

    #[tokio::test]
    async fn linking_a_number_requires_a_session_and_reports_a_missing_conversation() {
        let app = crate::test_support::app_state(store::test_support::mem_surreal().await).await;
        let state = crate::admin::AdminState {
            surreal: app.surreal,
            workflow_runtime: app.workflow_runtime,
            signature_provider: app.signature_provider,
            retainer_intake_questionnaire: workflows::retainer_intake_questionnaire(),
            questionnaire_runtime: app.questionnaire_runtime,
            storage: app.storage,
            assets_storage: app.assets_storage,
            forms_registry: app.forms_registry,
            email: app.email,
            billing_provider: app.billing_provider,
            contract_reviewer: app.contract_reviewer,
            bootstrap_owner_email: app.bootstrap_owner_email,
            on_call_lawyer_email: app.on_call_lawyer_email,
            bootstrap_company: crate::admin::bootstrap_company_from_env(),
            sessions: app.sessions,
            secure_cookies: false,
            attachment_scanner: app.attachment_scanner,
            runtime_kms: app.runtime_kms,
        };
        let form = || {
            axum::Form(ContactForm {
                _csrf: None,
                email: "pat@example.com".into(),
            })
        };
        let anonymous =
            link_contact_post(State(state.clone()), None, Path(Uuid::nil()), form()).await;
        assert_eq!(anonymous.status(), StatusCode::NOT_FOUND);

        let mut session =
            crate::session::SessionData::fresh("owner@example.com", store::persons::Role::Owner);
        session.person_id = Some(Uuid::now_v7());
        let missing = link_contact_post(
            State(state),
            Some(axum::Extension(session)),
            Path(Uuid::nil()),
            form(),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn new_text_requires_a_session_and_a_configured_number() {
        let app = crate::test_support::app_state(store::test_support::mem_surreal().await).await;
        let state = crate::admin::AdminState {
            surreal: app.surreal,
            workflow_runtime: app.workflow_runtime,
            signature_provider: app.signature_provider,
            retainer_intake_questionnaire: workflows::retainer_intake_questionnaire(),
            questionnaire_runtime: app.questionnaire_runtime,
            storage: app.storage,
            assets_storage: app.assets_storage,
            forms_registry: app.forms_registry,
            email: app.email,
            billing_provider: app.billing_provider,
            contract_reviewer: app.contract_reviewer,
            bootstrap_owner_email: app.bootstrap_owner_email,
            on_call_lawyer_email: app.on_call_lawyer_email,
            bootstrap_company: crate::admin::bootstrap_company_from_env(),
            sessions: app.sessions,
            secure_cookies: false,
            attachment_scanner: app.attachment_scanner,
            runtime_kms: app.runtime_kms,
        };
        let form = || {
            axum::Form(NewTextForm {
                _csrf: None,
                to: "+15555550123".into(),
                body: "Hello".into(),
                consent: Some("yes".into()),
            })
        };
        let missing_session = new_text_post(State(state.clone()), None, form()).await;
        assert_eq!(missing_session.status(), StatusCode::NOT_FOUND);

        let mut session =
            crate::session::SessionData::fresh("owner@example.com", store::persons::Role::Owner);
        session.person_id = Some(Uuid::now_v7());
        std::env::remove_var("NAVIGATOR_SMS_NUMBER");
        let unconfigured =
            new_text_post(State(state), Some(axum::Extension(session)), form()).await;
        assert_eq!(unconfigured.status(), StatusCode::SEE_OTHER);
        let location = unconfigured.headers()[axum::http::header::LOCATION]
            .to_str()
            .unwrap();
        assert!(location.starts_with("/app/inbox?error="), "{location}");
    }

    #[tokio::test]
    async fn intake_reply_requires_a_session_and_a_visibility() {
        let app = crate::test_support::app_state(store::test_support::mem_surreal().await).await;
        let state = crate::admin::AdminState {
            surreal: app.surreal,
            workflow_runtime: app.workflow_runtime,
            signature_provider: app.signature_provider,
            retainer_intake_questionnaire: workflows::retainer_intake_questionnaire(),
            questionnaire_runtime: app.questionnaire_runtime,
            storage: app.storage,
            assets_storage: app.assets_storage,
            forms_registry: app.forms_registry,
            email: app.email,
            billing_provider: app.billing_provider,
            contract_reviewer: app.contract_reviewer,
            bootstrap_owner_email: app.bootstrap_owner_email,
            on_call_lawyer_email: app.on_call_lawyer_email,
            bootstrap_company: crate::admin::bootstrap_company_from_env(),
            sessions: app.sessions,
            secure_cookies: false,
            attachment_scanner: app.attachment_scanner,
            runtime_kms: app.runtime_kms,
        };
        let missing_session = intake_reply_post(
            State(state.clone()),
            None,
            Path(Uuid::nil()),
            axum::Form(ReplyForm {
                _csrf: None,
                body: "Hello".into(),
                visibility: Some("public".into()),
            }),
        )
        .await;
        assert_eq!(missing_session.status(), StatusCode::NOT_FOUND);
        let mut session =
            crate::session::SessionData::fresh("owner@example.com", store::persons::Role::Owner);
        session.person_id = Some(Uuid::now_v7());
        let missing_visibility = intake_reply_post(
            State(state),
            Some(axum::Extension(session)),
            Path(Uuid::nil()),
            axum::Form(ReplyForm {
                _csrf: None,
                body: "Hello".into(),
                visibility: None,
            }),
        )
        .await;
        assert_eq!(missing_visibility.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn signed_in_assign_redirects_when_the_conversation_is_missing() {
        let db = store::test_support::mem_surreal().await;
        let mut session =
            crate::session::SessionData::fresh("owner@example.com", store::persons::Role::Owner);
        session.person_id = Some(Uuid::now_v7());
        let response = assign_post(
            State(db),
            Some(axum::Extension(session)),
            Path(Uuid::nil()),
            axum::Form(AssignForm {
                _csrf: None,
                project_id: Uuid::nil(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }

    #[test]
    fn sms_signature_is_required_only_when_a_secret_is_configured() {
        std::env::remove_var("NAVIGATOR_SMS_WEBHOOK_SECRET");
        assert!(sms_authorized(&HeaderMap::new(), b"{}"));
        std::env::set_var("NAVIGATOR_SMS_WEBHOOK_SECRET", "synthetic-sms-secret");
        assert!(!sms_authorized(&HeaderMap::new(), b"{}"));
        std::env::remove_var("NAVIGATOR_SMS_WEBHOOK_SECRET");
    }

    #[tokio::test]
    async fn ingest_support_email_persists_public_mail_and_skips_lawyer_private_mail() {
        let db = store::test_support::mem_surreal().await;
        let firm = store::test_support::seed_firm_with_brand(&db, "neon").await;
        let public = ingest_support_email_for_firm(
            &db,
            &InboundEmail {
                from: "Reader <reader@example.com>".into(),
                to: "support@neonlaw.com".into(),
                subject: "A question".into(),
                text: "Please call me.".into(),
                message_id: Some("mid-support-1".into()),
                ..InboundEmail::default()
            },
            Some(firm),
        )
        .await
        .unwrap();
        assert!(public.is_some());
        let lawyer = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Inbox Lawyer",
                format!("inbox-lawyer-{}@example.com", Uuid::now_v7()),
                store::persons::Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        let skipped = ingest_support_email_for_firm(
            &db,
            &InboundEmail {
                from: lawyer.email,
                to: "private@example.com".into(),
                subject: "Internal".into(),
                text: "Do not intake.".into(),
                message_id: Some("mid-lawyer-1".into()),
                ..InboundEmail::default()
            },
            Some(firm),
        )
        .await
        .unwrap();
        assert!(skipped.is_none());
    }

    fn session(
        email: &str,
        role: store::persons::Role,
        person_id: Uuid,
    ) -> crate::session::SessionData {
        let mut session = crate::session::SessionData::fresh(email, role);
        session.person_id = Some(person_id);
        session
    }

    async fn inbox_matter(
        db: &store::surreal::SurrealDb,
    ) -> (
        store::persons::Person,
        store::persons::Person,
        store::firms::Firm,
        store::projects::Project,
    ) {
        let owner = store::persons::create(
            db,
            &store::persons::NewPerson::with_role(
                "Inbox Owner",
                format!("inbox-owner-{}@example.com", Uuid::now_v7()),
                store::persons::Role::Owner,
            ),
        )
        .await
        .unwrap();
        let admin = store::persons::create(
            db,
            &store::persons::NewPerson::with_role(
                "Inbox Admin",
                format!("inbox-admin-{}@example.com", Uuid::now_v7()),
                store::persons::Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm = store::firms::create(
            db,
            &store::firms::NewFirm {
                name: format!("Inbox Firm {}", admin.id),
                status: "active".into(),
                entity_id: store::test_support::seed_entity(db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        let project = store::projects::create(
            db,
            &store::projects::NewProject {
                code: format!("inbox-{}", Uuid::now_v7()),
                name: "Inbox Matter".into(),
                status: "open".into(),
                entity_id: store::test_support::seed_entity(db).await,
                firm_id: Some(firm.id),
                ..store::projects::NewProject::default()
            },
        )
        .await
        .unwrap();
        (owner, admin, firm, project)
    }

    fn inbound<'a>(
        firm_id: Uuid,
        channel: &'a str,
        account: &'a str,
        provider_message_id: &'a str,
        address: &'a str,
    ) -> store::inbox::IngestInbound<'a> {
        store::inbox::IngestInbound {
            firm_id,
            brand_key: Some("neon"),
            interaction_type: Some("comment"),
            provider_post_id: Some("post-1"),
            parent_provider_message_id: None,
            visibility: Some("public"),
            channel,
            provider_account: account,
            provider_message_id,
            provider_thread_ref: Some("thread-1"),
            external_address: address,
            external_name: Some("Reader"),
            subject: Some("Question"),
            body: "Hello from outside.",
            source_page: Some("/contact"),
            source_channel: Some(channel),
            matter_consent: true,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachment_asset_id: None,
            occurred_at: "2026-10-09T00:00:00Z",
        }
    }

    #[tokio::test]
    async fn signed_in_owner_assigns_and_a_lawyer_leaves_a_note() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, project) = inbox_matter(&db).await;
        let ingested = store::inbox::ingest_inbound(
            &db,
            &inbound(
                firm.id,
                store::inbox::channel::EMAIL,
                "support",
                "email-1",
                "reader@example.com",
            ),
        )
        .await
        .unwrap();
        let assigned = assign_post(
            State(db.clone()),
            Some(axum::Extension(session(
                &owner.email,
                store::persons::Role::Owner,
                owner.id,
            ))),
            Path(ingested.conversation_id),
            axum::Form(AssignForm {
                _csrf: None,
                project_id: project.id,
            }),
        )
        .await;
        assert_eq!(assigned.status(), StatusCode::SEE_OTHER);
        let location = assigned
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(
            location.contains(&format!(
                "/app/projects/{}/inbox/{}",
                project.code, ingested.conversation_id
            )),
            "{location}"
        );

        let lawyer = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Inbox Lawyer",
                format!("inbox-note-lawyer-{}@example.com", Uuid::now_v7()),
                store::persons::Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        store::firms::ensure_membership(
            &db,
            &store::firms::NewPersonFirmRole {
                person_id: lawyer.id,
                firm_id: firm.id,
                membership: store::firms::FirmMembership::Lawyer,
                is_dri: false,
            },
        )
        .await
        .unwrap();
        store::projects::designate_dri_in_surreal(
            &db,
            project.id,
            lawyer.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
        let noted = note_post(
            State(db),
            Some(axum::Extension(session(
                &lawyer.email,
                store::persons::Role::Lawyer,
                lawyer.id,
            ))),
            Path((project.code, ingested.conversation_id)),
            axum::Form(NoteForm {
                _csrf: None,
                body: "Private strategy.".into(),
            }),
        )
        .await;
        assert_eq!(noted.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn intake_reply_queues_a_public_facebook_comment() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, _project) = inbox_matter(&db).await;
        let ingested = store::inbox::ingest_inbound(
            &db,
            &inbound(
                firm.id,
                store::inbox::channel::FACEBOOK,
                "page-1",
                "comment-1",
                "reader-1",
            ),
        )
        .await
        .unwrap();
        let app = crate::test_support::app_state(db).await;
        let state = crate::admin::AdminState {
            surreal: app.surreal,
            workflow_runtime: app.workflow_runtime,
            signature_provider: app.signature_provider,
            retainer_intake_questionnaire: workflows::retainer_intake_questionnaire(),
            questionnaire_runtime: app.questionnaire_runtime,
            storage: app.storage,
            assets_storage: app.assets_storage,
            forms_registry: app.forms_registry,
            email: app.email,
            billing_provider: app.billing_provider,
            contract_reviewer: app.contract_reviewer,
            bootstrap_owner_email: app.bootstrap_owner_email,
            on_call_lawyer_email: app.on_call_lawyer_email,
            bootstrap_company: crate::admin::bootstrap_company_from_env(),
            sessions: app.sessions,
            secure_cookies: false,
            attachment_scanner: app.attachment_scanner,
            runtime_kms: app.runtime_kms,
        };
        let response = intake_reply_post(
            State(state),
            Some(axum::Extension(session(
                &owner.email,
                store::persons::Role::Owner,
                owner.id,
            ))),
            Path(ingested.conversation_id),
            axum::Form(ReplyForm {
                _csrf: None,
                body: "A public reply.".into(),
                visibility: Some("public".into()),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn participating_lawyer_sends_a_matter_reply() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, project) = inbox_matter(&db).await;
        let ingested = store::inbox::ingest_inbound(
            &db,
            &inbound(
                firm.id,
                store::inbox::channel::EMAIL,
                "support",
                "email-reply-1",
                "reader@example.com",
            ),
        )
        .await
        .unwrap();
        store::inbox::assign(&db, owner.id, ingested.conversation_id, project.id)
            .await
            .unwrap();
        let lawyer = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Reply Lawyer",
                format!("inbox-reply-lawyer-{}@example.com", Uuid::now_v7()),
                store::persons::Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        store::firms::ensure_membership(
            &db,
            &store::firms::NewPersonFirmRole {
                person_id: lawyer.id,
                firm_id: firm.id,
                membership: store::firms::FirmMembership::Lawyer,
                is_dri: false,
            },
        )
        .await
        .unwrap();
        store::projects::designate_dri_in_surreal(
            &db,
            project.id,
            lawyer.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
        let app = crate::test_support::app_state(db).await;
        let state = crate::admin::AdminState {
            surreal: app.surreal,
            workflow_runtime: app.workflow_runtime,
            signature_provider: app.signature_provider,
            retainer_intake_questionnaire: workflows::retainer_intake_questionnaire(),
            questionnaire_runtime: app.questionnaire_runtime,
            storage: app.storage,
            assets_storage: app.assets_storage,
            forms_registry: app.forms_registry,
            email: app.email,
            billing_provider: app.billing_provider,
            contract_reviewer: app.contract_reviewer,
            bootstrap_owner_email: app.bootstrap_owner_email,
            on_call_lawyer_email: app.on_call_lawyer_email,
            bootstrap_company: crate::admin::bootstrap_company_from_env(),
            sessions: app.sessions,
            secure_cookies: false,
            attachment_scanner: app.attachment_scanner,
            runtime_kms: app.runtime_kms,
        };
        let response = reply_post(
            State(state),
            Some(axum::Extension(session(
                &lawyer.email,
                store::persons::Role::Lawyer,
                lawyer.id,
            ))),
            Path((project.code, ingested.conversation_id)),
            axum::Form(ReplyForm {
                _csrf: None,
                body: "We received your message.".into(),
                visibility: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }
}
