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

/// Every triage action redirects back to the review queue.
const TRIAGE_QUEUE: &str = "/app/inbox/triage";
const INBOX: &str = "/app/inbox";

/// Start a message-keyed durable alert after the inbound Message has been
/// persisted. Repeated provider deliveries attach to the same Slack alert.
/// The `InboundMessageAlert` switch chooses the worker's DBOS ingress; the
/// Restate lane remains while it is off, and a deployment with neither
/// configured is a no-op, as before.
pub async fn start_inbound_message_alert(message_id: Uuid) -> Result<(), String> {
    let target = workflows::trigger::service_target(
        workflows::trigger::INBOUND_MESSAGE_ALERT_ENABLED_ENV,
        std::env::var(workflows::trigger::INBOUND_MESSAGE_ALERT_ENABLED_ENV)
            .ok()
            .as_deref(),
        std::env::var(workflows::trigger::DBOS_INGRESS_URL_ENV).ok(),
        std::env::var(workflows::trigger::DBOS_INGRESS_TOKEN_ENV).ok(),
    )
    .map_err(|error| error.to_string())?;
    let request = workflows::InboundMessageAlertRequest { message_id };
    match &target {
        workflows::trigger::ServiceTarget::Dbos { ingress, token } => {
            workflows::trigger::start_dbos_inbound_message_alert(
                ingress,
                token,
                &serde_json::json!({
                    "workflow_id":
                        workflows::trigger::inbound_message_alert_workflow_id(message_id),
                    "request": request,
                }),
            )
            .await
            .map_err(|error| error.to_string())
        }
        workflows::trigger::ServiceTarget::Restate => {
            let Some(ingress) = std::env::var("RESTATE_BROKER_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
            else {
                return Ok(());
            };
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
    }
}

/// Ingest a parsed inbound email's clean attachments as content-addressed
/// Assets. Quarantined attachments never reach here: the scanner strips
/// their bytes, and only their safe metadata survives on the email. The
/// sha256 comes from the same bytes the Asset stores, so the link row's
/// dedup key always matches the stored content.
///
/// # Errors
///
/// Propagates storage or database failures as the ingest door's error string.
pub(crate) async fn ingest_attachments(
    db: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    email: &InboundEmail,
    firm_id: Uuid,
    project_id: Option<Uuid>,
) -> Result<Vec<store::inbox::IngestAttachment>, String> {
    let mut attachments = Vec::new();
    for attachment in &email.attachments {
        let content_type = if attachment.content_type.trim().is_empty() {
            "application/octet-stream"
        } else {
            attachment.content_type.trim()
        };
        attachments.push(
            store::inbox::ingest_attachment(
                db,
                storage,
                firm_id,
                project_id,
                &attachment.filename,
                content_type,
                &attachment.bytes,
            )
            .await
            .map_err(|error| error.to_string())?,
        );
    }
    Ok(attachments)
}

/// Start the triage workflow for a fresh, non-deduped inbound Message, keyed
/// by the same Message ID as the alert. Best effort in the same sense as the
/// alert submission: a failure surfaces as the handler's 503 so the provider
/// retries the webhook and the durable submission eventually lands — ingest
/// itself is never rolled back, and the alert that was already submitted is
/// keyed identically and survives the replay untouched.
pub async fn start_inbound_message_triage(message_id: Uuid) -> Result<(), String> {
    let Some(ingress) = std::env::var("RESTATE_BROKER_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let request = workflows::InboundTriageRequest { message_id };
    workflows::start_workflow(
        &ingress,
        std::env::var("RESTATE_AUTH_TOKEN").ok().as_deref(),
        "InboundTriage",
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
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    email: &InboundEmail,
) -> Result<Option<store::inbox::IngestedInbound>, String> {
    ingest_support_email_for_firm(db, storage, email, inbox_firm_id()).await
}

async fn ingest_support_email_for_firm(
    db: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
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
    // Keep the source on this normalized message even without optional
    // threading or an RFC Message-ID. The content key also survives replay.
    let raw_storage_key = if email.raw.is_empty() {
        None
    } else {
        let key = format!(
            "inbound/inbox/{firm_id}/{}.eml",
            store::assets::sha256_hex(&email.raw)
        );
        storage
            .put(&key, &email.raw, "message/rfc822")
            .await
            .map_err(|error| error.to_string())?;
        Some(key)
    };
    let email_conversation_id = linked_email_conversation(db, email, &from).await?;
    let attachments = ingest_attachments(db, storage, email, firm_id, None).await?;
    let account = std::env::var("NAVIGATOR_EMAIL_PROVIDER_ACCOUNT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "support".into());
    store::inbox::ingest_inbound(
        db,
        &store::inbox::IngestInbound {
            project_id: None,
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
            attachments,
            occurred_at: &chrono::Utc::now().to_rfc3339(),
            raw_storage_key: raw_storage_key.as_deref(),
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

pub(crate) fn provider_message_id(email: &InboundEmail) -> String {
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
    /// `inbox` returns to the firm inbox after assigning — the reading pane's
    /// control — instead of opening the Project thread.
    #[serde(default)]
    next: Option<String>,
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
        Ok(_) if form.next.as_deref() == Some("inbox") => Redirect::to(INBOX).into_response(),
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

async fn triage_path_matches(
    db: &store::surreal::SurrealDb,
    conversation_id: Uuid,
    triage_id: Uuid,
) -> bool {
    let Ok(Some(triage)) = store::inbox_triage::find_by_id(db, triage_id).await else {
        return false;
    };
    matches!(
        store::inbox::message_by_id(db, triage.message_id).await,
        Ok(Some(message)) if message.conversation_id == conversation_id
    )
}

/// `POST /app/inbox/{conversation_id}/triage/{triage_id}/confirm/{project_id}`
///
/// A person confirms the classifier's suggested Project. The store door
/// re-checks the suggestion matches, runs the participation-checked
/// assignment, and records the Confirmed disposition.
pub async fn triage_confirm_post(
    State(db): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path((conversation_id, triage_id, project_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Response {
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !triage_path_matches(&db, conversation_id, triage_id).await {
        return StatusCode::NOT_FOUND.into_response();
    }
    match store::inbox_triage::confirm_project(&db, actor_id, triage_id, project_id).await {
        Ok(_) => Redirect::to(TRIAGE_QUEUE).into_response(),
        Err(error) => redirect_error(TRIAGE_QUEUE, &error.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct TriageOverrideForm {
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
    project_id: Uuid,
    rationale: String,
}

/// `POST /app/inbox/{conversation_id}/triage/{triage_id}/override`
///
/// A person corrects the suggestion with a different Project and a
/// rationale; the correction is recorded as a labeled example.
pub async fn triage_override_post(
    State(db): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path((conversation_id, triage_id)): Path<(Uuid, Uuid)>,
    axum::Form(form): axum::Form<TriageOverrideForm>,
) -> Response {
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !triage_path_matches(&db, conversation_id, triage_id).await {
        return StatusCode::NOT_FOUND.into_response();
    }
    match store::inbox_triage::override_project(
        &db,
        actor_id,
        triage_id,
        form.project_id,
        &form.rationale,
    )
    .await
    {
        Ok(_) => Redirect::to(TRIAGE_QUEUE).into_response(),
        Err(error) => redirect_error(TRIAGE_QUEUE, &error.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct TriageResolveForm {
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
    privilege: String,
    rationale: String,
}

/// `POST /app/inbox/{conversation_id}/triage/{triage_id}/resolve`
///
/// A person settles an uncertain privilege label. The rule layer stays
/// authoritative server-side for public posts. Private messages accept a
/// human privilege decision.
pub async fn triage_resolve_post(
    State(db): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path((conversation_id, triage_id)): Path<(Uuid, Uuid)>,
    axum::Form(form): axum::Form<TriageResolveForm>,
) -> Response {
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !triage_path_matches(&db, conversation_id, triage_id).await {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(requested) = store::inbox_triage::PrivilegeLabel::from_form(&form.privilege) else {
        return redirect_error(TRIAGE_QUEUE, "unknown privilege label");
    };
    match store::inbox_triage::resolve_privilege(
        &db,
        actor_id,
        triage_id,
        requested,
        &form.rationale,
    )
    .await
    {
        Ok(_) => Redirect::to(TRIAGE_QUEUE).into_response(),
        Err(error) => redirect_error(TRIAGE_QUEUE, &error.to_string()),
    }
}

/// `POST /app/inbox/{conversation_id}/triage/{triage_id}/rerun`
///
/// A fresh pass of the rule layer over the row: the label is recomputed
/// from the message's channel and the disposition follows. A decided row
/// returns to the queue.
pub async fn triage_rerun_post(
    State(db): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path((conversation_id, triage_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !triage_path_matches(&db, conversation_id, triage_id).await {
        return StatusCode::NOT_FOUND.into_response();
    }
    match store::inbox_triage::rerun(&db, actor_id, triage_id).await {
        Ok(_) => Redirect::to(TRIAGE_QUEUE).into_response(),
        Err(error) => redirect_error(TRIAGE_QUEUE, &error.to_string()),
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

/// `GET /app/inbox/{conversation_id}/messages/{message_id}/attachments/{asset_id}`
///
/// One Message attachment from the firm inbox, under the same firm-person
/// authorization as the conversation itself: the bytes stream only while the
/// viewer may read the bodies, and only for an asset linked to that Message.
pub async fn attachment_get(
    State(state): State<AdminState>,
    session: Option<Extension<SessionData>>,
    Path((conversation_id, message_id, asset_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Response {
    let Some(actor_id) = actor_id(session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(detail) = store::inbox::intake_detail(&state.surreal, actor_id, conversation_id).await
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(message) = detail
        .messages
        .iter()
        .find(|message| message.id == message_id)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let linked = store::inbox::message_assets(
        &state.surreal,
        detail.conversation.firm_id,
        message_id,
    )
    .await
    .unwrap_or_default()
    .into_iter()
    .find(|link| link.asset_id == asset_id);
    if linked.is_none() && message.attachment_asset_id != Some(asset_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(Some(asset)) = store::assets::find_by_id(&state.surreal, asset_id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if asset
        .project_id
        .is_some_and(|id| Some(id) != detail.conversation.project_id)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let filename = linked
        .map(|link| link.filename)
        .or_else(|| asset.filename.clone())
        .unwrap_or_else(|| "attachment".into());
    match state.storage.get(&asset.storage_key).await {
        Ok(object) => crate::project_documents::attachment_bytes_response(
            object.bytes,
            &asset.content_type,
            &filename,
        ),
        Err(cloud::StorageError::NotFound(_)) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::warn!(%error, "inbox attachment read failed");
            StatusCode::BAD_GATEWAY.into_response()
        }
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
    // The deployment's sending number must be a registered number: the
    // registry — not the webhook host — says which Brand and Firm the
    // conversation belongs to.
    let Some(registry) = state.twilio_registry.as_deref() else {
        return redirect_error(back, "Texting is not configured on this deployment");
    };
    let registered = match store::brand_phone_numbers::find_by_number(registry, &config.number)
        .await
    {
        Ok(Some(registered)) => registered,
        Ok(None) => return redirect_error(back, "Texting is not configured on this deployment"),
        Err(error) => return redirect_error(back, &error.to_string()),
    };
    let firm_id = match store::firms::firm_id_for_brand_key(&state.surreal, &registered.brand_key)
        .await
    {
        Ok(Some(firm_id)) => firm_id,
        Ok(None) => return redirect_error(back, "The texting number's Brand has no owning Firm"),
        Err(error) => return redirect_error(back, &error.to_string()),
    };
    let queued = store::inbox::open_sms_conversation(
        &state.surreal,
        actor_id,
        firm_id,
        Some(&registered.brand_key),
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
            project_id: None,
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
            attachments: Vec::new(),
            occurred_at: &chrono::Utc::now().to_rfc3339(),
            raw_storage_key: None,
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
            if start_inbound_message_triage(receipt.message_id)
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

    async fn storage() -> std::sync::Arc<dyn cloud::StorageService> {
        crate::test_support::fs_storage().await
    }

    #[tokio::test]
    async fn unconfigured_support_mail_is_ignored() {
        let db = store::test_support::mem_surreal().await;
        let storage = storage().await;
        ingest_support_email(&db, &storage, &InboundEmail::default())
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
                next: None,
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
        let storage = storage().await;
        let neon = store::test_support::seed_firm_with_brand(&db, "neon").await;
        let shook = store::test_support::seed_firm_with_brand(&db, "guieb-shook").await;
        for (mailbox, firm, brand) in [
            ("support@neonlaw.com", neon, "neon"),
            ("contact@guiebshook.com", shook, "guieb-shook"),
        ] {
            let email = InboundEmail {
                from: "sender@example.com".into(),
                to: mailbox.into(),
                text: "Synthetic inquiry".into(),
                message_id: Some("synthetic-provider-id".into()),
                ..InboundEmail::default()
            };
            let saved = ingest_support_email_for_firm(&db, &storage, &email, None)
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
                &storage,
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
            twilio_registry: None,
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
            twilio_registry: None,
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
            twilio_registry: None,
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
                next: None,
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
        let storage = storage().await;
        let public = ingest_support_email_for_firm(
            &db,
            &storage,
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
            &storage,
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
            project_id: None,
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
            attachments: Vec::new(),
            raw_storage_key: None,
            occurred_at: "2026-10-09T00:00:00Z",
        }
    }

    #[tokio::test]
    async fn the_reading_pane_assign_returns_to_the_inbox() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, project) = inbox_matter(&db).await;
        let ingested = store::inbox::ingest_inbound(
            &db,
            &inbound(
                firm.id,
                store::inbox::channel::EMAIL,
                "support",
                "email-pane-assign",
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
                next: Some("inbox".into()),
            }),
        )
        .await;
        assert_eq!(assigned.status(), StatusCode::SEE_OTHER);
        assert_eq!(assigned.headers()[axum::http::header::LOCATION], "/app/inbox");
    }

    #[tokio::test]
    async fn a_firm_inbox_attachment_streams_only_for_its_own_message() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, _project) = inbox_matter(&db).await;
        let app = crate::test_support::app_state(db.clone()).await;
        let attachment = store::inbox::ingest_attachment(
            &db,
            &app.storage,
            firm.id,
            None,
            "photo.png",
            "image/png",
            b"synthetic png bytes",
        )
        .await
        .unwrap();
        let mut with_photo = inbound(
            firm.id,
            store::inbox::channel::SMS,
            "sms:+15555550100",
            "mms-1",
            "+15555550123",
        );
        with_photo.attachments = vec![attachment.clone()];
        let ingested = store::inbox::ingest_inbound(&db, &with_photo).await.unwrap();
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
            twilio_registry: None,
        };
        let owner_session = || {
            Some(axum::Extension(session(
                &owner.email,
                store::persons::Role::Owner,
                owner.id,
            )))
        };
        let get = |session, ids: (Uuid, Uuid, Uuid)| {
            attachment_get(State(state.clone()), session, Path(ids))
        };
        let ids = (
            ingested.conversation_id,
            ingested.message_id,
            attachment.asset_id,
        );
        let ok = get(owner_session(), ids).await;
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(ok.headers()[axum::http::header::CONTENT_TYPE], "image/png");
        assert_eq!(
            axum::body::to_bytes(ok.into_body(), usize::MAX).await.unwrap(),
            b"synthetic png bytes".as_slice()
        );
        assert_eq!(get(None, ids).await.status(), StatusCode::NOT_FOUND);
        for denied in [
            (Uuid::now_v7(), ids.1, ids.2),
            (ids.0, Uuid::now_v7(), ids.2),
            (ids.0, ids.1, Uuid::now_v7()),
        ] {
            assert_eq!(
                get(owner_session(), denied).await.status(),
                StatusCode::NOT_FOUND
            );
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
                next: None,
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
            twilio_registry: None,
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
            twilio_registry: None,
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

    // -- The triage review queue's doors ----------------------------------------

    use store::inbox_triage::{NewInboxTriage, PrivilegeLabel, TriageDisposition};

    /// A NeedsHuman email triage row with a suggested Project, over a
    /// fresh conversation, in `firm`'s inbox.
    async fn triaged_email(
        db: &store::surreal::SurrealDb,
        firm: Uuid,
        tag: &str,
        project: Option<Uuid>,
    ) -> (Uuid, store::inbox::IngestedInbound) {
        let message = store::inbox::ingest_inbound(
            db,
            &inbound(
                firm,
                store::inbox::channel::EMAIL,
                "support",
                tag,
                "reader@example.com",
            ),
        )
        .await
        .unwrap();
        let row = store::inbox_triage::record(
            db,
            &NewInboxTriage {
                message_id: message.message_id,
                firm_id: firm,
                suggested_project_id: project,
                project_confidence: None,
                privilege: PrivilegeLabel::Uncertain,
                privilege_confidence: None,
                rationale: Some("uncertain: classifier below floor".into()),
                model_id: Some("gemini-flash-lite".into()),
                prompt_version: "triage-v1".into(),
                raw_output: None,
                disposition: TriageDisposition::NeedsHuman,
                labeled_example: false,
            },
        )
        .await
        .unwrap();
        (row.id, message)
    }

    fn location(response: &Response) -> Option<String> {
        response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    #[tokio::test]
    async fn triage_actions_reject_a_mismatched_conversation_path() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _, firm, project) = inbox_matter(&db).await;
        let mut outcomes = Vec::new();
        for action in ["confirm", "override", "resolve", "rerun"] {
            let (triage_id, _) = triaged_email(&db, firm.id, action, Some(project.id)).await;
            let original = store::inbox_triage::find_by_id(&db, triage_id)
                .await
                .unwrap()
                .unwrap();
            let signed_in = Some(axum::Extension(session(&owner.email, owner.role, owner.id)));
            let wrong = Uuid::now_v7();
            let response = match action {
                "confirm" => {
                    triage_confirm_post(
                        State(db.clone()),
                        signed_in,
                        Path((wrong, triage_id, project.id)),
                    )
                    .await
                }
                "override" => {
                    triage_override_post(
                        State(db.clone()),
                        signed_in,
                        Path((wrong, triage_id)),
                        axum::Form(TriageOverrideForm {
                            _csrf: None,
                            project_id: project.id,
                            rationale: "synthetic correction".into(),
                        }),
                    )
                    .await
                }
                "resolve" => {
                    triage_resolve_post(
                        State(db.clone()),
                        signed_in,
                        Path((wrong, triage_id)),
                        axum::Form(TriageResolveForm {
                            _csrf: None,
                            privilege: "privileged".into(),
                            rationale: "synthetic review".into(),
                        }),
                    )
                    .await
                }
                _ => {
                    triage_rerun_post(State(db.clone()), signed_in, Path((wrong, triage_id))).await
                }
            };
            let unchanged = store::inbox_triage::find_by_id(&db, triage_id)
                .await
                .unwrap()
                .unwrap()
                == original;
            outcomes.push((action, response.status(), unchanged));
        }
        assert!(
            outcomes
                .iter()
                .all(|(_, status, unchanged)| *status == StatusCode::NOT_FOUND && *unchanged),
            "a mismatched path must never mutate a triage row: {outcomes:?}"
        );
    }

    #[tokio::test]
    async fn triage_doors_require_a_session() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, project) = inbox_matter(&db).await;
        let (triage_id, _) =
            triaged_email(&db, firm.id, "triage-door-anon", Some(project.id)).await;
        let state = crate::test_support::app_state(db).await;

        let confirmed = triage_confirm_post(
            State(state.surreal.clone()),
            None,
            Path((Uuid::now_v7(), triage_id, project.id)),
        )
        .await;
        assert_eq!(confirmed.status(), StatusCode::NOT_FOUND);

        let resolved = triage_resolve_post(
            State(state.surreal.clone()),
            None,
            Path((Uuid::now_v7(), triage_id)),
            axum::Form(TriageResolveForm {
                _csrf: None,
                privilege: "privileged".into(),
                rationale: "why".into(),
            }),
        )
        .await;
        assert_eq!(resolved.status(), StatusCode::NOT_FOUND);

        assert_eq!(
            store::inbox_triage::find_by_id(&state.surreal, triage_id)
                .await
                .unwrap()
                .unwrap()
                .disposition,
            TriageDisposition::NeedsHuman
        );
        let _ = owner;
    }

    #[tokio::test]
    async fn an_owner_confirms_a_suggestion_and_it_persists() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, project) = inbox_matter(&db).await;
        let (triage_id, message) =
            triaged_email(&db, firm.id, "triage-owner-confirm", Some(project.id)).await;
        let state = crate::test_support::app_state(db).await;

        let response = triage_confirm_post(
            State(state.surreal.clone()),
            Some(axum::Extension(session(
                &owner.email,
                store::persons::Role::Owner,
                owner.id,
            ))),
            Path((message.conversation_id, triage_id, project.id)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(location(&response).as_deref(), Some(TRIAGE_QUEUE));

        let conversation =
            store::inbox::conversation_by_id(&state.surreal, message.conversation_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(conversation.project_id, Some(project.id));
        let row = store::inbox_triage::find_by_id(&state.surreal, triage_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.disposition, TriageDisposition::NeedsHuman);
        assert!(!row.labeled_example);
        // Assignment is complete; uncertain privilege still waits for review.
        assert_eq!(
            store::inbox_triage::list_for_review(&state.surreal, owner.id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_firm_member_confirms_only_while_participating() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, project) = inbox_matter(&db).await;
        let (triage_id, message) =
            triaged_email(&db, firm.id, "triage-lawyer-confirm", Some(project.id)).await;
        let lawyer = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Triage Lawyer",
                format!("triage-lawyer-{}@example.com", Uuid::now_v7()),
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
        let state = crate::test_support::app_state(db).await;

        // Firm membership alone is not the participation check.
        let denied = triage_confirm_post(
            State(state.surreal.clone()),
            Some(axum::Extension(session(
                &lawyer.email,
                store::persons::Role::Lawyer,
                lawyer.id,
            ))),
            Path((message.conversation_id, triage_id, project.id)),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::SEE_OTHER);
        assert!(location(&denied)
            .unwrap_or_default()
            .starts_with(&format!("{TRIAGE_QUEUE}?error=")));
        let conversation =
            store::inbox::conversation_by_id(&state.surreal, message.conversation_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(conversation.project_id, None);

        // With active participation the same action lands.
        store::projects::add_participation(
            &state.surreal,
            project.id,
            lawyer.id,
            store::projects::participation_for_role(store::persons::Role::Lawyer),
        )
        .await
        .unwrap();
        let confirmed = triage_confirm_post(
            State(state.surreal.clone()),
            Some(axum::Extension(session(
                &lawyer.email,
                store::persons::Role::Lawyer,
                lawyer.id,
            ))),
            Path((message.conversation_id, triage_id, project.id)),
        )
        .await;
        assert_eq!(confirmed.status(), StatusCode::SEE_OTHER);
        let conversation =
            store::inbox::conversation_by_id(&state.surreal, message.conversation_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(conversation.project_id, Some(project.id));
        // The Owner's own door still works on another row: sanity in one.
        assert_eq!(
            store::inbox_triage::list_for_review(&state.surreal, owner.id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn an_override_persists_the_correction_as_a_labeled_example() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, project) = inbox_matter(&db).await;
        let (triage_id, message) = triaged_email(&db, firm.id, "triage-override", None).await;
        let state = crate::test_support::app_state(db).await;

        let response = triage_override_post(
            State(state.surreal.clone()),
            Some(axum::Extension(session(
                &owner.email,
                store::persons::Role::Owner,
                owner.id,
            ))),
            Path((message.conversation_id, triage_id)),
            axum::Form(TriageOverrideForm {
                _csrf: None,
                project_id: project.id,
                rationale: "the sender is on this matter".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let row = store::inbox_triage::find_by_id(&state.surreal, triage_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.disposition, TriageDisposition::NeedsHuman);
        assert!(row.labeled_example);
        let conversation =
            store::inbox::conversation_by_id(&state.surreal, message.conversation_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(conversation.project_id, Some(project.id));
    }

    #[tokio::test]
    async fn resolving_privilege_keeps_the_rule_layer_authoritative() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, _project) = inbox_matter(&db).await;

        // The human settles client email either way.
        let (email_row, email_message) =
            triaged_email(&db, firm.id, "triage-resolve-email", None).await;
        // A social row: the structural rule outranks the form.
        let social = store::inbox::ingest_inbound(
            &db,
            &inbound(
                firm.id,
                store::inbox::channel::INSTAGRAM,
                "ig-page",
                "triage-resolve-social",
                "poster@example.com",
            ),
        )
        .await
        .unwrap();
        let social_row = store::inbox_triage::record(
            &db,
            &NewInboxTriage {
                message_id: social.message_id,
                firm_id: firm.id,
                suggested_project_id: None,
                project_confidence: None,
                privilege: PrivilegeLabel::NotPrivileged,
                privilege_confidence: None,
                rationale: None,
                model_id: None,
                prompt_version: store::inbox_triage::RULES_ONLY_PROMPT.into(),
                raw_output: None,
                disposition: TriageDisposition::NeedsHuman,
                labeled_example: false,
            },
        )
        .await
        .unwrap()
        .id;
        let state = crate::test_support::app_state(db).await;

        let settled = triage_resolve_post(
            State(state.surreal.clone()),
            Some(axum::Extension(session(
                &owner.email,
                store::persons::Role::Owner,
                owner.id,
            ))),
            Path((email_message.conversation_id, email_row)),
            axum::Form(TriageResolveForm {
                _csrf: None,
                privilege: "privileged".into(),
                rationale: "a client discussing their matter".into(),
            }),
        )
        .await;
        assert_eq!(settled.status(), StatusCode::SEE_OTHER);
        let row = store::inbox_triage::find_by_id(&state.surreal, email_row)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.privilege, PrivilegeLabel::Privileged);

        // The social row's forced label: the form asked for privileged.
        let forced = triage_resolve_post(
            State(state.surreal.clone()),
            Some(axum::Extension(session(
                &owner.email,
                store::persons::Role::Owner,
                owner.id,
            ))),
            Path((social.conversation_id, social_row)),
            axum::Form(TriageResolveForm {
                _csrf: None,
                privilege: "privileged".into(),
                rationale: "looks privileged to me".into(),
            }),
        )
        .await;
        assert_eq!(forced.status(), StatusCode::SEE_OTHER);
        let row = store::inbox_triage::find_by_id(&state.surreal, social_row)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.privilege,
            PrivilegeLabel::NotPrivileged,
            "the structural rule outranks the form"
        );
    }

    #[tokio::test]
    async fn a_rerun_returns_a_decided_row_to_the_queue() {
        let db = store::test_support::mem_surreal().await;
        let (owner, _admin, firm, project) = inbox_matter(&db).await;
        let (triage_id, message) =
            triaged_email(&db, firm.id, "triage-rerun", Some(project.id)).await;
        store::projects::add_participation(
            &db,
            project.id,
            owner.id,
            store::projects::participation_for_role(owner.role),
        )
        .await
        .unwrap();
        let state = crate::test_support::app_state(db).await;
        let owner_session = Some(axum::Extension(session(
            &owner.email,
            store::persons::Role::Owner,
            owner.id,
        )));

        triage_confirm_post(
            State(state.surreal.clone()),
            owner_session.clone(),
            Path((message.conversation_id, triage_id, project.id)),
        )
        .await
        .status();
        let response = triage_rerun_post(
            State(state.surreal.clone()),
            owner_session,
            Path((message.conversation_id, triage_id)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let row = store::inbox_triage::find_by_id(&state.surreal, triage_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.privilege, PrivilegeLabel::Uncertain);
        assert_eq!(row.disposition, TriageDisposition::NeedsHuman);
        assert_eq!(
            store::inbox_triage::list_for_review(&state.surreal, owner.id)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
