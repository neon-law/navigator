//! Shared Firm-scoped message and Project inbox.
//!
//! Email and SMS land on one Firm-scoped conversation. A participating Lawyer
//! or Clerk may attribute it to a same-Firm Project after intake. Communication
//! rows stay project-scoped and move with reassignment.
//!
//! Email and SMS matter replies require a Lawyer or Clerk who participates on
//! the assigned Project at dispatch time. Social engagement has its own
//! Firm-scoped authorization path for Owner/Admin and the same participation
//! check for Lawyers/Clerks. A Lawyer approval is not a send. System
//! acknowledgments only confirm persisted intake.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::access::can_see_project;
use crate::communications::{self, IngestArgs};
use crate::external_identities;
use crate::persons::{self, Role};
use crate::postgres_mirror::{after_write, MirrorKey};
use crate::projects::{self, ProjectStoreError};
use crate::surreal::{record_id, record_uuid, SurrealDb};

const CONVERSATION: &str = "inbox_conversation";
const MESSAGE: &str = "message";
const MESSAGE_ASSET: &str = "inbox_message_asset";
const INGRESS: &str = "inbox_ingress";
const ASSIGNMENT: &str = "inbox_assignment";
const ATTEMPT: &str = "inbox_delivery_attempt";

pub mod channel {
    pub const EMAIL: &str = "email";
    pub const SMS: &str = "sms";
    pub const VOICE: &str = "voice";
    pub const FACEBOOK: &str = "facebook";
    pub const INSTAGRAM: &str = "instagram";
    pub const THREADS: &str = "threads";
    pub const YOUTUBE: &str = "youtube";
    pub const TIKTOK: &str = "tiktok";
    /// Court and program scrapers file their notices here. The Firm inbox
    /// handles them exactly like any other intake: the message carries the
    /// document as an attachment, and triage assigns it.
    pub const SCRAPER: &str = "scraper";
    pub const X: &str = "x";
    /// Imported LinkedIn direct messages (LAW-238 D1). The ingest source is
    /// the member's own data export, not a provider webhook, but the channel
    /// literal, threading, and dedup are exactly the other social channels'.
    pub const LINKEDIN: &str = "linkedin";
}

pub mod direction {
    pub const INBOUND: &str = "inbound";
    pub const OUTBOUND: &str = "outbound";
    pub const INTERNAL: &str = "internal";
}

pub mod delivery {
    pub const RECEIVED: &str = "received";
    pub const QUEUED: &str = "queued";
    pub const SUBMITTED: &str = "submitted";
    pub const DELIVERED: &str = "delivered";
    pub const UNKNOWN: &str = "unknown";
    pub const FAILED: &str = "failed";
    pub const SUPPRESSED: &str = "suppressed";
}

pub mod purpose {
    pub const MATTER: &str = "matter";
    pub const PROMOTIONAL: &str = "promotional";
}

/// The NEF pipeline's recorded outcome spellings. A triage outcome marks a
/// processed message that stays in the lawyer's backlog because nothing was
/// filed — never guess what a notice means.
pub mod nef_outcome {
    /// The notice's documents and docket entry were filed on a matter.
    pub const FILED: &str = "filed";
    /// Available notice documents were filed without a numbered docket entry.
    pub const TEXT_ONLY: &str = "text_only";
    /// A rerun over an entry the pipeline already filed.
    pub const ALREADY_RECORDED: &str = "already_recorded";
    /// The notice names no Project the pipeline could resolve.
    pub const NO_PROJECT: &str = "no_project";
    /// The message is not a notice the pipeline recognizes.
    pub const NOT_A_NOTICE: &str = "not_a_notice";
    /// A recognized NEF without a resolvable case number.
    pub const NO_CASE_NUMBER: &str = "no_case_number";

    /// Outcomes that leave the message in the unprocessed backlog after a
    /// process run records them.
    pub const TRIAGE: &[&str] = &[NO_PROJECT, NOT_A_NOTICE, NO_CASE_NUMBER];

    /// Every outcome spelling, in pipeline order.
    pub const ALL: &[&str] = &[
        FILED,
        TEXT_ONLY,
        ALREADY_RECORDED,
        NO_PROJECT,
        NOT_A_NOTICE,
        NO_CASE_NUMBER,
    ];

    /// Whether a recorded outcome keeps the message in the backlog.
    #[must_use]
    pub fn is_triage(outcome: &str) -> bool {
        TRIAGE.contains(&outcome)
    }
}

/// The inbox-process route vocabulary: the strings a process run records on
/// a message's `route` field, and the schema's ASSERT vocabulary. Portal
/// classification (`portal::inbound_email`) maps its verdict onto these
/// spellings; see `docs/glossary/inbox-process.md`.
pub mod route {
    /// A court-generated Notice of Electronic Filing.
    pub const NEF: &str = "nef";
    /// A bankruptcy claims-agent notice from a recognized noticing vendor.
    pub const CLAIMS_AGENT: &str = "claims_agent";
    /// Mail from a fixed-fee legal plan (ARAG, `LegalEASE`, `LegalShield`).
    pub const PLAN_ASSIGNMENT: &str = "plan_assignment";
    /// Court mail that is not a noticing event, or a hearing invitation.
    pub const CHAMBERS: &str = "chambers";
    /// Anything the classification layer cannot name confidently.
    pub const OTHER: &str = "other";

    /// Every route spelling, in classification order.
    pub const ALL: &[&str] = &[NEF, CLAIMS_AGENT, PLAN_ASSIGNMENT, CHAMBERS, OTHER];

    /// Whether `value` is a route the schema accepts on `message.route`.
    #[must_use]
    pub fn is_valid(value: &str) -> bool {
        ALL.contains(&value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)] // Independent flags: unread, matter consent, promotional opt-in, promotional opt-out.
pub struct Conversation {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub brand_key: Option<String>,
    pub interaction_type: String,
    pub provider_post_id: Option<String>,
    pub project_id: Option<Uuid>,
    pub channel: String,
    pub external_address: String,
    pub external_name: Option<String>,
    pub subject: Option<String>,
    pub unread: bool,
    pub provider_account: String,
    pub provider_thread_ref: Option<String>,
    pub source_page: Option<String>,
    pub source_channel: Option<String>,
    pub matter_consent: bool,
    pub promotional_opt_in: bool,
    pub promotional_opt_out: bool,
    pub email_conversation_id: Option<Uuid>,
    /// The Person the inbound email sender resolves to — persisted at
    /// ingest (primary or `person_mailbox` alias match). A lawyer- or
    /// clerk-tier sender never links here; see `sender_identity`.
    pub person_id: Option<Uuid>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub parent_message_id: Option<Uuid>,
    pub visibility: Option<String>,
    pub direction: String,
    pub body: String,
    pub author_person_id: Option<Uuid>,
    pub provider_message_id: Option<String>,
    pub provider_ref: Option<String>,
    pub delivery_state: String,
    pub purpose: String,
    pub attachment_asset_id: Option<Uuid>,
    /// Object-storage key of the verbatim payload this Message was ingested
    /// from — the raw `.eml`, or the member data-export CSV for an imported
    /// LinkedIn thread (LAW-238 D1). Webhook-delivered mail retains its raw
    /// source here independently of optional email threading.
    pub raw_storage_key: Option<String>,
    pub communication_id: Option<Uuid>,
    pub occurred_at: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assignment {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub firm_id: Uuid,
    pub from_project_id: Option<Uuid>,
    pub to_project_id: Uuid,
    pub actor_person_id: Uuid,
    pub inserted_at: DateTime<Utc>,
}

/// One attachment on an ingested provider message: its content-addressed
/// Asset, original filename, and sha256. The provider's attachment order is
/// preserved on the link; the first attachment is the Message's primary one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestAttachment {
    pub asset_id: Uuid,
    pub filename: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct IngestInbound<'a> {
    pub firm_id: Uuid,
    /// Project attribution set at ingest time (the CLI ingest door). Support
    /// and webhook paths leave it `None`: the conversation stays on the Firm
    /// until an Owner assigns it. Only threads carrying the same attribution
    /// absorb the delivery — an unassigned thread never silently adopts one.
    pub project_id: Option<Uuid>,
    pub brand_key: Option<&'a str>,
    pub interaction_type: Option<&'a str>,
    pub provider_post_id: Option<&'a str>,
    pub parent_provider_message_id: Option<&'a str>,
    pub visibility: Option<&'a str>,
    pub channel: &'a str,
    pub provider_account: &'a str,
    pub provider_message_id: &'a str,
    pub provider_thread_ref: Option<&'a str>,
    pub external_address: &'a str,
    pub external_name: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub body: &'a str,
    pub source_page: Option<&'a str>,
    pub source_channel: Option<&'a str>,
    pub matter_consent: bool,
    pub promotional_opt_in: bool,
    pub email_conversation_id: Option<Uuid>,
    pub attachments: Vec<IngestAttachment>,
    pub occurred_at: &'a str,
    /// Object-storage key of the archived source, including raw email and
    /// import payloads; see [`Message::raw_storage_key`]. Absent when unavailable.
    pub raw_storage_key: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestedInbound {
    pub conversation_id: Uuid,
    pub message_id: Uuid,
    pub deduped: bool,
    /// Whether this delivery wrote the conversation's `person_id` — the
    /// event a Relationship Log entry records exactly once per thread.
    pub person_linked: bool,
}

#[derive(Debug, Clone)]
pub struct OutboundEnvelope {
    pub channel: String,
    pub account: String,
    pub interaction_type: String,
    /// Provider post or media containing the interaction, when available.
    pub provider_post_id: Option<String>,
    /// Provider comment/message that this reply should answer.
    pub reply_to_provider_message_id: Option<String>,
    /// Provider thread or recipient reference used by messaging APIs.
    pub provider_thread_ref: Option<String>,
    pub to: String,
    pub subject: Option<String>,
    pub body: String,
    pub purpose: String,
    pub visibility: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportOutcome {
    Submitted { provider_ref: String },
    Unknown,
    Rejected { detail: String },
}

#[async_trait]
pub trait OutboundTransport: Send + Sync {
    async fn deliver(&self, envelope: &OutboundEnvelope) -> Result<TransportOutcome, String>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchReport {
    pub submitted: usize,
    pub unknown: usize,
    pub suppressed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntakeRow {
    /// The Person whose recorded phone is this text's number, when known.
    pub contact: Option<ContactLink>,
    pub id: Uuid,
    pub firm_id: Uuid,
    pub brand_key: Option<String>,
    pub project_id: Option<Uuid>,
    pub channel: String,
    pub assignee: String,
    pub unread: bool,
    pub delivery: String,
    pub next_action: String,
    pub external_address: String,
    pub external_name: Option<String>,
    pub subject: Option<String>,
    /// The direction of the conversation's most recent message — what the
    /// `?direction=` filter matches rows on (LAW-238 D3). Empty when the
    /// conversation has no messages yet, so an empty conversation matches
    /// no direction filter.
    pub direction: String,
    /// Whether any message on the conversation carries a filed attachment.
    /// What the `?attachments=true` filter matches rows on.
    pub has_attachments: bool,
    /// Whether a Person on the firm side sent a message on the conversation —
    /// an outbound message with an author. What the inbox's Sent folder
    /// lists; an automated acknowledgement has no author and never counts.
    pub sent: bool,
    /// Opening snippet of the conversation's most recent written message
    /// (same `occurred_at, id` order as [`IntakeRow::direction`]), whitespace
    /// collapsed and ellipsized — what a list row renders beneath the
    /// subject. An automated acknowledgement (outbound, no author) is skipped
    /// so the row previews what the counterparty or the firm actually wrote.
    /// None when no such message exists or its body is blank.
    pub preview: Option<String>,
    /// The Person the inbound sender resolved to at ingest, when one was
    /// persisted — display only, as [`IntakeRow::contact`] is.
    pub person: Option<PersonBadge>,
    pub updated_at: DateTime<Utc>,
}

/// The Person a text's number belongs to. Display only: a recorded phone is
/// an address, never proof of who is texting, so it grants nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContactLink {
    pub person_id: Uuid,
    pub name: String,
    /// The Person's role, lowercased (`client`, `lawyer`, ...).
    pub role: String,
}

impl ContactLink {
    pub(crate) fn of(person: &persons::Person) -> Self {
        Self {
            person_id: person.id,
            name: person.name.clone(),
            role: person.role.as_str().to_string(),
        }
    }
}

/// The Person an inbound conversation's sender resolved to at ingest — the
/// association persisted on the conversation (`conversation.person_id`),
/// surfaced display-only. A resolved identity is provenance, not authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonBadge {
    pub person_id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectChoice {
    pub id: Uuid,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntakeDetail {
    pub contact: Option<ContactLink>,
    pub conversation: Conversation,
    pub messages: Vec<Message>,
    pub bodies_visible: bool,
    pub can_social_reply: bool,
    pub can_sms_reply: bool,
    pub projects: Vec<ProjectChoice>,
    pub next_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadView {
    pub conversation: Conversation,
    pub messages: Vec<Message>,
    pub assignee: String,
    pub delivery: String,
    pub next_action: String,
    pub can_note: bool,
    pub can_send: bool,
    pub can_attribute: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("database: {0}")]
    Db(String),
    #[error("not allowed")]
    Denied,
    #[error("conversation was not found")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    #[error("that project is in another firm")]
    CrossFirm,
    #[error("empty message")]
    Empty,
    #[error("promotional send is opted out")]
    OptedOut,
    /// The import entry point requires the postgres Inbox authority —
    /// imports are postgres-native (LAW-238 D1) and the legacy Surreal path
    /// has no direction-aware writer.
    #[error("inbox import requires the PostgreSQL Inbox authority (set NAVIGATOR_POSTGRES_INBOX_AUTHORITY=1)")]
    PostgresAuthorityRequired,
    #[error("{0}")]
    Contact(String),
    #[error("unknown inbox route: {0}")]
    InvalidRoute(String),
}

impl From<surrealdb::Error> for InboxError {
    fn from(error: surrealdb::Error) -> Self {
        Self::Db(error.to_string())
    }
}

impl From<persons::PersonError> for InboxError {
    fn from(error: persons::PersonError) -> Self {
        Self::Db(error.to_string())
    }
}

impl From<communications::CommunicationError> for InboxError {
    fn from(error: communications::CommunicationError) -> Self {
        Self::Db(error.to_string())
    }
}

impl From<crate::email_conversations::EmailConversationError> for InboxError {
    fn from(error: crate::email_conversations::EmailConversationError) -> Self {
        Self::Db(error.to_string())
    }
}

impl From<ProjectStoreError> for InboxError {
    fn from(error: ProjectStoreError) -> Self {
        Self::Db(error.to_string())
    }
}

/// The postgres pool when the deployment has cut the Inbox slice over.
/// Configuration errors fail closed rather than silently keeping Surreal.
pub(crate) async fn inbox_authority() -> Result<Option<crate::postgres::PostgresDb>, InboxError> {
    crate::postgres::inbox::authority()
        .await
        .map_err(|error| InboxError::Db(error.to_string()))
}

#[derive(SurrealValue)]
#[allow(clippy::struct_excessive_bools)] // Mirrors Conversation's independent flags.
struct ConversationRow {
    id: surrealdb::types::RecordId,
    firm_id: surrealdb::types::RecordId,
    brand_key: Option<String>,
    interaction_type: String,
    provider_post_id: Option<String>,
    project_id: Option<surrealdb::types::RecordId>,
    channel: String,
    external_address: String,
    external_name: Option<String>,
    subject: Option<String>,
    unread: bool,
    provider_account: String,
    provider_thread_ref: Option<String>,
    source_page: Option<String>,
    source_channel: Option<String>,
    matter_consent: bool,
    promotional_opt_in: bool,
    promotional_opt_out: bool,
    email_conversation_id: Option<surrealdb::types::RecordId>,
    person_id: Option<surrealdb::types::RecordId>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(SurrealValue)]
struct MessageRow {
    id: surrealdb::types::RecordId,
    conversation_id: surrealdb::types::RecordId,
    parent_message_id: Option<surrealdb::types::RecordId>,
    visibility: Option<String>,
    direction: String,
    body: String,
    author_person_id: Option<surrealdb::types::RecordId>,
    provider_message_id: Option<String>,
    provider_ref: Option<String>,
    delivery_state: String,
    purpose: String,
    attachment_asset_id: Option<surrealdb::types::RecordId>,
    raw_storage_key: Option<String>,
    communication_id: Option<surrealdb::types::RecordId>,
    occurred_at: String,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(SurrealValue)]
struct AssignmentRow {
    id: surrealdb::types::RecordId,
    conversation_id: surrealdb::types::RecordId,
    firm_id: surrealdb::types::RecordId,
    from_project_id: Option<surrealdb::types::RecordId>,
    to_project_id: surrealdb::types::RecordId,
    actor_person_id: surrealdb::types::RecordId,
    inserted_at: surrealdb::types::Datetime,
}

#[derive(SurrealValue)]
struct IngressRow {
    message_id: surrealdb::types::RecordId,
}

#[derive(SurrealValue)]
struct ProjectPick {
    id: surrealdb::types::RecordId,
    code: String,
    name: String,
}

const CONVERSATION_SELECT: &str = "id, firm_id, brand_key, interaction_type, provider_post_id, project_id, channel, external_address, \
    external_name, subject, unread, provider_account, provider_thread_ref, source_page, \
    source_channel, matter_consent, promotional_opt_in, promotional_opt_out, \
    email_conversation_id, person_id, inserted_at, updated_at";

const MESSAGE_SELECT: &str =
    "id, conversation_id, parent_message_id, visibility, direction, body, author_person_id, \
    provider_message_id, provider_ref, delivery_state, purpose, attachment_asset_id, \
    raw_storage_key, communication_id, occurred_at, inserted_at, updated_at";

const ASSIGNMENT_SELECT: &str = "id, conversation_id, firm_id, from_project_id, to_project_id, \
    actor_person_id, inserted_at";

impl ConversationRow {
    fn into_conversation(self) -> Option<Conversation> {
        Some(Conversation {
            id: record_uuid(&self.id)?,
            firm_id: record_uuid(&self.firm_id)?,
            brand_key: self.brand_key,
            interaction_type: self.interaction_type,
            provider_post_id: self.provider_post_id,
            project_id: self.project_id.as_ref().and_then(record_uuid),
            channel: self.channel,
            external_address: self.external_address,
            external_name: self.external_name,
            subject: self.subject,
            unread: self.unread,
            provider_account: self.provider_account,
            provider_thread_ref: self.provider_thread_ref,
            source_page: self.source_page,
            source_channel: self.source_channel,
            matter_consent: self.matter_consent,
            promotional_opt_in: self.promotional_opt_in,
            promotional_opt_out: self.promotional_opt_out,
            email_conversation_id: self.email_conversation_id.as_ref().and_then(record_uuid),
            person_id: self.person_id.as_ref().and_then(record_uuid),
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

impl MessageRow {
    fn into_message(self) -> Option<Message> {
        Some(Message {
            id: record_uuid(&self.id)?,
            conversation_id: record_uuid(&self.conversation_id)?,
            parent_message_id: self.parent_message_id.as_ref().and_then(record_uuid),
            visibility: self.visibility,
            direction: self.direction,
            body: self.body,
            author_person_id: self.author_person_id.as_ref().and_then(record_uuid),
            provider_message_id: self.provider_message_id,
            provider_ref: self.provider_ref,
            delivery_state: self.delivery_state,
            purpose: self.purpose,
            attachment_asset_id: self.attachment_asset_id.as_ref().and_then(record_uuid),
            raw_storage_key: self.raw_storage_key,
            communication_id: self.communication_id.as_ref().and_then(record_uuid),
            occurred_at: self.occurred_at,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

impl AssignmentRow {
    fn into_assignment(self) -> Option<Assignment> {
        Some(Assignment {
            id: record_uuid(&self.id)?,
            conversation_id: record_uuid(&self.conversation_id)?,
            firm_id: record_uuid(&self.firm_id)?,
            from_project_id: self.from_project_id.as_ref().and_then(record_uuid),
            to_project_id: record_uuid(&self.to_project_id)?,
            actor_person_id: record_uuid(&self.actor_person_id)?,
            inserted_at: self.inserted_at.into(),
        })
    }
}

pub(crate) fn allows_external_reply(conversation: &Conversation) -> bool {
    conversation.channel != channel::VOICE
        && !crate::fi_import::is_account(&conversation.provider_account)
}

/// The label the inbox shows for what the viewer should do next.
#[must_use]
pub fn next_action(
    role: Role,
    assigned: bool,
    delivery_state: &str,
    can_send: bool,
) -> &'static str {
    if role == Role::Owner && !assigned {
        return "Assign to a project";
    }
    if role == Role::Owner {
        return "Reassign";
    }
    if can_send && delivery_state == delivery::UNKNOWN {
        return "Reconcile delivery";
    }
    if can_send {
        return "Send a reply";
    }
    if matches!(role, Role::Lawyer | Role::Clerk) {
        return "Leave an internal note";
    }
    "Review"
}

/// Ingest clean provider attachment bytes after the receiving Firm is resolved.
/// A bare Asset UUID or supplied hash cannot mint Firm staging authority.
pub async fn ingest_attachment(
    db: &SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    firm_id: Uuid,
    project_id: Option<Uuid>,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<IngestAttachment, InboxError> {
    let pg = inbox_authority().await?;
    ingest_attachment_on(
        pg.as_ref(),
        db,
        storage,
        firm_id,
        project_id,
        filename,
        content_type,
        bytes,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn ingest_attachment_on(
    pg: Option<&crate::postgres::PostgresDb>,
    db: &SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    firm_id: Uuid,
    project_id: Option<Uuid>,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<IngestAttachment, InboxError> {
    let sha256 = crate::assets::sha256_hex(bytes);
    let asset_id = if let Some(project_id) = project_id {
        let project = projects::find_by_id(db, project_id)
            .await?
            .ok_or(InboxError::NotFound)?;
        if project.firm_id != Some(firm_id) {
            return Err(InboxError::CrossFirm);
        }
        crate::documents::ingest_bytes(
            db,
            storage,
            &crate::documents::IngestArgs {
                project_id,
                source: crate::documents::source::EMAIL,
                filename,
                kind: "unclassified",
                content_type,
                description: None,
                secondary_storage_key: None,
                visibility: "internal",
            },
            bytes,
        )
        .await
        .map_err(|error| InboxError::Db(error.to_string()))?
        .asset_id
    } else {
        crate::assets::ingest_content(db, storage, bytes, content_type)
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?
    };
    if let Some(pg) = pg {
        // Required projections and receipt must succeed before native Message admission.
        crate::postgres_mirror::mirror(db, pg, &MirrorKey::Firm(firm_id))
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
        crate::postgres_mirror::mirror(db, pg, &MirrorKey::Asset(asset_id))
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
        if project_id.is_none() {
            crate::postgres::inbox::record_attachment_receipt_on(pg, firm_id, asset_id, &sha256)
                .await?;
        }
    }
    Ok(IngestAttachment {
        asset_id,
        filename: filename.into(),
        sha256,
    })
}

/// Rebuild the deduped result for an already-ingested provider id: the
/// caller receives the original conversation/message pair, marked `deduped`.
async fn replay_existing_message(
    db: &SurrealDb,
    message_id: Uuid,
) -> Result<IngestedInbound, InboxError> {
    let Some(message) = message_by_id(db, message_id).await? else {
        return Err(InboxError::NotFound);
    };
    Ok(IngestedInbound {
        conversation_id: message.conversation_id,
        message_id: message.id,
        deduped: true,
        person_linked: false,
    })
}

/// Ingest one provider delivery. A repeated webhook with the same Firm,
/// channel, account, and provider message id returns the original row.
///
/// # Errors
///
/// Propagates any database error.
pub async fn ingest_inbound(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
) -> Result<IngestedInbound, InboxError> {
    Box::pin(ingest_inbound_in(db, inbound, None)).await
}

/// Ingest one provider email into the Conversation its thread already
/// occupies. Before the per-sender lookup [`ingest_inbound`] runs, the
/// delivery joins an existing Conversation of the same Firm, channel, and
/// Project attribution when either
///
/// - `inbound.provider_thread_ref` (a provider thread id such as Gmail's
///   `X-GM-THRID`) is that Conversation's thread, or
/// - one of `references` — RFC 5322 Message-IDs from `In-Reply-To` and
///   `References`, nearest ancestor first, without angle brackets — is the
///   `provider_message_id` of a Message already in it.
///
/// So an inbound message and the Firm's reply to it land in one
/// Conversation whatever their senders. A Conversation with a different
/// Project attribution is never joined. Dedup on the provider message id
/// runs first, unchanged.
///
/// # Errors
///
/// Propagates any database error.
pub async fn ingest_inbound_threaded(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
    references: &[String],
) -> Result<IngestedInbound, InboxError> {
    Box::pin(ingest_inbound_in(db, inbound, Some(references))).await
}

/// `thread` is `None` for the per-sender threading of [`ingest_inbound`]
/// and the reference chain for [`ingest_inbound_threaded`].
async fn ingest_inbound_in(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
    thread: Option<&[String]>,
) -> Result<IngestedInbound, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        for attachment in &inbound.attachments {
            Box::pin(crate::postgres_mirror::mirror(
                db,
                &pg,
                &MirrorKey::Asset(attachment.asset_id),
            ))
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
        }
        return Box::pin(crate::postgres::inbox::ingest_threaded_on(
            &pg, db, inbound, thread,
        ))
        .await;
    }
    Box::pin(ingest_inbound_surreal(db, inbound, thread)).await
}

/// The Conversation the nearest referenced Message lives in: `references`
/// is ordered nearest ancestor first, and `hits` pairs each referenced
/// provider message id found in scope with its Conversation.
pub(crate) fn nearest_referenced_conversation(
    references: &[String],
    hits: &[(String, Uuid)],
) -> Option<Uuid> {
    references.iter().find_map(|reference| {
        hits.iter()
            .find(|(provider_message_id, _)| provider_message_id == reference)
            .map(|(_, conversation_id)| *conversation_id)
    })
}

/// The direction-aware ingest entry for imported correspondence (LAW-238
/// D1): lawyer-sent items arrive as `outbound` with the mailbox owner as
/// author, received mail as `inbound`. Dedup, threading, and projection are
/// the same machinery [`ingest_inbound`] runs.
///
/// Imports are postgres-native, so this entry requires the postgres Inbox
/// authority — the legacy Surreal store has no direction-aware writer, and
/// writing one would be new Surreal-bound code, which the removal forbids.
///
/// # Errors
///
/// Propagates any database error, and
/// [`InboxError::PostgresAuthorityRequired`] when the deployment has not cut
/// the Inbox slice over to postgres.
pub async fn ingest_imported(
    direction_value: &str,
    author_person_id: Option<Uuid>,
    inbound: &IngestInbound<'_>,
) -> Result<IngestedInbound, InboxError> {
    let Some(pg) = inbox_authority().await? else {
        return Err(InboxError::PostgresAuthorityRequired);
    };
    crate::postgres::inbox::ingest_message_on(&pg, direction_value, author_person_id, None, inbound)
        .await
}

/// Point an imported Message at its first filed attachment Asset (LAW-238
/// D1) — the postgres-authoritative counterpart of
/// [`crate::postgres::inbox::set_attachment_asset_on`]. No-op when the slot
/// is already set.
///
/// # Errors
///
/// Propagates any database error, and
/// [`InboxError::PostgresAuthorityRequired`] when the deployment has not cut
/// the Inbox slice over to postgres.
pub async fn set_attachment_asset(message_id: Uuid, asset_id: Uuid) -> Result<(), InboxError> {
    let Some(pg) = inbox_authority().await? else {
        return Err(InboxError::PostgresAuthorityRequired);
    };
    crate::postgres::inbox::set_attachment_asset_on(&pg, message_id, asset_id).await
}

/// The guard every email association runs through: a lawyer- or clerk-tier
/// resolved Person is that firm member's outgoing mail and never the
/// counterparty of inbound correspondence. The portal's lawyer-mailbox
/// guard refuses such mail before ingest; this is the store-level backstop
/// for the doors that bypass it (CLI ingest, imports).
pub(crate) fn counterparty_decision(person: &persons::Person) -> Option<Uuid> {
    if person.role.is_lawyer_tier() || person.role.is_clerk() {
        None
    } else {
        Some(person.id)
    }
}

/// The Relationship Log action an inbound association records: identity
/// provenance on the lawyer-acknowledged feed — automation never writes
/// typed `relationship` or `entity_role` edges from correspondence.
pub(crate) const INBOUND_ASSOCIATION_ACTION: &str = "inbound_email_associated";

/// The Person name an auto-created sender takes: the RFC From display name
/// when present, the address itself otherwise.
pub(crate) fn sender_display_name<'a>(display: Option<&'a str>, address: &'a str) -> &'a str {
    display
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| address.trim())
}

/// The inbound email sender's Person on the legacy Surreal lane: primary
/// or `person_mailbox` alias match (`find_by_any_email`) — never an
/// address-book claim, and never a lawyer- or clerk-tier sender. Only the
/// exact, case-insensitive address resolves; near matches do not. A sender
/// no Person claims becomes a new client-tier Person: the CRM grows from
/// correspondence, and a duplicate collapses later through `person_merge`.
async fn inbound_counterparty(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
) -> Result<Option<Uuid>, InboxError> {
    resolve_or_create_sender(db, inbound.external_address, inbound.external_name).await
}

/// The shared resolve-or-create behind ingest and the backfill: primary or
/// `person_mailbox` alias match (`find_by_any_email`) — never an
/// address-book claim, and never a lawyer- or clerk-tier sender. Only the
/// exact, case-insensitive address resolves; near matches do not. A sender
/// no Person claims becomes a new client-tier Person: the CRM grows from
/// correspondence, and a duplicate collapses later through `person_merge`.
pub(crate) async fn resolve_or_create_sender(
    db: &SurrealDb,
    external_address: &str,
    display_name: Option<&str>,
) -> Result<Option<Uuid>, InboxError> {
    let address = external_address.trim();
    let resolved = persons::find_by_any_email(db, address).await?;
    if let Some(person) = resolved {
        // A known lawyer- or clerk-tier sender is outgoing mail: decision
        // None, and certainly no second Person for the same address.
        return Ok(counterparty_decision(&person));
    }
    let created = persons::create(
        db,
        &persons::NewPerson::new(sender_display_name(display_name, address), address),
    )
    .await
    .map_err(|error| InboxError::Db(error.to_string()))?;
    Ok(Some(created.id))
}

/// Appends the Relationship Log entry for a conversation's first Person
/// association — advisory, post-commit, best-effort: a failed entry warns
/// and never fails the ingest, and an identical replay entry is skipped
/// (the `exists` dedup the seed import uses). `person_id` is None when no
/// link was newly made (guard rejection, non-email channel, replay).
pub(crate) async fn record_association_log(
    db: &SurrealDb,
    conversation_id: Uuid,
    person_id: Option<Uuid>,
    channel: &str,
    address: &str,
) {
    let Some(person_id) = person_id else {
        return;
    };
    let entry = crate::relationship_logs::NewRelationshipLog {
        actor_person_id: None,
        subject_type: crate::relationship_logs::PERSON_SUBJECT.to_string(),
        subject_id: person_id,
        action: INBOUND_ASSOCIATION_ACTION.to_string(),
        detail: format!("inbox_conversation:{conversation_id} {channel} sender {address}"),
        project_id: None,
    };
    match crate::relationship_logs::exists(db, &entry).await {
        Ok(true) => {}
        Ok(false) => {
            if let Err(error) = crate::relationship_logs::record(db, &entry).await {
                tracing::warn!(
                    %error,
                    %conversation_id,
                    "relationship log entry for the inbox association stalled"
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                %conversation_id,
                "relationship log dedup check stalled"
            );
        }
    }
}

/// The backfill reads only what identity needs: the conversation, the
/// address that owns it, and the display name the first message recorded.
#[derive(SurrealValue)]
struct UnassociatedConversation {
    id: surrealdb::types::RecordId,
    external_address: String,
    external_name: Option<String>,
}

/// Links still-unassociated email conversations to the Person the address
/// resolves to today — the same resolve-or-create the ingest uses — and
/// reports how many conversations gained a link.
///
/// Idempotent: only rows with `person_id IS NONE` are selected, so a
/// re-run finds nothing and writes nothing. Associations the backfill
/// makes behave like ingest associations: guarded against lawyer- and
/// clerk-tier senders, logged once on the Relationship Log, never edges.
///
/// # Errors
///
/// Propagates any database error, including the fail-closed authority
/// probe error.
pub async fn backfill_person_links(db: &SurrealDb) -> Result<u64, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::backfill_person_links_on(&pg, db)).await;
    }
    backfill_person_links_surreal(db).await
}

/// The legacy SurrealDB backfill behind [`backfill_person_links`].
async fn backfill_person_links_surreal(db: &SurrealDb) -> Result<u64, InboxError> {
    let mut response = db
        .query(format!(
            "SELECT id, external_address, external_name FROM {CONVERSATION} \
             WHERE channel = $channel AND person_id IS NONE"
        ))
        .bind(("channel", channel::EMAIL.to_string()))
        .await?
        .check()?;
    let unassociated: Vec<UnassociatedConversation> = response.take(0)?;
    let mut linked = 0u64;
    for row in unassociated {
        let Some(conversation_id) = record_uuid(&row.id) else {
            continue;
        };
        let Some(person_id) =
            resolve_or_create_sender(db, &row.external_address, row.external_name.as_deref())
                .await?
        else {
            continue;
        };
        db.query("UPDATE $id SET person_id = $person, updated_at = time::now()")
            .bind(("id", record_id(CONVERSATION, conversation_id)))
            .bind(("person", record_id("person", person_id)))
            .await?
            .check()?;
        linked += 1;
        record_association_log(
            db,
            conversation_id,
            Some(person_id),
            channel::EMAIL,
            &row.external_address,
        )
        .await;
        after_write(db, MirrorKey::InboxConversation(conversation_id)).await;
    }
    Ok(linked)
}

/// Resolves the counterparty Person, joins the thread the delivery answers
/// (`threaded`, from the reference chain) or the sender's open thread, or
/// creates a Conversation, and persists the first association — the identity
/// core of inbound ingest, shared by every lane that ingests from a live
/// counterparty. Returns the conversation, whether this ingest newly linked
/// it, and the resolved Person.
async fn associate_counterparty(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
    threaded: Option<Conversation>,
) -> Result<(Conversation, bool, Option<Uuid>), InboxError> {
    let sender_person_id = match inbound.channel {
        channel::EMAIL => inbound_counterparty(db, inbound).await?,
        _ => None,
    };
    let existing = match threaded {
        Some(existing) => Some(existing),
        None => find_open_thread(db, inbound).await?,
    };
    let (conversation, person_linked) = if let Some(existing) = existing {
        let linked = sender_person_id.is_some() && existing.person_id.is_none();
        (existing, linked)
    } else {
        let created = create_conversation(db, inbound, sender_person_id).await?;
        (created, sender_person_id.is_some())
    };
    if let (true, Some(person_id)) = (person_linked, sender_person_id) {
        // First association wins: an existing link is never overwritten,
        // and the WHERE guard keeps a concurrent ingest from double-writing.
        db.query(
            "UPDATE $id SET person_id = $person, updated_at = time::now() \
             WHERE person_id IS NONE",
        )
        .bind(("id", record_id(CONVERSATION, conversation.id)))
        .bind(("person", record_id("person", person_id)))
        .await?
        .check()?;
        after_write(db, MirrorKey::InboxConversation(conversation.id)).await;
    }
    Ok((conversation, person_linked, sender_person_id))
}

/// The legacy SurrealDB store behind [`ingest_inbound`] — the webhook-shaped
/// ingest path retained for deployments that have not cut the Inbox slice
/// over to PostgreSQL.
async fn ingest_inbound_surreal(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
    thread: Option<&[String]>,
) -> Result<IngestedInbound, InboxError> {
    let channel_supported = matches!(
        inbound.channel,
        channel::EMAIL | channel::SMS | channel::SCRAPER | channel::VOICE
    ) || is_social_channel(inbound.channel);
    if !channel_supported
        || !matches!(
            inbound.interaction_type.unwrap_or("message"),
            "message" | "comment" | "direct_message"
        )
    {
        return Err(InboxError::Denied);
    }
    if let Some(existing) = find_ingress(
        db,
        inbound.firm_id,
        inbound.channel,
        inbound.provider_account,
        inbound.provider_message_id,
    )
    .await?
    {
        return replay_existing_message(db, existing).await;
    }

    let threaded = match thread {
        Some(references) => find_referenced_thread(db, inbound, references).await?,
        None => None,
    };
    let (conversation, person_linked, sender_person_id) =
        associate_counterparty(db, inbound, threaded).await?;
    if inbound.email_conversation_id.is_some() && conversation.email_conversation_id.is_none() {
        link_email_conversation(db, conversation.id, inbound.email_conversation_id).await?;
    }
    let message_id = create_message(
        db,
        conversation.id,
        direction::INBOUND,
        inbound.body,
        None,
        Some(inbound.provider_message_id),
        None,
        delivery::RECEIVED,
        purpose::MATTER,
        inbound.visibility,
        inbound
            .attachments
            .first()
            .map(|attachment| attachment.asset_id),
        inbound.occurred_at,
        inbound.raw_storage_key,
    )
    .await?;
    link_message_assets(db, inbound.firm_id, message_id, &inbound.attachments).await?;
    if let Some(parent_provider_id) = inbound.parent_provider_message_id {
        let parent = parent_message_id(db, conversation.id, parent_provider_id).await?;
        if let Some(parent_id) = parent {
            db.query("UPDATE $id SET parent_message_id = $parent, updated_at = time::now()")
                .bind(("id", record_id(MESSAGE, message_id)))
                .bind(("parent", record_id(MESSAGE, parent_id)))
                .await?
                .check()?;
            after_write(db, MirrorKey::InboxMessage(message_id)).await;
        }
    }
    if let Err(error) = create_ingress(db, inbound, message_id).await {
        return recover_deduped_ingress(db, inbound, message_id, error).await;
    }
    let _ = db
        .query("UPDATE $id SET unread = true, updated_at = time::now()")
        .bind(("id", record_id(CONVERSATION, conversation.id)))
        .await;
    after_write(db, MirrorKey::InboxConversation(conversation.id)).await;
    let ingested = IngestedInbound {
        conversation_id: conversation.id,
        message_id,
        deduped: false,
        person_linked,
    };
    record_association_log(
        db,
        ingested.conversation_id,
        ingested.person_linked.then_some(sender_person_id).flatten(),
        inbound.channel,
        inbound.external_address,
    )
    .await;
    Ok(ingested)
}

/// The attachments linked to one Inbox Message, in provider order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessageAttachment {
    pub asset_id: Uuid,
    pub filename: String,
    pub position: i64,
    pub sha256: String,
}

#[derive(SurrealValue)]
struct MessageAssetRow {
    asset_id: surrealdb::types::RecordId,
    filename: String,
    position: i64,
    sha256: String,
}

impl MessageAssetRow {
    fn into_attachment(self) -> Option<MessageAttachment> {
        Some(MessageAttachment {
            asset_id: record_uuid(&self.asset_id)?,
            filename: self.filename,
            position: self.position,
            sha256: self.sha256,
        })
    }
}

/// Link a Message to its attachment Assets in provider order. Re-linking
/// the same message and sha reuses the row, so a retried ingest cannot
/// duplicate a link.
///
/// # Errors
///
/// Propagates database and Inbox authority errors.
pub async fn link_message_assets(
    db: &SurrealDb,
    firm_id: Uuid,
    message_id: Uuid,
    attachments: &[IngestAttachment],
) -> Result<(), InboxError> {
    if let Some(pg) = inbox_authority().await? {
        for attachment in attachments {
            crate::postgres_mirror::mirror(db, &pg, &MirrorKey::Asset(attachment.asset_id))
                .await
                .map_err(|error| InboxError::Db(error.to_string()))?;
        }
        return crate::postgres::inbox::link_message_assets_on(
            &pg,
            firm_id,
            message_id,
            attachments,
        )
        .await;
    }
    for (position, attachment) in attachments.iter().enumerate() {
        let position = i64::try_from(position).unwrap_or(i64::MAX);
        let mut response = db
            .query(format!(
                "SELECT VALUE id FROM {MESSAGE_ASSET} \
                 WHERE message_id = $message AND sha256 = $sha LIMIT 1"
            ))
            .bind(("message", record_id(MESSAGE, message_id)))
            .bind(("sha", attachment.sha256.clone()))
            .await?
            .check()?;
        let existing: Vec<surrealdb::types::RecordId> = response.take(0)?;
        if !existing.is_empty() {
            continue;
        }
        db.query(
            "CREATE $id SET firm_id = $firm, message_id = $message, \
             asset_id = $asset, filename = $filename, position = $position, sha256 = $sha",
        )
        .bind(("id", record_id(MESSAGE_ASSET, Uuid::now_v7())))
        .bind(("firm", record_id(crate::firms::TABLE, firm_id)))
        .bind(("message", record_id(MESSAGE, message_id)))
        .bind((
            "asset",
            record_id(crate::assets::TABLE, attachment.asset_id),
        ))
        .bind(("filename", attachment.filename.clone()))
        .bind(("position", position))
        .bind(("sha", attachment.sha256.clone()))
        .await?
        .check()?;
    }
    Ok(())
}

/// The attachments linked to one Inbox Message, in provider order. Firm
/// scoping rides on the link row: a caller outside the message's Firm
/// sees nothing.
///
/// # Errors
///
/// Propagates any database error.
pub async fn message_assets(
    db: &SurrealDb,
    firm_id: Uuid,
    message_id: Uuid,
) -> Result<Vec<MessageAttachment>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return crate::postgres::inbox::message_assets_on(&pg, firm_id, message_id).await;
    }
    let mut response = db
        .query(format!(
            "SELECT asset_id, filename, position, sha256 FROM {MESSAGE_ASSET} \
             WHERE firm_id = $firm AND message_id = $message ORDER BY position"
        ))
        .bind(("firm", record_id(crate::firms::TABLE, firm_id)))
        .bind(("message", record_id(MESSAGE, message_id)))
        .await?
        .check()?;
    let rows: Vec<MessageAssetRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(MessageAssetRow::into_attachment)
        .collect())
}

/// A lost ingress-creation race: another worker filed the same provider
/// message between our dedup check and our ingress write. Roll our message
/// back and surface the winner's row instead of failing the ingest.
async fn recover_deduped_ingress(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
    message_id: Uuid,
    error: InboxError,
) -> Result<IngestedInbound, InboxError> {
    let _ = db
        .query("DELETE $id")
        .bind(("id", record_id(MESSAGE, message_id)))
        .await;
    after_write(db, MirrorKey::InboxMessage(message_id)).await;
    if let Some(existing) = find_ingress(
        db,
        inbound.firm_id,
        inbound.channel,
        inbound.provider_account,
        inbound.provider_message_id,
    )
    .await?
    {
        let Some(message) = message_by_id(db, existing).await? else {
            return Err(InboxError::NotFound);
        };
        return Ok(IngestedInbound {
            conversation_id: message.conversation_id,
            message_id: message.id,
            deduped: true,
            person_linked: false,
        });
    }
    Err(error)
}

/// Check the authoritative Person and participation before an Inbox assignment.
/// Owners may assign across Projects; Lawyers and Clerks require participation.
///
/// # Errors
/// Returns an error when the Person or participation cannot be read.
pub async fn can_assign_project(
    db: &SurrealDb,
    actor_id: Uuid,
    project_id: Uuid,
) -> Result<bool, InboxError> {
    let actor = load_person(db, actor_id).await?;
    match actor.role {
        Role::Owner => Ok(true),
        Role::Lawyer | Role::Clerk => sees_project(db, actor_id, actor.role, project_id).await,
        Role::Admin | Role::Client => Ok(false),
    }
}

/// Attribute or reassign a conversation to a Project in the same Firm.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor is not an Owner or a participating Lawyer/Clerk.
/// [`InboxError::CrossFirm`] when the Project belongs to another Firm.
/// [`InboxError::NotFound`] when the conversation or Project is missing.
/// Voice billing and read-only Fi SMS history leave the Firm queue once.
/// Repeated assignment to that Project repairs a checkpoint gap without
/// allowing a concurrent import to move the history to another Project.
pub async fn assign(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.channel == channel::VOICE
        || (conversation.channel == channel::SMS
            && crate::fi_import::is_account(&conversation.provider_account))
    {
        let pg = crate::postgres::PostgresDb::from_env()
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
        return Box::pin(crate::billed_calls::assign_conversation(
            &pg,
            db,
            actor_id,
            conversation_id,
            project_id,
        ))
        .await;
    }
    assign_unbilled(db, actor_id, conversation_id, project_id).await
}

/// Require ordinary Inbox assignment rights using the authoritative Person and Project.
/// Owner may assign; a Lawyer or Clerk needs target Project participation.
/// The target Project must belong to the Conversation or import's resolved Firm.
pub async fn require_assignment_authorization(
    db: &SurrealDb,
    actor_id: Uuid,
    firm_id: Uuid,
    project_id: Uuid,
) -> Result<(), InboxError> {
    let project = projects::find_by_id(db, project_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if project.firm_id != Some(firm_id) {
        return Err(InboxError::CrossFirm);
    }
    if !can_assign_project(db, actor_id, project_id).await? {
        return Err(InboxError::Denied);
    }
    Ok(())
}

pub(crate) async fn assign_unbilled(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::assign_on(
            &pg,
            db,
            actor_id,
            conversation_id,
            project_id,
        ))
        .await;
    }
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    require_assignment_authorization(db, actor_id, conversation.firm_id, project_id).await?;
    assign_write(db, actor_id, &conversation, project_id).await
}

/// The triage review queue's assignment door. Owner may act across Firms;
/// Admin requires membership in the target Firm. A Lawyer or Clerk requires
/// participation on the Project the suggestion or correction names.
///
/// # Errors
///
/// [`InboxError::Denied`] for a Client, an actor outside the Firm, or a
/// Lawyer/Clerk without active participation on the Project. The other
/// cases of [`assign`] apply unchanged.
pub(crate) async fn assign_for_triage_review(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.channel == channel::VOICE
        || (conversation.channel == channel::SMS
            && crate::fi_import::is_account(&conversation.provider_account))
    {
        let pg = crate::postgres::PostgresDb::from_env()
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
        return Box::pin(crate::billed_calls::assign_conversation_for_triage_review(
            &pg,
            db,
            actor_id,
            conversation_id,
            project_id,
        ))
        .await;
    }
    Box::pin(assign_for_triage_review_unbilled(
        db,
        actor_id,
        conversation_id,
        project_id,
    ))
    .await
}

pub(crate) async fn assign_for_triage_review_unbilled(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::assign_for_triage_review_on(
            &pg,
            db,
            actor_id,
            conversation_id,
            project_id,
        ))
        .await;
    }
    let actor = load_person(db, actor_id).await?;
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let project = projects::find_by_id(db, project_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if project.firm_id != Some(conversation.firm_id) {
        return Err(InboxError::CrossFirm);
    }
    if !triage_reviewer_may_assign(db, &actor, project_id).await? {
        return Err(InboxError::Denied);
    }
    if matches!(actor.role, Role::Lawyer | Role::Clerk) {
        if let Some(source_project) = conversation.project_id {
            if !triage_reviewer_may_assign(db, &actor, source_project).await? {
                return Err(InboxError::Denied);
            }
        }
    }
    assign_write(db, actor_id, &conversation, project_id).await
}

/// Whether the actor may act on a triage row whose human decision assigns
/// `project_id`. Owner always, Admin within their Firm; a Lawyer or Clerk
/// passes the participation check the reply doors use; a Client never.
pub(crate) async fn triage_reviewer_may_assign(
    db: &SurrealDb,
    actor: &persons::Person,
    project_id: Uuid,
) -> Result<bool, InboxError> {
    match actor.role {
        Role::Owner => Ok(true),
        Role::Admin => {
            let Some(project) = projects::find_by_id(db, project_id).await? else {
                return Ok(false);
            };
            match project.firm_id {
                Some(firm_id) => triage_reviewer_may_resolve(db, actor, firm_id).await,
                None => Ok(false),
            }
        }
        Role::Lawyer | Role::Clerk => sees_project(db, actor.id, actor.role, project_id).await,
        Role::Client => Ok(false),
    }
}

/// Whether the actor may act on a triage row that names no Project —
/// privilege resolution and re-runs. Owner always; Admin, Lawyer, or
/// Clerk when they belong to the row's Firm (the same visibility the queue
/// list grants); a Client never.
pub(crate) async fn triage_reviewer_may_resolve(
    db: &SurrealDb,
    actor: &persons::Person,
    firm_id: Uuid,
) -> Result<bool, InboxError> {
    match actor.role {
        Role::Owner => Ok(true),
        Role::Admin | Role::Lawyer | Role::Clerk => {
            Ok(crate::firms::memberships_for_person(db, actor.id)
                .await
                .map_err(|error| InboxError::Db(error.to_string()))?
                .iter()
                .any(|membership| membership.firm_id == firm_id))
        }
        Role::Client => Ok(false),
    }
}

/// Authorization-only door for the triage review actions: denies before any
/// row or suggestion state is read, so an unauthorized actor learns nothing
/// from the error it receives.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor fails the review authority,
/// [`InboxError::NotFound`] when the actor does not exist.
pub(crate) async fn require_triage_reviewer(
    db: &SurrealDb,
    actor_id: Uuid,
    project_id: Uuid,
) -> Result<(), InboxError> {
    let actor = load_person(db, actor_id).await?;
    if triage_reviewer_may_assign(db, &actor, project_id).await? {
        Ok(())
    } else {
        Err(InboxError::Denied)
    }
}

/// The shared write behind both assignment doors: re-point the conversation,
/// mirror it, rescope any email thread, and record the assignment row.
async fn assign_write(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation: &Conversation,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    let conversation_id = conversation.id;
    let from_project_id = conversation.project_id;
    if from_project_id == Some(project_id)
        && (conversation.channel == channel::VOICE
            || (conversation.channel == channel::SMS
                && crate::fi_import::is_account(&conversation.provider_account)))
    {
        project_all_messages(db, conversation).await?;
        if let Some(previous) = assignments(db, conversation_id)
            .await?
            .into_iter()
            .rev()
            .find(|assignment| assignment.to_project_id == project_id)
        {
            return Ok(previous);
        }
    }
    db.query("UPDATE $id SET project_id = $project, unread = true, updated_at = time::now()")
        .bind(("id", record_id(CONVERSATION, conversation_id)))
        .bind(("project", record_id(projects::PROJECT_TABLE, project_id)))
        .await?
        .check()?;
    after_write(db, MirrorKey::InboxConversation(conversation_id)).await;
    if let Some(email_id) = conversation.email_conversation_id {
        crate::email_conversations::set_scope(
            db,
            email_id,
            Some(conversation.firm_id),
            Some(project_id),
        )
        .await?;
    }
    let updated = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    project_all_messages(db, &updated).await?;
    let assignment_id = Uuid::now_v7();
    let mut response = db
        .query(format!(
            "CREATE $id SET conversation_id = $conversation, firm_id = $firm, \
             from_project_id = $from_project, to_project_id = $to_project, \
             actor_person_id = $actor, inserted_at = time::now() RETURN {ASSIGNMENT_SELECT}"
        ))
        .bind(("id", record_id(ASSIGNMENT, assignment_id)))
        .bind(("conversation", record_id(CONVERSATION, conversation_id)))
        .bind(("firm", record_id(crate::firms::TABLE, conversation.firm_id)))
        .bind((
            "from_project",
            from_project_id.map(|id| record_id(projects::PROJECT_TABLE, id)),
        ))
        .bind(("to_project", record_id(projects::PROJECT_TABLE, project_id)))
        .bind(("actor", record_id("person", actor_id)))
        .await?
        .check()?;
    let rows: Vec<AssignmentRow> = response.take(0)?;
    after_write(db, MirrorKey::InboxAssignment(assignment_id)).await;
    rows.into_iter()
        .find_map(AssignmentRow::into_assignment)
        .ok_or(InboxError::NotFound)
}

/// Post-operation that attributes an intake message to one Project after it
/// has been received. Only an Owner or a participating Lawyer/Clerk may do so.
pub async fn attribute_to_project(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    assign(db, actor_id, conversation_id, project_id).await
}

/// Participating Lawyer or Clerk leaves a note that never leaves the firm.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor cannot see the assigned Project or
/// is not a Lawyer or Clerk. [`InboxError::Empty`] when the body is blank.
pub async fn post_note(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
) -> Result<Uuid, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::post_note_on(
            &pg,
            db,
            actor_id,
            conversation_id,
            body,
        ))
        .await;
    }
    let body = body.trim();
    if body.is_empty() {
        return Err(InboxError::Empty);
    }
    let actor = load_person(db, actor_id).await?;
    if !matches!(actor.role, Role::Lawyer | Role::Clerk) {
        return Err(InboxError::Denied);
    }
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let Some(project_id) = conversation.project_id else {
        return Err(InboxError::Denied);
    };
    if !sees_project(db, actor_id, actor.role, project_id).await? {
        return Err(InboxError::Denied);
    }
    let message_id = create_message(
        db,
        conversation_id,
        direction::INTERNAL,
        body,
        Some(actor_id),
        None,
        None,
        delivery::RECEIVED,
        purpose::MATTER,
        None,
        None,
        &Utc::now().to_rfc3339(),
        None,
    )
    .await?;
    let message = message_by_id(db, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    project_message(db, &conversation, &message).await?;
    Ok(message_id)
}

/// Queue an external reply. Only a participating Lawyer or Clerk may queue one.
///
/// # Errors
///
/// [`InboxError::Denied`] for every other role, including Owner and Admin,
/// and for a Lawyer or Clerk who does not currently participate.
/// [`InboxError::OptedOut`] when the reply is promotional and the address
/// has opted out.
pub async fn queue_reply(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
    purpose_value: &str,
) -> Result<Uuid, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::queue_reply_on(
            &pg,
            db,
            actor_id,
            conversation_id,
            body,
            purpose_value,
        ))
        .await;
    }
    let body = body.trim();
    if body.is_empty() {
        return Err(InboxError::Empty);
    }
    let actor = load_person(db, actor_id).await?;
    if !matches!(actor.role, Role::Lawyer | Role::Clerk) {
        return Err(InboxError::Denied);
    }
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if !allows_external_reply(&conversation) {
        return Err(InboxError::Denied);
    }
    let Some(project_id) = conversation.project_id else {
        return Err(InboxError::Denied);
    };
    if !sees_project(db, actor_id, actor.role, project_id).await? {
        return Err(InboxError::Denied);
    }
    if purpose_value == purpose::PROMOTIONAL && conversation.promotional_opt_out {
        return Err(InboxError::OptedOut);
    }
    create_message(
        db,
        conversation_id,
        direction::OUTBOUND,
        body,
        Some(actor_id),
        None,
        None,
        delivery::QUEUED,
        purpose_value,
        None,
        None,
        &Utc::now().to_rfc3339(),
        None,
    )
    .await
}

/// Queue a social reply with an explicit public/private audience. The system
/// Owner and the Brand's Firm Admin DRI may manage social engagement;
/// Lawyers and Clerks must participate on the assigned Project. This path is
/// separate from matter email/SMS dispatch authorization.
pub async fn queue_social_reply(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
    visibility: &str,
) -> Result<Uuid, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::queue_social_reply_on(
            &pg,
            db,
            actor_id,
            conversation_id,
            body,
            visibility,
        ))
        .await;
    }
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let capability_supported = match conversation.channel.as_str() {
        channel::FACEBOOK | channel::INSTAGRAM => matches!(visibility, "public" | "private"),
        channel::THREADS | channel::YOUTUBE | channel::TIKTOK => visibility == "public",
        _ => false,
    };
    if !capability_supported
        || (conversation.channel == channel::TIKTOK
            && (conversation.interaction_type != "comment"
                || conversation
                    .provider_post_id
                    .as_deref()
                    .is_none_or(str::is_empty)))
        || (conversation.interaction_type == "direct_message" && visibility != "private")
    {
        return Err(InboxError::Denied);
    }
    let body = body.trim();
    if body.is_empty() {
        return Err(InboxError::Empty);
    }
    authorize_social_reply(db, actor_id, &conversation).await?;
    if visibility == "public"
        && messages_for(db, conversation_id)
            .await?
            .iter()
            .any(|message| {
                message.direction == direction::INBOUND
                    && message.visibility.as_deref() == Some("private")
            })
    {
        return Err(InboxError::Denied);
    }
    let message_id = create_message(
        db,
        conversation_id,
        direction::OUTBOUND,
        body,
        Some(actor_id),
        None,
        None,
        delivery::QUEUED,
        purpose::MATTER,
        Some(visibility),
        None,
        &Utc::now().to_rfc3339(),
        None,
    )
    .await?;
    let message = message_by_id(db, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    project_message(db, &conversation, &message).await?;
    Ok(message_id)
}

/// The `ExternalSystem` a social channel's sender ids live under.
fn social_system(channel_name: &str) -> Option<external_identities::ExternalSystem> {
    match channel_name {
        channel::FACEBOOK => Some(external_identities::ExternalSystem::Facebook),
        channel::INSTAGRAM => Some(external_identities::ExternalSystem::Instagram),
        channel::THREADS => Some(external_identities::ExternalSystem::Threads),
        channel::YOUTUBE => Some(external_identities::ExternalSystem::Youtube),
        channel::TIKTOK => Some(external_identities::ExternalSystem::Tiktok),
        _ => None,
    }
}

/// Who a conversation's sender is — an address-book lookup, display only.
/// Email resolves by the address a Person records, SMS by the phone a
/// Person records, and the social channels through the identity address
/// book keyed on the provider id the inbound webhook recorded — the same
/// value `Conversation.external_address` carries.
///
/// The resolved [`ContactLink`] grants nothing: it never satisfies a
/// dispatch, participation, or privilege check — those read the person's
/// role and participation, never this table.
///
/// # Errors
///
/// Propagates any database error.
pub(crate) async fn sender_identity(
    db: &SurrealDb,
    conversation: &Conversation,
) -> Result<Option<ContactLink>, InboxError> {
    match conversation.channel.as_str() {
        channel::EMAIL => Ok(match conversation.person_id {
            // The association persisted at ingest answers first; the
            // address read below only repairs rows that predate it.
            Some(person_id) => persons::find_by_id(db, person_id)
                .await?
                .as_ref()
                .map(ContactLink::of),
            None => persons::find_by_any_email(db, conversation.external_address.trim())
                .await?
                .as_ref()
                .map(ContactLink::of),
        }),
        phone if has_phone_counterparty(phone) => {
            Ok(persons::find_by_phone(db, &conversation.external_address)
                .await?
                .as_ref()
                .map(ContactLink::of))
        }
        other => match social_system(other) {
            Some(system) => Ok(external_identities::find_person_by_account(
                db,
                system,
                &conversation.external_address,
            )
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?
            .as_ref()
            .map(ContactLink::of)),
            None => Ok(None),
        },
    }
}

/// Record that this text conversation's number is the direct line of the
/// Person with `email`, so later texts show whose they are. The same people
/// who may reply to a text may link it. A number already held by another
/// Person, or a Person who already has a different phone, is refused rather
/// than overwritten.
///
/// # Errors
///
/// [`InboxError::Denied`] for a non-SMS conversation or a missing authority;
/// [`InboxError::Contact`] when no Person has that email, the number belongs
/// to someone else, or the Person already records another phone.
pub async fn link_contact_phone(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    email: &str,
) -> Result<ContactLink, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::link_contact_phone_on(
            &pg,
            db,
            actor_id,
            conversation_id,
            email,
        ))
        .await;
    }
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.channel != channel::SMS {
        return Err(InboxError::Denied);
    }
    authorize_social_reply(db, actor_id, &conversation).await?;
    let person = persons::find_by_email_ci(db, email.trim())
        .await?
        .ok_or_else(|| InboxError::Contact("No person has that email".into()))?;
    if let Some(holder) = persons::find_by_phone(db, &conversation.external_address).await? {
        return if holder.id == person.id {
            Ok(ContactLink::of(&holder))
        } else {
            Err(InboxError::Contact(
                "That number already belongs to another person".into(),
            ))
        };
    }
    if person.phone.is_some() {
        return Err(InboxError::Contact(
            "That person already has a different phone recorded".into(),
        ));
    }
    let updated = persons::set_phone(db, person.id, Some(&conversation.external_address))
        .await?
        .ok_or(InboxError::NotFound)?;
    Ok(ContactLink::of(&updated))
}

/// Maximum length Twilio accepts for one text request.
pub(crate) const SMS_BODY_LIMIT: usize = 1600;

/// A text starts only toward an E.164 number.
pub(crate) fn is_e164(number: &str) -> bool {
    number.strip_prefix('+').is_some_and(|digits| {
        (8..=15).contains(&digits.len())
            && digits.bytes().all(|byte| byte.is_ascii_digit())
            && !digits.starts_with('0')
    })
}

/// Queue a text in an existing SMS conversation from the admin inbox. The
/// Owner and the Firm's Brand-content Admin may reply; a Lawyer or Clerk only
/// while participating on the assigned Project.
///
/// # Errors
///
/// [`InboxError::Denied`] when the conversation is not an SMS thread or the
/// actor lacks authority. [`InboxError::Empty`] for a blank or over-long body.
pub async fn queue_sms_reply(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
) -> Result<Uuid, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::queue_sms_reply_on(
            &pg,
            db,
            actor_id,
            conversation_id,
            body,
        ))
        .await;
    }
    let body = body.trim();
    if body.is_empty() || body.chars().count() > SMS_BODY_LIMIT {
        return Err(InboxError::Empty);
    }
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.channel != channel::SMS {
        return Err(InboxError::Denied);
    }
    authorize_social_reply(db, actor_id, &conversation).await?;
    queue_sms_message(db, &conversation, actor_id, body).await
}

/// Open a text conversation with a number that has not texted the Firm first.
/// The sender attests that the recipient agreed to receive texts, which is
/// recorded as matter consent on the conversation. An existing thread with the
/// same number is reused.
///
/// # Errors
///
/// [`InboxError::Denied`] when the number is not E.164, consent was not
/// attested, or the actor lacks authority. [`InboxError::Empty`] for a blank
/// or over-long body.
#[allow(clippy::too_many_arguments)]
pub async fn open_sms_conversation(
    db: &SurrealDb,
    actor_id: Uuid,
    firm_id: Uuid,
    brand_key: Option<&str>,
    provider_account: &str,
    to: &str,
    body: &str,
    recipient_consented: bool,
) -> Result<(Uuid, Uuid), InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::open_sms_conversation_on(
            &pg,
            db,
            actor_id,
            firm_id,
            brand_key,
            provider_account,
            to,
            body,
            recipient_consented,
        ))
        .await;
    }
    let to = to.trim();
    let body = body.trim();
    if body.is_empty() || body.chars().count() > SMS_BODY_LIMIT {
        return Err(InboxError::Empty);
    }
    if !recipient_consented || !is_e164(to) || to == provider_account {
        return Err(InboxError::Denied);
    }
    if !reply_actor_authorized(db, actor_id, firm_id, None).await? {
        return Err(InboxError::Denied);
    }
    let thread = IngestInbound {
        project_id: None,
        firm_id,
        brand_key,
        interaction_type: Some("message"),
        provider_post_id: None,
        parent_provider_message_id: None,
        visibility: Some("private"),
        channel: channel::SMS,
        provider_account,
        provider_message_id: "",
        provider_thread_ref: None,
        external_address: to,
        external_name: None,
        subject: None,
        body,
        source_page: Some(provider_account),
        source_channel: Some(channel::SMS),
        matter_consent: true,
        promotional_opt_in: false,
        email_conversation_id: None,
        attachments: Vec::new(),
        occurred_at: &Utc::now().to_rfc3339(),
        raw_storage_key: None,
    };
    let conversation = match find_open_thread(db, &thread).await? {
        Some(existing) => existing,
        None => create_conversation(db, &thread, None).await?,
    };
    let message_id = queue_sms_message(db, &conversation, actor_id, body).await?;
    Ok((conversation.id, message_id))
}

async fn queue_sms_message(
    db: &SurrealDb,
    conversation: &Conversation,
    actor_id: Uuid,
    body: &str,
) -> Result<Uuid, InboxError> {
    if crate::fi_import::is_account(&conversation.provider_account) {
        return Err(InboxError::Denied);
    }
    let message_id = create_message(
        db,
        conversation.id,
        direction::OUTBOUND,
        body,
        Some(actor_id),
        None,
        None,
        delivery::QUEUED,
        purpose::MATTER,
        None,
        None,
        &Utc::now().to_rfc3339(),
        None,
    )
    .await?;
    let message = message_by_id(db, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    project_message(db, conversation, &message).await?;
    Ok(message_id)
}

/// Send every queued reply, re-checking Lawyer or Clerk participation first.
///
/// An unknown provider result stays unknown. A later pass does not transmit
/// it again.
///
/// # Errors
///
/// Propagates any database error. A single transport failure is recorded on
/// that message and does not abort the rest of the queue.
pub async fn dispatch_queued(
    db: &SurrealDb,
    transport: &dyn OutboundTransport,
) -> Result<DispatchReport, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::dispatch_queued_on(
            &pg, db, transport,
        ))
        .await;
    }
    dispatch_queued_surreal(db, transport).await
}

/// The legacy SurrealDB store behind [`dispatch_queued`].
async fn dispatch_queued_surreal(
    db: &SurrealDb,
    transport: &dyn OutboundTransport,
) -> Result<DispatchReport, InboxError> {
    let queued = messages_in_state(db, delivery::QUEUED).await?;
    let mut report = DispatchReport::default();
    for message in queued {
        let Some(conversation) = conversation_by_id(db, message.conversation_id).await? else {
            continue;
        };
        if message.purpose == purpose::PROMOTIONAL && conversation.promotional_opt_out {
            set_delivery(db, message.id, delivery::SUPPRESSED, None).await?;
            journal_attempt(
                db,
                message.id,
                delivery::SUPPRESSED,
                None,
                Some("opted out"),
            )
            .await?;
            report.suppressed += 1;
            continue;
        }
        let allowed = queued_author_still_authorized(db, &conversation, &message).await?;
        if !allowed || !allows_external_reply(&conversation) {
            set_delivery(db, message.id, delivery::SUPPRESSED, None).await?;
            journal_attempt(
                db,
                message.id,
                delivery::SUPPRESSED,
                None,
                Some(
                    if crate::fi_import::is_account(&conversation.provider_account) {
                        "import source is read-only"
                    } else if conversation.channel == channel::VOICE {
                        "voice replies are unsupported"
                    } else {
                        "participation revoked"
                    },
                ),
            )
            .await?;
            report.suppressed += 1;
            continue;
        }
        let messages = messages_for(db, conversation.id).await?;
        if is_social_channel(&conversation.channel)
            && message.visibility.as_deref() == Some("public")
            && (conversation.interaction_type == "direct_message"
                || messages.iter().any(|entry| {
                    entry.direction == direction::INBOUND
                        && entry.visibility.as_deref() == Some("private")
                }))
        {
            set_delivery(db, message.id, delivery::SUPPRESSED, None).await?;
            journal_attempt(
                db,
                message.id,
                delivery::SUPPRESSED,
                None,
                Some("private conversation cannot receive a public reply"),
            )
            .await?;
            report.suppressed += 1;
            continue;
        }
        let envelope = outbound_envelope(&conversation, &message, &messages);
        if !reserve_dispatch(db, message.id).await? {
            continue;
        }
        match transport.deliver(&envelope).await {
            Ok(TransportOutcome::Submitted { provider_ref }) => {
                set_delivery(db, message.id, delivery::SUBMITTED, Some(&provider_ref)).await?;
                journal_attempt(
                    db,
                    message.id,
                    delivery::SUBMITTED,
                    Some(&provider_ref),
                    None,
                )
                .await?;
                if let Some(fresh) = message_by_id(db, message.id).await? {
                    let _ = project_message(db, &conversation, &fresh).await;
                }
                report.submitted += 1;
            }
            Ok(TransportOutcome::Unknown) => {
                set_delivery(db, message.id, delivery::UNKNOWN, None).await?;
                journal_attempt(db, message.id, delivery::UNKNOWN, None, Some("timeout")).await?;
                report.unknown += 1;
            }
            Ok(TransportOutcome::Rejected { detail }) | Err(detail) => {
                set_delivery(db, message.id, delivery::FAILED, None).await?;
                journal_attempt(db, message.id, delivery::FAILED, None, Some(&detail)).await?;
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

async fn queued_author_still_authorized(
    db: &SurrealDb,
    conversation: &Conversation,
    message: &Message,
) -> Result<bool, InboxError> {
    match message.author_person_id {
        Some(author_id)
            if is_social_channel(&conversation.channel) || conversation.channel == channel::SMS =>
        {
            // Text and social replies retain the same Firm and participation checks.
            social_reply_actor_still_authorized(db, author_id, conversation).await
        }
        Some(author_id) => {
            participant_still_may_reply(db, author_id, conversation.project_id).await
        }
        None => Ok(false),
    }
}

pub(crate) fn outbound_envelope(
    conversation: &Conversation,
    message: &Message,
    messages: &[Message],
) -> OutboundEnvelope {
    let reply_to_provider_message_id = messages
        .iter()
        .find(|candidate| {
            candidate.direction == direction::INBOUND && candidate.provider_message_id.is_some()
        })
        .and_then(|candidate| candidate.provider_message_id.clone());
    OutboundEnvelope {
        channel: conversation.channel.clone(),
        account: conversation.provider_account.clone(),
        interaction_type: conversation.interaction_type.clone(),
        provider_post_id: conversation.provider_post_id.clone(),
        reply_to_provider_message_id,
        provider_thread_ref: conversation.provider_thread_ref.clone(),
        to: conversation.external_address.clone(),
        subject: conversation.subject.clone(),
        body: message.body.clone(),
        purpose: message.purpose.clone(),
        visibility: message.visibility.clone(),
    }
}

/// Apply a provider delivery callback scoped by Firm, channel, and account.
///
/// An unknown provider reference records nothing and does not send. A
/// callback that would move delivery backward is ignored. `unknown` may
/// advance to `delivered`.
///
/// # Errors
///
/// Propagates any database error.
pub async fn apply_delivery(
    db: &SurrealDb,
    firm_id: Uuid,
    channel_name: &str,
    provider_account: &str,
    provider_ref: &str,
    incoming: &str,
) -> Result<Option<Uuid>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::apply_delivery_on(
            &pg,
            db,
            firm_id,
            channel_name,
            provider_account,
            provider_ref,
            incoming,
        ))
        .await;
    }
    let Some(message) =
        find_by_provider_ref(db, firm_id, channel_name, provider_account, provider_ref).await?
    else {
        return Ok(None);
    };
    let next = advance_delivery(&message.delivery_state, incoming);
    if next == message.delivery_state {
        return Ok(Some(message.id));
    }
    set_delivery(db, message.id, next, Some(provider_ref)).await?;
    journal_attempt(db, message.id, next, Some(provider_ref), None).await?;
    if next == delivery::DELIVERED || next == delivery::SUBMITTED {
        if let Some(conversation) = conversation_by_id(db, message.conversation_id).await? {
            if let Some(fresh) = message_by_id(db, message.id).await? {
                let _ = project_message(db, &conversation, &fresh).await;
            }
        }
    }
    Ok(Some(message.id))
}

/// Honor a promotional opt-out and suppress queued promotional sends.
///
/// # Errors
///
/// Propagates any database error.
pub async fn record_promotional_opt_out(
    db: &SurrealDb,
    firm_id: Uuid,
    channel_name: &str,
    external_address: &str,
) -> Result<usize, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::record_promotional_opt_out_on(
            &pg,
            firm_id,
            channel_name,
            external_address,
        ))
        .await;
    }
    let mut response = db
        .query(format!(
            "SELECT {CONVERSATION_SELECT} FROM {CONVERSATION} \
             WHERE firm_id = $firm AND channel = $channel AND external_address = $address"
        ))
        .bind(("firm", record_id(crate::firms::TABLE, firm_id)))
        .bind(("channel", channel_name.to_string()))
        .bind(("address", external_address.to_string()))
        .await?
        .check()?;
    let rows: Vec<ConversationRow> = response.take(0)?;
    let mut suppressed = 0usize;
    for row in rows
        .into_iter()
        .filter_map(ConversationRow::into_conversation)
    {
        db.query(
            "UPDATE $id SET promotional_opt_out = true, promotional_opt_in = false, \
             updated_at = time::now()",
        )
        .bind(("id", record_id(CONVERSATION, row.id)))
        .await?
        .check()?;
        after_write(db, MirrorKey::InboxConversation(row.id)).await;
        for message in messages_for(db, row.id).await? {
            if message.purpose == purpose::PROMOTIONAL && message.delivery_state == delivery::QUEUED
            {
                set_delivery(db, message.id, delivery::SUPPRESSED, None).await?;
                journal_attempt(
                    db,
                    message.id,
                    delivery::SUPPRESSED,
                    None,
                    Some("opted out"),
                )
                .await?;
                suppressed += 1;
            }
        }
    }
    Ok(suppressed)
}

/// Firm intake list. Message bodies are not included.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor cannot access any Firm inbox.
pub async fn list_intake(db: &SurrealDb, actor_id: Uuid) -> Result<Vec<IntakeRow>, InboxError> {
    intake_rows(db, actor_id, None).await
}

/// The Firm intake list narrowed to one Brand, in the store: the same
/// Firm-scoped rule as [`list_intake`] decides what the actor may see, and
/// the Brand narrowing happens beside it so a caller cannot widen the view.
/// Message bodies are not included.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor cannot access any Firm inbox.
pub async fn list_intake_in_brand(
    db: &SurrealDb,
    actor_id: Uuid,
    brand_key: &str,
) -> Result<Vec<IntakeRow>, InboxError> {
    intake_rows(db, actor_id, Some(brand_key)).await
}

/// The Brand keys across the conversations this actor may see, sorted and
/// deduplicated — the filter vocabulary an inbox renders. A Brand with no
/// conversations this actor may see does not appear.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor cannot access any Firm inbox.
pub async fn intake_brands(db: &SurrealDb, actor_id: Uuid) -> Result<Vec<String>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        let brands: BTreeSet<String> = crate::postgres::inbox::list_intake_on(&pg, db, actor_id)
            .await?
            .into_iter()
            .filter_map(|row| row.brand_key)
            .collect();
        return Ok(brands.into_iter().collect());
    }
    let actor = load_person(db, actor_id).await?;
    let brands: BTreeSet<String> = visible_conversations(db, &actor, None)
        .await?
        .into_iter()
        .filter_map(|conversation| conversation.brand_key)
        .collect();
    Ok(brands.into_iter().collect())
}

/// The Firms whose inbox conversations this actor may see; `None` for the
/// Owner, who reads across every Firm. A Client is refused — they read the
/// matter log, not the inbox.
async fn visible_firms(
    db: &SurrealDb,
    actor: &persons::Person,
) -> Result<Option<Vec<Uuid>>, InboxError> {
    if actor.role == Role::Owner {
        return Ok(None);
    }
    if matches!(actor.role, Role::Lawyer | Role::Clerk | Role::Admin) {
        return Ok(Some(
            crate::firms::memberships_for_person(db, actor.id)
                .await
                .map_err(|error| InboxError::Db(error.to_string()))?
                .into_iter()
                .map(|membership| membership.firm_id)
                .collect::<Vec<_>>(),
        ));
    }
    Err(InboxError::Denied)
}

async fn conversations_in_brand(
    db: &SurrealDb,
    brand: Option<&str>,
) -> Result<Vec<Conversation>, InboxError> {
    let Some(brand) = brand else {
        return all_conversations(db).await;
    };
    let mut response = db
        .query(format!(
            "SELECT {CONVERSATION_SELECT} FROM {CONVERSATION} \
             WHERE brand_key = $brand ORDER BY updated_at DESC"
        ))
        .bind(("brand", brand.to_string()))
        .await?
        .check()?;
    let rows: Vec<ConversationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(ConversationRow::into_conversation)
        .collect())
}

/// Conversations the actor may see, optionally narrowed to one Brand. The
/// Firm scope is decided here in the store, and the Brand narrowing happens
/// beside it, before any row reaches a caller.
async fn visible_conversations(
    db: &SurrealDb,
    actor: &persons::Person,
    brand: Option<&str>,
) -> Result<Vec<Conversation>, InboxError> {
    let visible_firms = visible_firms(db, actor).await?;
    let mut conversations = conversations_in_brand(db, brand).await?;
    if let Some(firms) = visible_firms {
        conversations.retain(|conversation| firms.contains(&conversation.firm_id));
    }
    Ok(conversations)
}

async fn intake_rows(
    db: &SurrealDb,
    actor_id: Uuid,
    brand: Option<&str>,
) -> Result<Vec<IntakeRow>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        let rows = crate::postgres::inbox::list_intake_on(&pg, db, actor_id).await?;
        return Ok(rows
            .into_iter()
            .filter(|row| brand.is_none_or(|brand| row.brand_key.as_deref() == Some(brand)))
            .collect());
    }
    let actor = load_person(db, actor_id).await?;
    let conversations = visible_conversations(db, &actor, brand).await?;
    let people = person_badges(db, &conversations).await?;
    let mut rows = Vec::with_capacity(conversations.len());
    for conversation in conversations {
        let contact = sender_identity(db, &conversation).await?;
        let delivery_state = latest_outbound_state(db, conversation.id).await?;
        let assignee = assignee_label(db, conversation.project_id).await?;
        let messages = messages_for(db, conversation.id).await?;
        let person = conversation
            .person_id
            .and_then(|person_id| people.get(&person_id).cloned());
        rows.push(IntakeRow {
            contact,
            id: conversation.id,
            firm_id: conversation.firm_id,
            brand_key: conversation.brand_key.clone(),
            project_id: conversation.project_id,
            channel: conversation.channel,
            assignee,
            unread: conversation.unread,
            delivery: delivery_state.clone(),
            next_action: next_action(
                actor.role,
                conversation.project_id.is_some(),
                &delivery_state,
                false,
            )
            .to_string(),
            external_address: conversation.external_address,
            external_name: conversation.external_name,
            subject: conversation.subject,
            direction: latest_direction(&messages),
            preview: latest_preview(&messages),
            person,
            has_attachments: has_attachments(&messages),
            sent: has_sent(&messages),
            updated_at: conversation.updated_at,
        });
    }
    Ok(rows)
}

/// One unprocessed Firm-inbox message in the process backlog. The body is
/// included for the routing layer's classification; doors never return it
/// to a client. The conversation's subject and addresses give the plan its
/// context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnprocessedRow {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub firm_id: Uuid,
    pub project_id: Option<Uuid>,
    pub channel: String,
    pub provider_message_id: Option<String>,
    pub external_address: String,
    pub external_name: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    pub occurred_at: String,
    pub inserted_at: DateTime<Utc>,
}

/// A message's recorded process state. `processed_at: None` means the
/// message has never been processed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessState {
    /// The recorded route spelling (`route::*`).
    pub route: Option<String>,
    /// When the process run recorded it.
    pub processed_at: Option<DateTime<Utc>>,
    /// The recorded NEF-pipeline outcome (`nef_outcome::*`), when the route
    /// ran the pipeline.
    pub nef_outcome: Option<String>,
}

#[derive(SurrealValue)]
struct UnprocessedMessageRow {
    id: surrealdb::types::RecordId,
    conversation_id: surrealdb::types::RecordId,
    provider_message_id: Option<String>,
    body: String,
    occurred_at: String,
    inserted_at: surrealdb::types::Datetime,
}

#[derive(SurrealValue)]
struct ProcessStateRow {
    route: Option<String>,
    /// The writer stores an RFC 3339 string, matching the message table's
    /// string-timestamp convention (`occurred_at`); parse on the way out.
    processed_at: Option<String>,
    nef_outcome: Option<String>,
}

/// Parse the stored `processed_at` back into a UTC instant. Anything the
/// parser refuses is a corrupted field, reported — never invented around.
fn parse_processed_at(stored: &str) -> Result<DateTime<Utc>, InboxError> {
    DateTime::parse_from_rfc3339(stored)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|error| InboxError::Db(format!("invalid processed_at {stored:?}: {error}")))
}

/// The Firm-inbox messages no process run has recorded yet, oldest first.
/// The owner sees every firm's backlog; lawyers, clerks, and admins see
/// their firms'; anyone else is denied. Each row carries the conversation
/// context and the raw body the classification layer reads.
///
/// # Errors
///
/// A store failure or a denial for a client-tier actor.
pub async fn unprocessed_inbox(
    db: &SurrealDb,
    actor_id: Uuid,
) -> Result<Vec<UnprocessedRow>, InboxError> {
    let actor = load_person(db, actor_id).await?;
    let visible_firms = if actor.role == Role::Owner {
        None
    } else if matches!(actor.role, Role::Lawyer | Role::Clerk | Role::Admin) {
        Some(
            crate::firms::memberships_for_person(db, actor_id)
                .await
                .map_err(|error| InboxError::Db(error.to_string()))?
                .into_iter()
                .map(|membership| membership.firm_id)
                .collect::<Vec<_>>(),
        )
    } else {
        return Err(InboxError::Denied);
    };

    if let Some(pg) = inbox_authority().await? {
        return crate::postgres::inbox::unprocessed_inbox_on(&pg, visible_firms.as_deref()).await;
    }
    let mut response = db
        .query(format!(
            "SELECT id, conversation_id, provider_message_id, body, occurred_at, inserted_at \
             FROM {MESSAGE} WHERE direction = 'inbound' AND (processed_at IS NONE \
             OR nef_outcome IN ['{}', '{}', '{}']) ORDER BY inserted_at ASC",
            crate::inbox::nef_outcome::NO_PROJECT,
            crate::inbox::nef_outcome::NOT_A_NOTICE,
            crate::inbox::nef_outcome::NO_CASE_NUMBER
        ))
        .await?
        .check()?;
    let rows: Vec<UnprocessedMessageRow> = response.take(0)?;

    let mut unprocessed = Vec::new();
    for row in rows {
        let Some(message_id) = record_uuid(&row.id) else {
            continue;
        };
        let Some(conversation_id) = record_uuid(&row.conversation_id) else {
            continue;
        };
        let conversation = conversation_by_id(db, conversation_id)
            .await?
            .ok_or(InboxError::NotFound)?;
        if let Some(firms) = &visible_firms {
            if !firms.contains(&conversation.firm_id) {
                continue;
            }
        }
        unprocessed.push(UnprocessedRow {
            id: message_id,
            conversation_id: conversation.id,
            firm_id: conversation.firm_id,
            project_id: conversation.project_id,
            channel: conversation.channel,
            provider_message_id: row.provider_message_id,
            external_address: conversation.external_address,
            external_name: conversation.external_name,
            subject: conversation.subject,
            body: row.body,
            occurred_at: row.occurred_at,
            inserted_at: row.inserted_at.into(),
        });
    }
    Ok(unprocessed)
}

/// The recorded process state of one message. `Ok(None)` means the message
/// does not exist; a state with `processed_at: None` has never been
/// processed.
///
/// # Errors
///
/// A store failure.
pub async fn process_state(
    db: &SurrealDb,
    message_id: Uuid,
) -> Result<Option<ProcessState>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return crate::postgres::inbox::process_state_on(&pg, message_id).await;
    }
    let mut response = db
        .query("SELECT route, processed_at, nef_outcome FROM ONLY $id")
        .bind(("id", record_id(MESSAGE, message_id)))
        .await?
        .check()?;
    let row: Option<ProcessStateRow> = response.take(0)?;
    row.map(|row| {
        Ok(ProcessState {
            route: row.route,
            processed_at: row
                .processed_at
                .map(|stored| parse_processed_at(&stored))
                .transpose()?,
            nef_outcome: row.nef_outcome,
        })
    })
    .transpose()
}

/// Record the route, processing time, and — when the route ran the NEF
/// pipeline — its outcome on one message. Idempotent: a re-run overwrites
/// with the same truth, and the Postgres mirror write follows.
///
/// # Errors
///
/// An unknown route or outcome spelling, a store failure, or a mirror-write
/// failure.
pub async fn mark_processed(
    db: &SurrealDb,
    message_id: Uuid,
    route: &str,
    processed_at: DateTime<Utc>,
    outcome: Option<&str>,
) -> Result<(), InboxError> {
    if !route::is_valid(route) {
        return Err(InboxError::InvalidRoute(route.to_owned()));
    }
    if let Some(spelling) = outcome {
        if !crate::inbox::nef_outcome::ALL.contains(&spelling) {
            return Err(InboxError::InvalidRoute(spelling.to_owned()));
        }
    }
    if let Some(pg) = inbox_authority().await? {
        return crate::postgres::inbox::mark_processed_on(
            &pg,
            message_id,
            route,
            processed_at,
            outcome,
        )
        .await;
    }
    db.query(
        "UPDATE $id SET route = $route, processed_at = $processed_at, \
         nef_outcome = $nef_outcome, updated_at = time::now()",
    )
    .bind(("id", record_id(MESSAGE, message_id)))
    .bind(("route", route))
    .bind(("processed_at", processed_at.to_rfc3339()))
    .bind(("nef_outcome", outcome))
    .await?
    .check()?;
    after_write(db, MirrorKey::InboxMessage(message_id)).await;
    Ok(())
}

/// One message's process context: the row the classification layer reads
/// plus the recorded state. Visibility follows the intake gate — the owner
/// sees any firm; lawyers, clerks, and admins their firms; a missing record
/// and a firm outside the caller's membership both report not found, so
/// the door never discloses a conversation it cannot serve.
///
/// # Errors
///
/// A store failure, a denial for a client-tier actor, or not found
/// (including out-of-scope).
pub async fn process_context(
    db: &SurrealDb,
    actor_id: Uuid,
    message_id: Uuid,
) -> Result<ProcessContext, InboxError> {
    let actor = load_person(db, actor_id).await?;
    let message = message_by_id(db, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let conversation = conversation_by_id(db, message.conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if actor.role != Role::Owner {
        if !matches!(actor.role, Role::Lawyer | Role::Clerk | Role::Admin) {
            return Err(InboxError::Denied);
        }
        let memberships = crate::firms::memberships_for_person(db, actor_id)
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
        if !memberships
            .iter()
            .any(|membership| membership.firm_id == conversation.firm_id)
        {
            return Err(InboxError::NotFound);
        }
    }
    let state = process_state(db, message_id)
        .await?
        .unwrap_or(ProcessState {
            route: None,
            processed_at: None,
            nef_outcome: None,
        });
    Ok(ProcessContext {
        row: UnprocessedRow {
            id: message.id,
            conversation_id: message.conversation_id,
            firm_id: conversation.firm_id,
            project_id: conversation.project_id,
            channel: conversation.channel,
            provider_message_id: message.provider_message_id,
            external_address: conversation.external_address,
            external_name: conversation.external_name,
            subject: conversation.subject,
            body: message.body,
            occurred_at: message.occurred_at,
            inserted_at: message.inserted_at,
        },
        state,
    })
}

/// The intake gate's row plus the recorded process state, served by the
/// single-message doors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessContext {
    /// The message and conversation facts classification reads.
    pub row: UnprocessedRow,
    /// What a previous process run recorded, if anything.
    pub state: ProcessState,
}

/// Intake detail. Assigned message bodies stay hidden unless the actor
/// participates on that Project.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor lacks Firm access.
/// [`InboxError::NotFound`] when the conversation does not exist.
pub async fn intake_detail(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
) -> Result<IntakeDetail, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::intake_detail_on(
            &pg,
            db,
            actor_id,
            conversation_id,
        ))
        .await;
    }
    let actor = load_person(db, actor_id).await?;
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if actor.role != Role::Owner
        && (!matches!(actor.role, Role::Lawyer | Role::Clerk | Role::Admin)
            || !crate::firms::memberships_for_person(db, actor_id)
                .await
                .map_err(|error| InboxError::Db(error.to_string()))?
                .iter()
                .any(|membership| membership.firm_id == conversation.firm_id))
    {
        return Err(InboxError::Denied);
    }
    let participates = match conversation.project_id {
        Some(project_id) => sees_project(db, actor_id, actor.role, project_id).await?,
        None => false,
    };
    let can_social_reply = is_social_channel(&conversation.channel)
        && social_reply_actor_still_authorized(db, actor_id, &conversation).await?;
    let can_sms_reply = conversation.channel == channel::SMS
        && !crate::fi_import::is_account(&conversation.provider_account)
        && social_reply_actor_still_authorized(db, actor_id, &conversation).await?;
    let bodies_visible =
        conversation.project_id.is_none() || participates || can_social_reply || can_sms_reply;
    let messages = if bodies_visible {
        messages_for(db, conversation.id).await?
    } else {
        Vec::new()
    };
    if bodies_visible {
        mark_read(db, conversation.id).await?;
    }
    let delivery_state = latest_outbound_state(db, conversation.id).await?;
    let contact = sender_identity(db, &conversation).await?;
    Ok(IntakeDetail {
        contact,
        projects: projects_in_firm(db, conversation.firm_id).await?,
        next_action: if can_social_reply || can_sms_reply {
            next_action(actor.role, true, &delivery_state, true).to_string()
        } else {
            next_action(
                actor.role,
                conversation.project_id.is_some(),
                &delivery_state,
                false,
            )
            .to_string()
        },
        conversation,
        messages,
        bodies_visible,
        can_social_reply,
        can_sms_reply,
    })
}

/// Project inbox for a participating Lawyer, Clerk, or — read-only — any
/// other participant. Clients are refused; they read the matter
/// communication log, which omits internal notes.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor does not participate, or is a Client.
/// [`InboxError::NotFound`] when the conversation is not on that Project.
pub async fn project_thread(
    db: &SurrealDb,
    actor_id: Uuid,
    project_id: Uuid,
    conversation_id: Uuid,
) -> Result<ThreadView, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::project_thread_on(
            &pg,
            db,
            actor_id,
            project_id,
            conversation_id,
        ))
        .await;
    }
    let actor = load_person(db, actor_id).await?;
    if actor.role == Role::Client || !sees_project(db, actor_id, actor.role, project_id).await? {
        return Err(InboxError::Denied);
    }
    let conversation = conversation_by_id(db, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.project_id != Some(project_id) {
        return Err(InboxError::NotFound);
    }
    let can_note = matches!(actor.role, Role::Lawyer | Role::Clerk);
    let can_send =
        matches!(actor.role, Role::Lawyer | Role::Clerk) && allows_external_reply(&conversation);
    let delivery_state = latest_outbound_state(db, conversation.id).await?;
    let messages = messages_for(db, conversation.id).await?;
    mark_read(db, conversation.id).await?;
    Ok(ThreadView {
        assignee: assignee_label(db, conversation.project_id).await?,
        delivery: delivery_state.clone(),
        next_action: next_action(actor.role, true, &delivery_state, can_send).to_string(),
        conversation,
        messages,
        can_note,
        can_send,
        can_attribute: matches!(actor.role, Role::Lawyer | Role::Clerk),
    })
}

/// Conversations assigned to one Project, for a participant who may open
/// the Project inbox.
///
/// # Errors
///
/// [`InboxError::Denied`] for a non-participant or a Client.
pub async fn list_project(
    db: &SurrealDb,
    actor_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<IntakeRow>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::list_project_on(
            &pg, db, actor_id, project_id,
        ))
        .await;
    }
    let actor = load_person(db, actor_id).await?;
    if actor.role == Role::Client || !sees_project(db, actor_id, actor.role, project_id).await? {
        return Err(InboxError::Denied);
    }
    let mut response = db
        .query(format!(
            "SELECT {CONVERSATION_SELECT} FROM {CONVERSATION} WHERE project_id = $project \
             ORDER BY updated_at DESC"
        ))
        .bind(("project", record_id(projects::PROJECT_TABLE, project_id)))
        .await?
        .check()?;
    let rows: Vec<ConversationRow> = response.take(0)?;
    let can_send = matches!(actor.role, Role::Lawyer | Role::Clerk);
    let conversations: Vec<Conversation> = rows
        .into_iter()
        .filter_map(ConversationRow::into_conversation)
        .collect();
    let people = person_badges(db, &conversations).await?;
    let mut listed = Vec::new();
    for conversation in conversations {
        let contact = sender_identity(db, &conversation).await?;
        let delivery_state = latest_outbound_state(db, conversation.id).await?;
        let messages = messages_for(db, conversation.id).await?;
        let person = conversation
            .person_id
            .and_then(|person_id| people.get(&person_id).cloned());
        listed.push(IntakeRow {
            contact,
            id: conversation.id,
            firm_id: conversation.firm_id,
            brand_key: conversation.brand_key.clone(),
            project_id: conversation.project_id,
            channel: conversation.channel.clone(),
            assignee: assignee_label(db, conversation.project_id).await?,
            unread: conversation.unread,
            next_action: next_action(
                actor.role,
                true,
                &delivery_state,
                can_send && allows_external_reply(&conversation),
            )
            .to_string(),
            delivery: delivery_state,
            external_address: conversation.external_address,
            external_name: conversation.external_name,
            subject: conversation.subject,
            direction: latest_direction(&messages),
            preview: latest_preview(&messages),
            person,
            has_attachments: has_attachments(&messages),
            sent: has_sent(&messages),
            updated_at: conversation.updated_at,
        });
    }
    Ok(listed)
}

/// Unread conversations assigned to one Project — the matter dashboard's
/// correspondence tile. A read-only projection over the conversations'
/// existing `unread` flag, firm-wide as the Inbox itself keeps it (there is
/// no per-participant read-state to derive a per-reader count from). No
/// authorization here: every caller resolves its own reader's access before
/// reaching this — the dashboard through the `matter_viewer` seam, the
/// project inbox through [`list_project`].
///
/// # Errors
///
/// Propagates any database error.
pub async fn unread_count_for_project(db: &SurrealDb, project_id: Uuid) -> Result<i64, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::unread_count_for_project_on(
            &pg, project_id,
        ))
        .await;
    }
    let mut response = db
        .query(format!(
            "SELECT VALUE count() FROM {CONVERSATION} WHERE project_id = $project \
             AND unread = true GROUP ALL"
        ))
        .bind(("project", record_id(projects::PROJECT_TABLE, project_id)))
        .await?
        .check()?;
    // `SELECT VALUE count()` yields bare numbers — one per GROUP ALL group,
    // and no row at all when nothing matches, which is a count of zero.
    let counts: Vec<i64> = response.take(0)?;
    Ok(counts.first().copied().unwrap_or(0))
}

/// Whether a participating Lawyer or Clerk may relay this email thread now.
///
/// The check reads the person row and the current participation, so a
/// revocation between accept and send is visible.
///
/// # Errors
///
/// Propagates any database error.
pub async fn clerk_may_relay_email_thread(
    db: &SurrealDb,
    person_id: Uuid,
    email_conversation_id: Uuid,
) -> Result<bool, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::clerk_may_relay_email_thread_on(
            &pg,
            db,
            person_id,
            email_conversation_id,
        ))
        .await;
    }
    let Ok(person) = load_person(db, person_id).await else {
        return Ok(false);
    };
    if !matches!(person.role, Role::Lawyer | Role::Clerk) {
        return Ok(false);
    }
    let Some(conversation) = find_by_email_conversation(db, email_conversation_id).await? else {
        return Ok(false);
    };
    participant_still_may_reply(db, person_id, conversation.project_id).await
}

/// Assignment history, oldest first.
///
/// # Errors
///
/// Propagates any database error.
pub async fn assignments(
    db: &SurrealDb,
    conversation_id: Uuid,
) -> Result<Vec<Assignment>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::assignments_on(&pg, conversation_id)).await;
    }
    let mut response = db
        .query(format!(
            "SELECT {ASSIGNMENT_SELECT} FROM {ASSIGNMENT} WHERE conversation_id = $conversation \
             ORDER BY inserted_at"
        ))
        .bind(("conversation", record_id(CONVERSATION, conversation_id)))
        .await?
        .check()?;
    let rows: Vec<AssignmentRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(AssignmentRow::into_assignment)
        .collect())
}

/// Asset id for a message the actor may read, or `None` when it has no
/// attachment. A caller who may not read the message is [`InboxError::Denied`].
///
/// # Errors
///
/// [`InboxError::Denied`] or [`InboxError::NotFound`].
pub async fn readable_attachment(
    db: &SurrealDb,
    actor_id: Uuid,
    message_id: Uuid,
) -> Result<Option<Uuid>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::readable_attachment_on(
            &pg, db, actor_id, message_id,
        ))
        .await;
    }
    let message = message_by_id(db, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let conversation = conversation_by_id(db, message.conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let actor = load_person(db, actor_id).await?;
    let allowed = match (actor.role, conversation.project_id) {
        (Role::Owner, None) => true,
        (Role::Owner, Some(project_id)) => {
            sees_project(db, actor_id, Role::Owner, project_id).await?
        }
        (Role::Client, _) => false,
        (_, Some(project_id)) => {
            let sees = sees_project(db, actor_id, actor.role, project_id).await?;
            if message.direction == direction::INTERNAL {
                sees && matches!(actor.role, Role::Lawyer | Role::Clerk)
            } else {
                sees && actor.role != Role::Client
            }
        }
        _ => false,
    };
    if !allowed {
        return Err(InboxError::Denied);
    }
    Ok(message.attachment_asset_id)
}

pub async fn conversation_by_id(
    db: &SurrealDb,
    id: Uuid,
) -> Result<Option<Conversation>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::conversation_by_id_on(&pg, id)).await;
    }
    let mut response = db
        .query(format!("SELECT {CONVERSATION_SELECT} FROM ONLY $id"))
        .bind(("id", record_id(CONVERSATION, id)))
        .await?
        .check()?;
    let row: Option<ConversationRow> = response.take(0)?;
    let conversation = row.and_then(ConversationRow::into_conversation);
    crate::postgres_shadow::conversation_read(id, conversation.as_ref());
    Ok(conversation)
}

pub async fn message_by_id(db: &SurrealDb, id: Uuid) -> Result<Option<Message>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::message_by_id_on(&pg, id)).await;
    }
    let mut response = db
        .query(format!("SELECT {MESSAGE_SELECT} FROM ONLY $id"))
        .bind(("id", record_id(MESSAGE, id)))
        .await?
        .check()?;
    let row: Option<MessageRow> = response.take(0)?;
    let message = row.and_then(MessageRow::into_message);
    crate::postgres_shadow::message_read(id, message.as_ref());
    Ok(message)
}

// This stable key is part of the receipt's persistent idempotency contract.
pub(crate) fn acknowledgment_id(inbound_message_id: Uuid) -> Uuid {
    Uuid::from_u128(inbound_message_id.as_u128() ^ (1_u128 << 127))
}

/// The system receipt reserved for an inbound message, including an unknown send.
pub async fn acknowledgment_for(
    db: &SurrealDb,
    inbound_message_id: Uuid,
) -> Result<Option<Message>, InboxError> {
    message_by_id(db, acknowledgment_id(inbound_message_id)).await
}

/// Reserve one automatic acknowledgment per inbound message before sending.
/// A reserved row is `unknown`: a crash must never cause a second external send.
/// This system receipt carries no Clerk authority and cannot dispatch a matter reply.
pub async fn reserve_acknowledgment(
    db: &SurrealDb,
    inbound: &IngestedInbound,
    body: &str,
) -> Result<Option<Uuid>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::reserve_acknowledgment_on(
            &pg, inbound, body,
        ))
        .await;
    }
    let id = acknowledgment_id(inbound.message_id);
    if message_by_id(db, id).await?.is_some() {
        return Ok(None);
    }
    let result = db
        .query(
            "CREATE ONLY $id SET conversation_id = $conversation, direction = 'outbound', \
         body = $body, delivery_state = 'unknown', purpose = 'matter', \
         occurred_at = $occurred, inserted_at = time::now(), updated_at = time::now()",
        )
        .bind(("id", record_id(MESSAGE, id)))
        .bind((
            "conversation",
            record_id(CONVERSATION, inbound.conversation_id),
        ))
        .bind(("body", body.to_string()))
        .bind(("occurred", Utc::now().to_rfc3339()))
        .await?
        .check();
    match result {
        Ok(_) => {
            after_write(db, MirrorKey::InboxMessage(id)).await;
            Ok(Some(id))
        }
        Err(_) if message_by_id(db, id).await?.is_some() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Record provider acceptance of a reserved acknowledgment.
pub async fn acknowledgment_submitted(
    db: &SurrealDb,
    id: Uuid,
    provider_ref: Option<String>,
) -> Result<(), InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return Box::pin(crate::postgres::inbox::acknowledgment_submitted_on(
            &pg,
            id,
            provider_ref,
        ))
        .await;
    }
    db.query("UPDATE $id SET delivery_state = 'submitted', provider_ref = $provider, updated_at = time::now()")
        .bind(("id", record_id(MESSAGE, id)))
        .bind(("provider", provider_ref))
        .await?.check()?;
    after_write(db, MirrorKey::InboxMessage(id)).await;
    Ok(())
}

pub(crate) fn advance_delivery(current: &str, incoming: &str) -> &'static str {
    if matches!(current, delivery::FAILED | delivery::SUPPRESSED) {
        return match current {
            delivery::FAILED => delivery::FAILED,
            _ => delivery::SUPPRESSED,
        };
    }
    if current == delivery::DELIVERED {
        return delivery::DELIVERED;
    }
    if current == delivery::UNKNOWN && incoming == delivery::SUBMITTED {
        return delivery::UNKNOWN;
    }
    match incoming {
        delivery::DELIVERED => delivery::DELIVERED,
        delivery::SUBMITTED => delivery::SUBMITTED,
        delivery::FAILED => delivery::FAILED,
        delivery::SUPPRESSED => delivery::SUPPRESSED,
        _ => delivery::UNKNOWN,
    }
}

pub(crate) async fn participant_still_may_reply(
    db: &SurrealDb,
    person_id: Uuid,
    project_id: Option<Uuid>,
) -> Result<bool, InboxError> {
    let person = match load_person(db, person_id).await {
        Ok(person) => person,
        Err(InboxError::NotFound) => return Ok(false),
        Err(error) => return Err(error),
    };
    if !matches!(person.role, Role::Lawyer | Role::Clerk) {
        return Ok(false);
    }
    let Some(project_id) = project_id else {
        return Ok(false);
    };
    sees_project(db, person_id, person.role, project_id).await
}

pub(crate) fn is_social_channel(channel_name: &str) -> bool {
    matches!(
        channel_name,
        channel::FACEBOOK
            | channel::INSTAGRAM
            | channel::THREADS
            | channel::YOUTUBE
            | channel::TIKTOK
            | channel::X
            | channel::LINKEDIN
    )
}

/// Whether a conversation's counterparty address is a phone number. A call
/// arrives from the same E.164 address a text does, so the display-only
/// contact link applies to both.
fn has_phone_counterparty(channel_name: &str) -> bool {
    matches!(channel_name, channel::SMS | channel::VOICE)
}

async fn authorize_social_reply(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation: &Conversation,
) -> Result<(), InboxError> {
    if social_reply_actor_still_authorized(db, actor_id, conversation).await? {
        Ok(())
    } else {
        Err(InboxError::Denied)
    }
}

/// Social moderation authority belongs to the deployment Owner or the
/// conversation's Firm Admin DRI. A matter-side Lawyer/Clerk may reply only
/// while their Project participation remains active.
pub(crate) async fn social_reply_actor_still_authorized(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation: &Conversation,
) -> Result<bool, InboxError> {
    reply_actor_authorized(db, actor_id, conversation.firm_id, conversation.project_id).await
}

/// The same authority for a Firm and optional Project, so an outbound text can
/// be authorized before its conversation exists.
pub(crate) async fn reply_actor_authorized(
    db: &SurrealDb,
    actor_id: Uuid,
    firm_id: Uuid,
    project_id: Option<Uuid>,
) -> Result<bool, InboxError> {
    let actor = match load_person(db, actor_id).await {
        Ok(actor) => actor,
        Err(InboxError::NotFound) => return Ok(false),
        Err(error) => return Err(error),
    };
    match actor.role {
        Role::Owner => Ok(true),
        Role::Admin => crate::firm_capability::FirmCapability::ManageBrandContent
            .resolve(db, actor.role, Some(actor_id), firm_id)
            .await
            .map(super::firm_capability::FirmCapabilityDecision::is_allowed)
            .map_err(|error| InboxError::Db(error.to_string())),
        Role::Lawyer | Role::Clerk => participant_still_may_reply(db, actor_id, project_id).await,
        Role::Client => Ok(false),
    }
}

pub(crate) async fn sees_project(
    db: &SurrealDb,
    person_id: Uuid,
    role: Role,
    project_id: Uuid,
) -> Result<bool, InboxError> {
    can_see_project(db, Some(person_id), role, project_id)
        .await
        .map_err(InboxError::Db)
}

pub(crate) async fn load_person(
    db: &SurrealDb,
    person_id: Uuid,
) -> Result<persons::Person, InboxError> {
    persons::find_by_id(db, person_id)
        .await?
        .ok_or(InboxError::NotFound)
}

async fn find_ingress(
    db: &SurrealDb,
    firm_id: Uuid,
    channel_name: &str,
    provider_account: &str,
    provider_message_id: &str,
) -> Result<Option<Uuid>, InboxError> {
    let mut response = db
        .query(
            "SELECT message_id FROM inbox_ingress WHERE firm_id = $firm AND channel = $channel \
             AND provider_account = $account AND provider_message_id = $provider LIMIT 1",
        )
        .bind(("firm", record_id(crate::firms::TABLE, firm_id)))
        .bind(("channel", channel_name.to_string()))
        .bind(("account", provider_account.to_string()))
        .bind(("provider", provider_message_id.to_string()))
        .await?
        .check()?;
    let rows: Vec<IngressRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .find_map(|row| record_uuid(&row.message_id)))
}

async fn parent_message_id(
    db: &SurrealDb,
    conversation_id: Uuid,
    provider_message_id: &str,
) -> Result<Option<Uuid>, InboxError> {
    let mut response = db
        .query("SELECT VALUE id FROM message WHERE conversation_id = $conversation AND provider_message_id = $provider LIMIT 1")
        .bind(("conversation", record_id(CONVERSATION, conversation_id)))
        .bind(("provider", provider_message_id.to_string()))
        .await?
        .check()?;
    let id: Option<surrealdb::types::RecordId> = response.take(0)?;
    Ok(id.as_ref().and_then(record_uuid))
}

#[derive(SurrealValue)]
struct ReferencedRow {
    conversation_id: surrealdb::types::RecordId,
    provider_message_id: String,
}

/// The Conversation [`ingest_inbound_threaded`] joins: same Firm, channel,
/// and Project attribution, keyed on the provider thread id first and then
/// on the nearest referenced Message.
async fn find_referenced_thread(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
    references: &[String],
) -> Result<Option<Conversation>, InboxError> {
    let project = inbound.project_id.map(|id| record_id("project", id));
    let mut conversation_id = None;
    if let Some(thread) = inbound.provider_thread_ref {
        let mut response = db
            .query(format!(
                "SELECT VALUE id FROM {CONVERSATION} WHERE firm_id = $firm AND channel = $channel \
                 AND project_id = $project AND provider_thread_ref = $thread \
                 ORDER BY inserted_at DESC LIMIT 1"
            ))
            .bind(("firm", record_id(crate::firms::TABLE, inbound.firm_id)))
            .bind(("channel", inbound.channel.to_string()))
            .bind(("project", project.clone()))
            .bind(("thread", thread.to_string()))
            .await?
            .check()?;
        let ids: Vec<surrealdb::types::RecordId> = response.take(0)?;
        conversation_id = ids.first().and_then(record_uuid);
    }
    if conversation_id.is_none() && !references.is_empty() {
        let mut response = db
            .query(format!(
                "SELECT conversation_id, provider_message_id FROM {MESSAGE} \
                 WHERE provider_message_id IN $references \
                 AND conversation_id.firm_id = $firm AND conversation_id.channel = $channel \
                 AND conversation_id.project_id = $project"
            ))
            .bind(("references", references.to_vec()))
            .bind(("firm", record_id(crate::firms::TABLE, inbound.firm_id)))
            .bind(("channel", inbound.channel.to_string()))
            .bind(("project", project))
            .await?
            .check()?;
        let rows: Vec<ReferencedRow> = response.take(0)?;
        let hits: Vec<(String, Uuid)> = rows
            .into_iter()
            .filter_map(|row| Some((row.provider_message_id, record_uuid(&row.conversation_id)?)))
            .collect();
        conversation_id = nearest_referenced_conversation(references, &hits);
    }
    let Some(conversation_id) = conversation_id else {
        return Ok(None);
    };
    let mut response = db
        .query(format!("SELECT {CONVERSATION_SELECT} FROM ONLY $id"))
        .bind(("id", record_id(CONVERSATION, conversation_id)))
        .await?
        .check()?;
    let row: Option<ConversationRow> = response.take(0)?;
    Ok(row.and_then(ConversationRow::into_conversation))
}

async fn find_open_thread(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
) -> Result<Option<Conversation>, InboxError> {
    let mut response = db
        .query(format!(
            "SELECT {CONVERSATION_SELECT} FROM {CONVERSATION} WHERE firm_id = $firm \
             AND channel = $channel AND brand_key = $brand AND provider_account = $account \
             AND interaction_type = $interaction \
             AND (provider_thread_ref = $thread OR $thread IS NONE) \
             AND project_id = $project \
             AND external_address = $address ORDER BY inserted_at DESC LIMIT 1"
        ))
        .bind(("firm", record_id(crate::firms::TABLE, inbound.firm_id)))
        .bind(("channel", inbound.channel.to_string()))
        .bind(("brand", inbound.brand_key.map(str::to_string)))
        .bind((
            "interaction",
            inbound.interaction_type.unwrap_or("message").to_string(),
        ))
        .bind(("thread", inbound.provider_thread_ref.map(str::to_string)))
        .bind((
            "project",
            inbound.project_id.map(|id| record_id("project", id)),
        ))
        .bind(("account", inbound.provider_account.to_string()))
        .bind(("address", inbound.external_address.to_string()))
        .await?
        .check()?;
    let rows: Vec<ConversationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .find_map(ConversationRow::into_conversation))
}

async fn create_conversation(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
    person_id: Option<Uuid>,
) -> Result<Conversation, InboxError> {
    let id = Uuid::now_v7();
    let mut response = db
        .query(format!(
            "CREATE $id SET firm_id = $firm, brand_key = $brand, interaction_type = $interaction_type, \
             provider_post_id = $provider_post_id, project_id = $project, channel = $channel, \
             external_address = $address, external_name = $name, subject = $subject, \
             unread = true, provider_account = $account, provider_thread_ref = $thread, \
             source_page = $source_page, source_channel = $source_channel, \
             matter_consent = $matter_consent, promotional_opt_in = $promotional, \
             promotional_opt_out = false, email_conversation_id = $email_conversation, \
             person_id = $person, inserted_at = time::now(), updated_at = time::now() \
             RETURN {CONVERSATION_SELECT}"
        ))
        .bind(("id", record_id(CONVERSATION, id)))
        .bind(("firm", record_id(crate::firms::TABLE, inbound.firm_id)))
        .bind(("brand", inbound.brand_key.map(str::to_string)))
        .bind(("interaction_type", inbound.interaction_type.unwrap_or("message").to_string()))
        .bind(("provider_post_id", inbound.provider_post_id.map(str::to_string)))
        .bind((
            "project",
            inbound.project_id.map(|id| record_id("project", id)),
        ))
        .bind(("channel", inbound.channel.to_string()))
        .bind(("address", inbound.external_address.to_string()))
        .bind(("name", inbound.external_name.map(str::to_string)))
        .bind(("subject", inbound.subject.map(str::to_string)))
        .bind(("account", inbound.provider_account.to_string()))
        .bind(("thread", inbound.provider_thread_ref.map(str::to_string)))
        .bind(("source_page", inbound.source_page.map(str::to_string)))
        .bind(("source_channel", inbound.source_channel.map(str::to_string)))
        .bind(("matter_consent", inbound.matter_consent))
        .bind(("promotional", inbound.promotional_opt_in))
        .bind((
            "email_conversation",
            inbound
                .email_conversation_id
                .map(|id| record_id("email_conversation", id)),
        ))
        .bind(("person", person_id.map(|id| record_id("person", id))))
        .await?
        .check()?;
    let rows: Vec<ConversationRow> = response.take(0)?;
    let conversation = rows
        .into_iter()
        .find_map(ConversationRow::into_conversation)
        .ok_or(InboxError::NotFound)?;
    after_write(db, MirrorKey::InboxConversation(id)).await;
    Ok(conversation)
}

async fn link_email_conversation(
    db: &SurrealDb,
    conversation_id: Uuid,
    email_conversation_id: Option<Uuid>,
) -> Result<(), InboxError> {
    db.query("UPDATE $id SET email_conversation_id = $email, updated_at = time::now()")
        .bind(("id", record_id(CONVERSATION, conversation_id)))
        .bind((
            "email",
            email_conversation_id.map(|id| record_id("email_conversation", id)),
        ))
        .await?
        .check()?;
    after_write(db, MirrorKey::InboxConversation(conversation_id)).await;
    Ok(())
}

async fn create_ingress(
    db: &SurrealDb,
    inbound: &IngestInbound<'_>,
    message_id: Uuid,
) -> Result<(), InboxError> {
    let id = Uuid::now_v7();
    db.query(
        "CREATE $id SET firm_id = $firm, channel = $channel, provider_account = $account, \
         provider_message_id = $provider, message_id = $message, inserted_at = time::now()",
    )
    .bind(("id", record_id(INGRESS, id)))
    .bind(("firm", record_id(crate::firms::TABLE, inbound.firm_id)))
    .bind(("channel", inbound.channel.to_string()))
    .bind(("account", inbound.provider_account.to_string()))
    .bind(("provider", inbound.provider_message_id.to_string()))
    .bind(("message", record_id(MESSAGE, message_id)))
    .await?
    .check()?;
    after_write(db, MirrorKey::InboxIngress(id)).await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn create_message(
    db: &SurrealDb,
    conversation_id: Uuid,
    direction_value: &str,
    body: &str,
    author_person_id: Option<Uuid>,
    provider_message_id: Option<&str>,
    provider_ref: Option<&str>,
    delivery_state: &str,
    purpose_value: &str,
    visibility: Option<&str>,
    attachment_asset_id: Option<Uuid>,
    occurred_at: &str,
    raw_storage_key: Option<&str>,
) -> Result<Uuid, InboxError> {
    let id = Uuid::now_v7();
    db.query(
        "CREATE $id SET conversation_id = $conversation, direction = $direction, body = $body, \
         visibility = $visibility, alert_state = $alert_state, \
         author_person_id = $author, provider_message_id = $provider_message, \
         provider_ref = $provider_ref, delivery_state = $delivery, purpose = $purpose, \
         attachment_asset_id = $attachment, raw_storage_key = $raw_storage_key, \
         communication_id = NONE, occurred_at = $occurred, \
         inserted_at = time::now(), updated_at = time::now()",
    )
    .bind(("id", record_id(MESSAGE, id)))
    .bind(("conversation", record_id(CONVERSATION, conversation_id)))
    .bind(("direction", direction_value.to_string()))
    .bind((
        "alert_state",
        (direction_value == direction::INBOUND).then(|| "pending".to_string()),
    ))
    .bind(("body", body.to_string()))
    .bind(("author", author_person_id.map(|id| record_id("person", id))))
    .bind(("provider_message", provider_message_id.map(str::to_string)))
    .bind(("provider_ref", provider_ref.map(str::to_string)))
    .bind(("delivery", delivery_state.to_string()))
    .bind(("purpose", purpose_value.to_string()))
    .bind(("visibility", visibility.map(str::to_string)))
    .bind((
        "attachment",
        attachment_asset_id.map(|id| record_id("asset", id)),
    ))
    .bind(("occurred", occurred_at.to_string()))
    .bind(("raw_storage_key", raw_storage_key.map(str::to_string)))
    .await?
    .check()?;
    after_write(db, MirrorKey::InboxMessage(id)).await;
    Ok(id)
}

async fn project_all_messages(
    db: &SurrealDb,
    conversation: &Conversation,
) -> Result<(), InboxError> {
    for message in messages_for(db, conversation.id).await? {
        project_message(db, conversation, &message).await?;
    }
    Ok(())
}

async fn project_message(
    db: &SurrealDb,
    conversation: &Conversation,
    message: &Message,
) -> Result<(), InboxError> {
    let Some(project_id) = conversation.project_id else {
        return Ok(());
    };
    if let Some(communication_id) = message.communication_id {
        communications::move_to_project(db, communication_id, project_id).await?;
        return Ok(());
    }
    let source_ref = format!("inbox:{}", message.id);
    let channel_name = spine_channel(&conversation.channel, &message.direction);
    if channel_name.is_empty() {
        return Err(InboxError::Denied);
    }
    let spine_direction = if message.direction == direction::INTERNAL {
        communications::direction::INTERNAL
    } else if message.direction == direction::OUTBOUND {
        communications::direction::OUTBOUND
    } else {
        communications::direction::INBOUND
    };
    let ingested = communications::ingest(
        db,
        &IngestArgs {
            project_id,
            channel: channel_name,
            direction: spine_direction,
            author_person_id: message.author_person_id,
            counterparty: Some(conversation.external_address.as_str()),
            subject: conversation.subject.as_deref(),
            body: &message.body,
            source_ref: Some(&source_ref),
            asset_id: message.attachment_asset_id,
            occurred_at: &message.occurred_at,
        },
    )
    .await?;
    db.query("UPDATE $id SET communication_id = $communication, updated_at = time::now()")
        .bind(("id", record_id(MESSAGE, message.id)))
        .bind((
            "communication",
            record_id("communication", ingested.communication_id),
        ))
        .await?
        .check()?;
    after_write(db, MirrorKey::InboxMessage(message.id)).await;
    if ingested.deduped {
        communications::move_to_project(db, ingested.communication_id, project_id).await?;
    }
    Ok(())
}

pub(crate) fn spine_channel(channel_name: &str, direction_value: &str) -> &'static str {
    match (channel_name, direction_value) {
        (channel::VOICE, _) => communications::channel::CALL,
        (channel::SMS, direction::OUTBOUND) => communications::channel::SMS_OUTBOUND,
        (channel::SMS, _) => communications::channel::SMS_INBOUND,
        (channel::EMAIL, direction::OUTBOUND) => communications::channel::EMAIL_OUTBOUND,
        (channel::EMAIL, _) => communications::channel::EMAIL_INBOUND,
        (channel::FACEBOOK, direction::OUTBOUND) => communications::channel::FACEBOOK_OUTBOUND,
        (channel::FACEBOOK, _) => communications::channel::FACEBOOK_INBOUND,
        (channel::INSTAGRAM, direction::OUTBOUND) => communications::channel::INSTAGRAM_OUTBOUND,
        (channel::INSTAGRAM, _) => communications::channel::INSTAGRAM_INBOUND,
        (channel::THREADS, direction::OUTBOUND) => communications::channel::THREADS_OUTBOUND,
        (channel::THREADS, _) => communications::channel::THREADS_INBOUND,
        (channel::TIKTOK, direction::OUTBOUND) => communications::channel::TIKTOK_OUTBOUND,
        (channel::TIKTOK, _) => communications::channel::TIKTOK_INBOUND,
        (channel::YOUTUBE, direction::OUTBOUND) => communications::channel::YOUTUBE_OUTBOUND,
        (channel::YOUTUBE, _) => communications::channel::YOUTUBE_INBOUND,
        (channel::X, direction::OUTBOUND) => communications::channel::X_OUTBOUND,
        (channel::X, _) => communications::channel::X_INBOUND,
        (channel::LINKEDIN, direction::OUTBOUND) => communications::channel::LINKEDIN_OUTBOUND,
        (channel::LINKEDIN, _) => communications::channel::LINKEDIN_INBOUND,
        _ => "",
    }
}

async fn messages_for(db: &SurrealDb, conversation_id: Uuid) -> Result<Vec<Message>, InboxError> {
    let mut response = db
        .query(format!(
            "SELECT {MESSAGE_SELECT} FROM {MESSAGE} WHERE conversation_id = $conversation \
             ORDER BY occurred_at, id"
        ))
        .bind(("conversation", record_id(CONVERSATION, conversation_id)))
        .await?
        .check()?;
    let rows: Vec<MessageRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(MessageRow::into_message)
        .collect())
}

async fn messages_in_state(db: &SurrealDb, state: &str) -> Result<Vec<Message>, InboxError> {
    let mut response = db
        .query(format!(
            "SELECT {MESSAGE_SELECT} FROM {MESSAGE} WHERE delivery_state = $state"
        ))
        .bind(("state", state.to_string()))
        .await?
        .check()?;
    let rows: Vec<MessageRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(MessageRow::into_message)
        .collect())
}

async fn set_delivery(
    db: &SurrealDb,
    message_id: Uuid,
    state: &str,
    provider_ref: Option<&str>,
) -> Result<(), InboxError> {
    db.query(
        "UPDATE $id SET delivery_state = $state, provider_ref = $provider, updated_at = time::now()",
    )
    .bind(("id", record_id(MESSAGE, message_id)))
    .bind(("state", state.to_string()))
    .bind(("provider", provider_ref.map(str::to_string)))
    .await?
    .check()?;
    after_write(db, MirrorKey::InboxMessage(message_id)).await;
    Ok(())
}

async fn reserve_dispatch(db: &SurrealDb, message_id: Uuid) -> Result<bool, InboxError> {
    #[derive(SurrealValue)]
    struct Claimed {
        id: surrealdb::types::RecordId,
    }
    let mut response = db.query("UPDATE $id SET delivery_state = 'unknown', updated_at = time::now() WHERE direction = 'outbound' AND delivery_state = 'queued' RETURN id")
        .bind(("id", record_id(MESSAGE, message_id))).await?.check()?;
    let claimed: Vec<Claimed> = response.take(0)?;
    let claimed = claimed
        .iter()
        .any(|row| record_uuid(&row.id) == Some(message_id));
    if claimed {
        after_write(db, MirrorKey::InboxMessage(message_id)).await;
    }
    Ok(claimed)
}

async fn journal_attempt(
    db: &SurrealDb,
    message_id: Uuid,
    state: &str,
    provider_ref: Option<&str>,
    detail: Option<&str>,
) -> Result<(), InboxError> {
    let id = Uuid::now_v7();
    db.query(
        "CREATE $id SET message_id = $message, state = $state, provider_ref = $provider, \
         detail = $detail, inserted_at = time::now()",
    )
    .bind(("id", record_id(ATTEMPT, id)))
    .bind(("message", record_id(MESSAGE, message_id)))
    .bind(("state", state.to_string()))
    .bind(("provider", provider_ref.map(str::to_string)))
    .bind(("detail", detail.map(str::to_string)))
    .await?
    .check()?;
    after_write(db, MirrorKey::InboxDeliveryAttempt(id)).await;
    Ok(())
}

async fn find_by_provider_ref(
    db: &SurrealDb,
    firm_id: Uuid,
    channel_name: &str,
    provider_account: &str,
    provider_ref: &str,
) -> Result<Option<Message>, InboxError> {
    let mut response = db
        .query(format!(
            "SELECT {MESSAGE_SELECT} FROM {MESSAGE} WHERE provider_ref = $provider"
        ))
        .bind(("provider", provider_ref.to_string()))
        .await?
        .check()?;
    let rows: Vec<MessageRow> = response.take(0)?;
    for message in rows.into_iter().filter_map(MessageRow::into_message) {
        let Some(conversation) = conversation_by_id(db, message.conversation_id).await? else {
            continue;
        };
        if conversation.firm_id == firm_id
            && conversation.channel == channel_name
            && conversation.provider_account == provider_account
        {
            return Ok(Some(message));
        }
    }
    Ok(None)
}

async fn find_by_email_conversation(
    db: &SurrealDb,
    email_conversation_id: Uuid,
) -> Result<Option<Conversation>, InboxError> {
    let mut response = db
        .query(format!(
            "SELECT {CONVERSATION_SELECT} FROM {CONVERSATION} \
             WHERE email_conversation_id = $email LIMIT 1"
        ))
        .bind((
            "email",
            record_id("email_conversation", email_conversation_id),
        ))
        .await?
        .check()?;
    let rows: Vec<ConversationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .find_map(ConversationRow::into_conversation))
}

async fn all_conversations(db: &SurrealDb) -> Result<Vec<Conversation>, InboxError> {
    let mut response = db
        .query(format!(
            "SELECT {CONVERSATION_SELECT} FROM {CONVERSATION} ORDER BY updated_at DESC"
        ))
        .await?
        .check()?;
    let rows: Vec<ConversationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(ConversationRow::into_conversation)
        .collect())
}

async fn latest_outbound_state(
    db: &SurrealDb,
    conversation_id: Uuid,
) -> Result<String, InboxError> {
    let messages = messages_for(db, conversation_id).await?;
    Ok(messages
        .iter()
        .rev()
        .find(|message| message.direction == direction::OUTBOUND)
        .map_or_else(
            || delivery::RECEIVED.to_string(),
            |message| message.delivery_state.clone(),
        ))
}

/// The direction of a conversation's most recent message (`occurred_at, id`
/// order), empty when it has none — what the list view's `?direction=`
/// filter matches rows on (LAW-238 D3).
fn latest_direction(messages: &[Message]) -> String {
    messages
        .last()
        .map(|message| message.direction.clone())
        .unwrap_or_default()
}

/// Whether any message on the conversation carries a filed attachment —
/// what the list view's `?attachments=true` filter matches rows on.
fn has_attachments(messages: &[Message]) -> bool {
    messages
        .iter()
        .any(|message| message.attachment_asset_id.is_some())
}

/// Whether a firm-side Person sent any message on the conversation — what
/// the Sent folder lists. Automated acknowledgements carry no author.
fn has_sent(messages: &[Message]) -> bool {
    messages.iter().any(is_sent)
}

fn is_sent(message: &Message) -> bool {
    message.direction == direction::OUTBOUND && message.author_person_id.is_some()
}

/// How much of a message body a list row previews.
pub(crate) const PREVIEW_CHAR_LIMIT: usize = 120;

/// The preview snippet for a conversation's most recent written message,
/// under the same `occurred_at, id` convention as [`latest_direction`]. An
/// automated outbound message (no author) is skipped.
pub(crate) fn latest_preview(messages: &[Message]) -> Option<String> {
    let message = messages
        .iter()
        .rev()
        .find(|message| message.direction != direction::OUTBOUND || is_sent(message))?;
    preview_snippet(&message.body)
}

/// One message body's row-preview text: whitespace collapsed to single
/// spaces, then ellipsized past [`PREVIEW_CHAR_LIMIT`]. A blank body
/// previews as nothing.
pub(crate) fn preview_snippet(body: &str) -> Option<String> {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(if trimmed.chars().count() <= PREVIEW_CHAR_LIMIT {
        trimmed.to_string()
    } else {
        format!(
            "{}\u{2026}",
            trimmed.chars().take(PREVIEW_CHAR_LIMIT).collect::<String>()
        )
    })
}

/// The conversations' persisted sender associations as Person badges,
/// batched into one lookup. Conversations without a persisted association
/// badge as nothing.
pub(crate) async fn person_badges(
    db: &SurrealDb,
    conversations: &[Conversation],
) -> Result<HashMap<Uuid, PersonBadge>, InboxError> {
    let ids: Vec<Uuid> = conversations
        .iter()
        .filter_map(|conversation| conversation.person_id)
        .collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let people = persons::find_by_ids(db, &ids)
        .await
        .map_err(|error| InboxError::Db(error.to_string()))?;
    Ok(people
        .into_iter()
        .map(|person| {
            (
                person.id,
                PersonBadge {
                    person_id: person.id,
                    name: person.name,
                },
            )
        })
        .collect())
}

pub(crate) async fn assignee_label(
    db: &SurrealDb,
    project_id: Option<Uuid>,
) -> Result<String, InboxError> {
    let Some(project_id) = project_id else {
        return Ok("Unassigned".to_string());
    };
    Ok(projects::find_by_id(db, project_id)
        .await?
        .map_or_else(|| "Unassigned".to_string(), |project| project.name))
}

pub async fn projects_in_firm(
    db: &SurrealDb,
    firm_id: Uuid,
) -> Result<Vec<ProjectChoice>, InboxError> {
    let mut response = db
        .query("SELECT id, code, name FROM project WHERE firm_id = $firm ORDER BY name")
        .bind(("firm", record_id(crate::firms::TABLE, firm_id)))
        .await?
        .check()?;
    let rows: Vec<ProjectPick> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some(ProjectChoice {
                id: record_uuid(&row.id)?,
                code: row.code,
                name: row.name,
            })
        })
        .collect())
}

async fn mark_read(db: &SurrealDb, conversation_id: Uuid) -> Result<(), InboxError> {
    db.query("UPDATE $id SET unread = false, updated_at = time::now()")
        .bind(("id", record_id(CONVERSATION, conversation_id)))
        .await?
        .check()?;
    after_write(db, MirrorKey::InboxConversation(conversation_id)).await;
    Ok(())
}

/// Shared transport handle for callers that dispatch after a request returns.
pub type SharedTransport = Arc<dyn OutboundTransport>;

/// Messages for an already-authorized reporting Conversation, from its active
/// authority. The caller must resolve visibility before supplying this ID.
pub(crate) async fn messages_for_reporting(
    db: &SurrealDb,
    id: Uuid,
) -> Result<Vec<Message>, InboxError> {
    if let Some(pg) = inbox_authority().await? {
        return crate::postgres::inbox::messages_for_on(&pg, id).await;
    }
    messages_for(db, id).await
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::firms::{self, FirmMembership, NewFirm, NewPersonFirmRole};
    use crate::persons::{self, NewPerson};
    use crate::projects::{self, NewProject};
    use crate::test_support::{mem_surreal, seed_entity};

    struct RecordingTransport {
        calls: Mutex<Vec<OutboundEnvelope>>,
        outcome: Mutex<TransportOutcome>,
    }

    impl RecordingTransport {
        fn new(outcome: TransportOutcome) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                outcome: Mutex::new(outcome),
            }
        }

        fn calls(&self) -> Vec<OutboundEnvelope> {
            self.calls.lock().expect("transport calls").clone()
        }
    }

    #[async_trait]
    impl OutboundTransport for RecordingTransport {
        async fn deliver(&self, envelope: &OutboundEnvelope) -> Result<TransportOutcome, String> {
            self.calls
                .lock()
                .expect("transport calls")
                .push(envelope.clone());
            Ok(self.outcome.lock().expect("transport outcome").clone())
        }
    }

    struct World {
        firm: Uuid,
        other_firm: Uuid,
        project: Uuid,
        other_project: Uuid,
        foreign_project: Uuid,
        owner: Uuid,
        admin: Uuid,
        other_admin: Uuid,
        lawyer: Uuid,
        clerk: Uuid,
        client: Uuid,
        outsider: Uuid,
        clerk_participation: Uuid,
    }

    async fn world(db: &SurrealDb) -> World {
        let owner = person(db, "Owner", "owner@example.com", Role::Owner).await;
        let admin = person(db, "Admin", "admin@example.com", Role::Admin).await;
        let other_admin = person(db, "Other Admin", "other-admin@example.com", Role::Admin).await;
        let lawyer = person(db, "Lawyer", "lawyer@example.com", Role::Lawyer).await;
        let clerk = person(db, "Clerk", "clerk@example.com", Role::Clerk).await;
        let client = person(db, "Client", "client@example.com", Role::Client).await;
        let outsider = person(db, "Outsider", "outsider@example.com", Role::Lawyer).await;
        let firm = open_firm(db, "Practice", admin).await;
        let other_firm = open_firm(db, "Other Practice", other_admin).await;
        for (person_id, firm_id, membership, is_dri) in [
            (lawyer, firm, FirmMembership::Lawyer, false),
            (clerk, firm, FirmMembership::Clerk, false),
            (other_admin, other_firm, FirmMembership::Admin, true),
        ] {
            firms::ensure_membership(
                db,
                &NewPersonFirmRole {
                    person_id,
                    firm_id,
                    membership,
                    is_dri,
                },
            )
            .await
            .unwrap();
        }
        let project = open_project(db, "alpha-matter", "Alpha Matter", firm).await;
        let other_project = open_project(db, "beta-matter", "Beta Matter", firm).await;
        let foreign_project =
            open_project(db, "foreign-matter", "Foreign Matter", other_firm).await;
        projects::designate_dri_in_surreal(db, project, lawyer, projects::DriSide::Lawyer)
            .await
            .unwrap();
        projects::designate_dri_in_surreal(db, other_project, lawyer, projects::DriSide::Lawyer)
            .await
            .unwrap();
        let clerk_participation = projects::add_participation(
            db,
            project,
            clerk,
            projects::participation_for_role(Role::Clerk),
        )
        .await
        .unwrap();
        projects::add_participation(
            db,
            other_project,
            clerk,
            projects::participation_for_role(Role::Clerk),
        )
        .await
        .unwrap();
        projects::add_participation(
            db,
            project,
            client,
            projects::participation_for_role(Role::Client),
        )
        .await
        .unwrap();
        World {
            firm,
            other_firm,
            project,
            other_project,
            foreign_project,
            owner,
            admin,
            other_admin,
            lawyer,
            clerk,
            client,
            outsider,
            clerk_participation: clerk_participation.id,
        }
    }

    async fn person(db: &SurrealDb, name: &str, email: &str, role: Role) -> Uuid {
        persons::create(db, &NewPerson::with_role(name, email, role))
            .await
            .unwrap()
            .id
    }

    async fn open_firm(db: &SurrealDb, name: &str, admin_id: Uuid) -> Uuid {
        firms::create(
            db,
            &NewFirm {
                name: name.into(),
                status: "active".into(),
                entity_id: seed_entity(db).await,
                admin_dri_person_id: admin_id,
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn open_project(db: &SurrealDb, code: &str, name: &str, firm_id: Uuid) -> Uuid {
        projects::create(
            db,
            &NewProject {
                code: code.into(),
                name: name.into(),
                status: "open".into(),
                entity_id: seed_entity(db).await,
                firm_id: Some(firm_id),
                ..NewProject::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    fn inbound<'a>(
        firm: Uuid,
        channel_name: &'a str,
        account: &'a str,
        provider_message_id: &'a str,
        address: &'a str,
    ) -> IngestInbound<'a> {
        IngestInbound {
            project_id: None,
            firm_id: firm,
            brand_key: None,
            interaction_type: None,
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: channel_name,
            provider_account: account,
            provider_message_id,
            provider_thread_ref: None,
            external_address: address,
            external_name: Some("Sender"),
            subject: Some("Question"),
            body: "Hello from outside.",
            source_page: Some("/contact"),
            source_channel: Some(channel_name),
            matter_consent: true,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachments: Vec::new(),
            occurred_at: "2026-09-27T00:00:00Z",
            raw_storage_key: None,
        }
    }

    fn attachment(asset_id: Uuid, filename: &str, sha: &str) -> IngestAttachment {
        IngestAttachment {
            asset_id,
            filename: filename.to_string(),
            sha256: sha.to_string(),
        }
    }

    #[tokio::test]
    async fn attachments_ingest_as_ordered_assets() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let mut message = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-attachments-1",
            "client@example.com",
        );
        message.attachments = vec![
            attachment(first, "complaint.pdf", "sha-complaint"),
            attachment(second, "contract.docx", "sha-contract"),
        ];
        let ingested = ingest_inbound(&db, &message).await.unwrap();
        assert!(!ingested.deduped);

        let linked = message_assets(&db, world.firm, ingested.message_id)
            .await
            .unwrap();
        assert_eq!(linked.len(), 2);
        assert_eq!(linked[0].asset_id, first);
        assert_eq!(linked[0].filename, "complaint.pdf");
        assert_eq!(linked[0].position, 0);
        assert_eq!(linked[0].sha256, "sha-complaint");
        assert_eq!(linked[1].asset_id, second);
        assert_eq!(linked[1].position, 1);

        let stored = message_by_id(&db, ingested.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.attachment_asset_id, Some(first));
    }

    #[tokio::test]
    async fn an_alias_sender_resolves_to_the_mailbox_holder() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        persons::add_alias(&db, world.client, "client-alias@example.com")
            .await
            .unwrap();
        let message = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "alias-sender-1",
            "client-alias@example.com",
        );
        let ingested = ingest_inbound(&db, &message).await.unwrap();
        let conversation = conversation_by_id(&db, ingested.conversation_id)
            .await
            .unwrap()
            .unwrap();
        // The alias sender IS the client: the person_mailbox claim resolves
        // where the primary-email lookup missed, and the link persists.
        assert_eq!(conversation.person_id, Some(world.client));
        let contact = sender_identity(&db, &conversation).await.unwrap().unwrap();
        assert_eq!(contact.person_id, world.client);
    }

    #[tokio::test]
    async fn a_lawyer_or_clerk_sender_never_persists_a_person_link() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        for (role, address) in [
            (world.lawyer, "lawyer@example.com"),
            (world.clerk, "clerk@example.com"),
        ] {
            let message_id = format!("lawyer-sender-{role}");
            let message = inbound(
                world.firm,
                channel::EMAIL,
                "fixture-account",
                &message_id,
                address,
            );
            let ingested = ingest_inbound(&db, &message).await.unwrap();
            let conversation = conversation_by_id(&db, ingested.conversation_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                conversation.person_id, None,
                "outgoing mail never auto-associates"
            );
        }
    }

    /// Counts Person rows holding an exact email — the auto-create dedup
    /// probe for the tests below.
    async fn person_count(db: &SurrealDb, email: &str) -> i64 {
        let mut response = db
            .query("SELECT VALUE count() FROM person WHERE email = $email GROUP ALL")
            .bind(("email", email))
            .await
            .expect("person count query")
            .check()
            .expect("person count check");
        // `SELECT VALUE count()` yields bare numbers — one per GROUP ALL
        // group, and no row at all when nothing matches, which is zero.
        let counts: Vec<i64> = response.take(0).expect("person count decode");
        counts.first().copied().unwrap_or(0)
    }

    #[tokio::test]
    async fn an_unknown_sender_persists_a_created_person_and_one_relationship_log() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let message = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "unknown-sender-1",
            "new-counterparty@example.com",
        );
        let ingested = ingest_inbound(&db, &message).await.unwrap();
        let conversation = conversation_by_id(&db, ingested.conversation_id)
            .await
            .unwrap()
            .unwrap();
        // Exactly one Person appears: named from the RFC From display name,
        // the sender's address as primary email, client-tier by default.
        let created = persons::find_by_any_email(&db, "new-counterparty@example.com")
            .await
            .unwrap()
            .expect("created person");
        assert_eq!(conversation.person_id, Some(created.id));
        assert_eq!(person_count(&db, "new-counterparty@example.com").await, 1);
        assert_eq!(created.name, "Sender");
        assert_eq!(created.role, Role::Client);
        // One Relationship Log entry — the lawyer-acknowledged feed — and
        // never a typed relationship edge.
        let logs = crate::relationship_logs::all(&db).await.unwrap();
        let associations: Vec<_> = logs
            .iter()
            .filter(|log| log.action == INBOUND_ASSOCIATION_ACTION)
            .collect();
        assert_eq!(associations.len(), 1);
        assert_eq!(associations[0].subject_type, "person");
        assert_eq!(associations[0].subject_id, created.id);
    }

    #[tokio::test]
    async fn a_reingest_from_the_same_address_creates_no_second_person_or_log() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        for message_id in ["unknown-sender-2", "unknown-sender-3"] {
            let message = inbound(
                world.firm,
                channel::EMAIL,
                "fixture-account",
                message_id,
                "repeat-counterparty@example.com",
            );
            let ingested = ingest_inbound(&db, &message).await.unwrap();
            let conversation = conversation_by_id(&db, ingested.conversation_id)
                .await
                .unwrap()
                .unwrap();
            assert!(conversation.person_id.is_some());
        }
        // The resolver dedups: one Person and one Relationship Log entry no
        // matter how many deliveries arrive from the same address.
        assert_eq!(
            person_count(&db, "repeat-counterparty@example.com").await,
            1
        );
        let logs = crate::relationship_logs::all(&db).await.unwrap();
        assert_eq!(
            logs.iter()
                .filter(|log| log.action == INBOUND_ASSOCIATION_ACTION)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_backfill_links_unassociated_conversations_and_reruns_without_effect() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "fixture-account",
                "backfill-1",
                "archived.correspondent@example.com",
            ),
        )
        .await
        .unwrap();
        // A row that predates the association: the ingest above already
        // created the Person, so the backfill must re-resolve the same
        // address, not re-create it.
        db.query("UPDATE $id SET person_id = NONE")
            .bind(("id", record_id(CONVERSATION, ingested.conversation_id)))
            .await
            .unwrap()
            .check()
            .unwrap();

        let linked = backfill_person_links(&db).await.unwrap();
        assert_eq!(linked, 1);
        assert_eq!(
            person_count(&db, "archived.correspondent@example.com").await,
            1
        );
        let conversation = conversation_by_id(&db, ingested.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert!(conversation.person_id.is_some());
        // One Relationship Log entry still: the backfill's association is
        // the same fact the ingest already logged, and the entry dedups.
        let logs = crate::relationship_logs::all(&db).await.unwrap();
        assert_eq!(
            logs.iter()
                .filter(|log| log.action == INBOUND_ASSOCIATION_ACTION)
                .count(),
            1
        );

        // Idempotent: a re-run finds nothing unassociated left.
        let again = backfill_person_links(&db).await.unwrap();
        assert_eq!(again, 0, "a re-run finds nothing left to link");
        assert_eq!(
            person_count(&db, "archived.correspondent@example.com").await,
            1
        );
    }

    #[tokio::test]
    async fn redelivered_attachments_do_not_duplicate_links() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let asset = Uuid::now_v7();
        let message = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-attachments-2",
            "client@example.com",
        );
        let mut redelivery = message.clone();
        redelivery.attachments = vec![attachment(asset, "exhibit.pdf", "sha-exhibit")];
        let first = ingest_inbound(&db, &redelivery).await.unwrap();
        let second = ingest_inbound(&db, &redelivery).await.unwrap();
        assert!(second.deduped);
        assert_eq!(second.message_id, first.message_id);

        // The deduped redelivery returns the original message before any
        // link write, so the link set is unchanged.
        let linked = message_assets(&db, world.firm, first.message_id)
            .await
            .unwrap();
        assert_eq!(linked.len(), 1);

        // A direct re-link of the same message and sha also reuses the row.
        link_message_assets(&db, world.firm, first.message_id, &redelivery.attachments)
            .await
            .unwrap();
        let linked = message_assets(&db, world.firm, first.message_id)
            .await
            .unwrap();
        assert_eq!(linked.len(), 1);
    }

    #[tokio::test]
    async fn attachments_are_firm_scoped() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut message = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-attachments-3",
            "client@example.com",
        );
        message.attachments = vec![attachment(Uuid::now_v7(), "discovery.pdf", "sha-discovery")];
        let ingested = ingest_inbound(&db, &message).await.unwrap();
        let foreign = message_assets(&db, world.other_firm, ingested.message_id)
            .await
            .unwrap();
        assert!(foreign.is_empty());
    }

    /// Scraped notices enter the same ingest door as email and SMS: the
    /// `scraper` channel is supported, the message dedupes on the run-spanning
    /// notice identity, and triage sees it like any other intake.
    #[tokio::test]
    async fn scraper_channel_ingests_and_dedupes_like_other_intake() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut message = inbound(
            world.firm,
            channel::SCRAPER,
            "la-superior",
            "la-superior/0004/4",
            "0004",
        );
        message.attachments = vec![attachment(
            Uuid::now_v7(),
            "DKT 0004 - Notice of Hearing.pdf",
            "sha-dkt",
        )];
        let first = ingest_inbound(&db, &message).await.unwrap();
        assert!(!first.deduped);

        // A retry after a lost acknowledgment returns the original row.
        let second = ingest_inbound(&db, &message).await.unwrap();
        assert!(second.deduped);
        assert_eq!(second.message_id, first.message_id);
    }

    #[tokio::test]
    async fn ingest_time_project_attribution_scopes_thread_matching() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut attributed = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-attributed-1",
            "client@example.com",
        );
        attributed.project_id = Some(world.project);
        let first = ingest_inbound(&db, &attributed).await.unwrap();
        let stored = conversation_by_id(&db, first.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.project_id, Some(world.project));

        // A second attributed delivery joins the attributed thread.
        let mut follow_up = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-attributed-2",
            "client@example.com",
        );
        follow_up.project_id = Some(world.project);
        let second = ingest_inbound(&db, &follow_up).await.unwrap();
        assert_eq!(second.conversation_id, first.conversation_id);
        assert!(!second.deduped);

        // An unattributed delivery from the same address never silently
        // adopts the attributed thread; it starts its own.
        let unattributed = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-attributed-3",
            "client@example.com",
        );
        let third = ingest_inbound(&db, &unattributed).await.unwrap();
        assert_ne!(third.conversation_id, first.conversation_id);
        let own_thread = conversation_by_id(&db, third.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(own_thread.project_id, None);
    }

    async fn assigned_sms(db: &SurrealDb, world: &World) -> Uuid {
        let ingested = ingest_inbound(
            db,
            &inbound(
                world.firm,
                channel::SMS,
                "fixture-account",
                "sms-1",
                "sms:fixture-sender",
            ),
        )
        .await
        .unwrap();
        assign(db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        ingested.conversation_id
    }

    #[tokio::test]
    async fn owner_assigns_inside_the_firm_and_projects_communications() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        let stored = conversation_by_id(&db, ingested.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.source_page.as_deref(), Some("/contact"));
        assert_eq!(stored.source_channel.as_deref(), Some(channel::EMAIL));
        assert!(communications::for_project(&db, world.project)
            .await
            .unwrap()
            .is_empty());

        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        let spine = communications::for_project(&db, world.project)
            .await
            .unwrap();
        assert_eq!(spine.len(), 1);
        assert_eq!(spine[0].body, "Hello from outside.");
        assert_eq!(spine[0].project_id, world.project);
        let audit = assignments(&db, ingested.conversation_id).await.unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].to_project_id, world.project);
        assert!(audit[0].from_project_id.is_none());
    }

    #[tokio::test]
    async fn ordinary_assignment_authorization_uses_source_roles_and_supervised_participation() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        for actor in [world.owner, world.lawyer, world.clerk] {
            require_assignment_authorization(&db, actor, world.firm, world.project)
                .await
                .unwrap();
        }
        for actor in [world.admin, world.client, world.outsider] {
            assert!(matches!(
                require_assignment_authorization(&db, actor, world.firm, world.project).await,
                Err(InboxError::Denied)
            ));
        }
        assert!(matches!(
            require_assignment_authorization(&db, world.owner, world.firm, world.foreign_project)
                .await,
            Err(InboxError::CrossFirm)
        ));
        assert!(firms::memberships_for_person(&db, world.outsider)
            .await
            .unwrap()
            .is_empty());
        projects::add_participation(
            &db,
            world.project,
            world.outsider,
            projects::participation_for_role(Role::Lawyer),
        )
        .await
        .unwrap();
        require_assignment_authorization(&db, world.outsider, world.firm, world.project)
            .await
            .unwrap();
        projects::remove_participation(&db, world.clerk_participation)
            .await
            .unwrap();
        assert!(matches!(
            require_assignment_authorization(&db, world.clerk, world.firm, world.project).await,
            Err(InboxError::Denied)
        ));
    }

    #[tokio::test]
    async fn cross_firm_assignment_is_denied_and_non_owners_cannot_assign() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        let error = assign(
            &db,
            world.owner,
            ingested.conversation_id,
            world.foreign_project,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InboxError::CrossFirm));
        for actor in [world.admin, world.client] {
            let error = assign(&db, actor, ingested.conversation_id, world.project)
                .await
                .unwrap_err();
            assert!(matches!(error, InboxError::Denied), "{actor}");
        }
        for actor in [world.lawyer, world.clerk] {
            assign(&db, actor, ingested.conversation_id, world.project)
                .await
                .unwrap();
        }
        assert!(list_intake(&db, world.admin).await.is_ok());
        assert!(list_intake(&db, world.lawyer).await.is_ok());
    }

    #[tokio::test]
    async fn outsiders_and_clients_cannot_read_an_assigned_thread() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        assert!(
            project_thread(&db, world.outsider, world.project, conversation_id)
                .await
                .is_err()
        );
        assert!(
            project_thread(&db, world.client, world.project, conversation_id)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_participant_lists_the_project_inbox_and_a_client_cannot() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        for actor in [world.client, world.outsider] {
            let error = list_project(&db, actor, world.project).await.unwrap_err();
            assert!(matches!(error, InboxError::Denied), "{actor}");
        }
        let rows = list_project(&db, world.clerk, world.project).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, conversation_id);
        assert_eq!(rows[0].channel, channel::SMS);
        assert_eq!(rows[0].next_action, "Send a reply");
        let lawyer_rows = list_project(&db, world.lawyer, world.project)
            .await
            .unwrap();
        assert_eq!(lawyer_rows.len(), 1);
        assert_eq!(lawyer_rows[0].next_action, "Send a reply");
    }

    #[test]
    fn next_action_names_each_viewers_move() {
        assert_eq!(
            next_action(Role::Owner, false, delivery::RECEIVED, false),
            "Assign to a project"
        );
        assert_eq!(
            next_action(Role::Owner, true, delivery::RECEIVED, false),
            "Reassign"
        );
        assert_eq!(
            next_action(Role::Clerk, true, delivery::UNKNOWN, true),
            "Reconcile delivery"
        );
        assert_eq!(
            next_action(Role::Clerk, true, delivery::SUBMITTED, true),
            "Send a reply"
        );
        assert_eq!(
            next_action(Role::Lawyer, true, delivery::RECEIVED, false),
            "Leave an internal note"
        );
        assert_eq!(
            next_action(Role::Admin, true, delivery::RECEIVED, false),
            "Review"
        );
    }

    #[test]
    fn unknown_channels_do_not_project_as_email() {
        assert_eq!(spine_channel("mastodon", direction::INBOUND), "");
        assert_eq!(spine_channel("unsupported", direction::OUTBOUND), "");
        assert_eq!(
            spine_channel(channel::INSTAGRAM, direction::INBOUND),
            communications::channel::INSTAGRAM_INBOUND
        );
        assert_eq!(
            spine_channel(channel::THREADS, direction::OUTBOUND),
            communications::channel::THREADS_OUTBOUND
        );
        assert_eq!(
            spine_channel(channel::EMAIL, direction::INBOUND),
            communications::channel::EMAIL_INBOUND
        );
        assert_eq!(
            spine_channel(channel::SMS, direction::OUTBOUND),
            communications::channel::SMS_OUTBOUND
        );
    }

    #[tokio::test]
    async fn a_participating_lawyer_can_reply_and_elevated_roles_cannot_send() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        let lawyer = project_thread(&db, world.lawyer, world.project, conversation_id)
            .await
            .unwrap();
        assert!(lawyer.can_note);
        assert!(lawyer.can_send);
        assert_eq!(lawyer.next_action, "Send a reply");
        post_note(&db, world.lawyer, conversation_id, "Private strategy.")
            .await
            .unwrap();
        let reply = queue_reply(
            &db,
            world.lawyer,
            conversation_id,
            "A participating lawyer can reply.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .author_person_id,
            Some(world.lawyer)
        );
        for (actor, body) in [
            (world.owner, "An owner cannot send this."),
            (world.admin, "An admin cannot send this."),
            (world.outsider, "An outsider cannot send this."),
        ] {
            assert!(
                queue_reply(&db, actor, conversation_id, body, purpose::MATTER)
                    .await
                    .is_err(),
                "{actor}"
            );
        }
    }

    #[tokio::test]
    async fn a_lawyer_who_does_not_participate_cannot_reply() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        assert!(queue_reply(
            &db,
            world.outsider,
            conversation_id,
            "Not authorized.",
            purpose::MATTER,
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn a_clerk_send_omits_the_internal_note() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        post_note(&db, world.lawyer, conversation_id, "Private strategy.")
            .await
            .unwrap();
        let clerk = project_thread(&db, world.clerk, world.project, conversation_id)
            .await
            .unwrap();
        assert!(clerk.can_note);
        assert!(clerk.can_send);
        assert_eq!(clerk.next_action, "Send a reply");
        let reply = queue_reply(
            &db,
            world.clerk,
            conversation_id,
            "We received your message.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "provider-1".into(),
        });
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.submitted, 1);
        let calls = transport.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].channel, channel::SMS);
        assert_eq!(calls[0].body, "We received your message.");
        assert!(!calls[0].body.contains("Private strategy"));
        let sent = message_by_id(&db, reply).await.unwrap().unwrap();
        assert_eq!(sent.delivery_state, delivery::SUBMITTED);
        let firm_view = communications::for_project(&db, world.project)
            .await
            .unwrap();
        let client_view = communications::for_project_client_visible(&db, world.project)
            .await
            .unwrap();
        assert!(firm_view.iter().any(|row| row.body == "Private strategy."));
        assert!(client_view
            .iter()
            .all(|row| row.body != "Private strategy."));
        assert!(client_view
            .iter()
            .any(|row| row.body == "We received your message."));
    }

    #[tokio::test]
    async fn reassignment_moves_history_and_revokes_the_old_project() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        assign(
            &db,
            world.owner,
            ingested.conversation_id,
            world.other_project,
        )
        .await
        .unwrap();
        assert!(communications::for_project(&db, world.project)
            .await
            .unwrap()
            .is_empty());
        let moved = communications::for_project(&db, world.other_project)
            .await
            .unwrap();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].body, "Hello from outside.");
        assert!(
            project_thread(&db, world.lawyer, world.project, ingested.conversation_id)
                .await
                .is_err()
        );
        let audit = assignments(&db, ingested.conversation_id).await.unwrap();
        assert_eq!(audit.len(), 2);
        assert_eq!(audit[1].from_project_id, Some(world.project));
        assert_eq!(audit[1].to_project_id, world.other_project);
    }

    #[tokio::test]
    async fn revoked_participation_suppresses_a_queued_send() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        let reply = queue_reply(
            &db,
            world.clerk,
            ingested.conversation_id,
            "Still drafting.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        projects::remove_participation(&db, world.clerk_participation)
            .await
            .unwrap();
        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "should-not-send".into(),
        });
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.suppressed, 1);
        assert!(transport.calls().is_empty());
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::SUPPRESSED
        );
    }

    #[tokio::test]
    async fn revoked_lawyer_participation_suppresses_a_queued_send() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "lawyer-msg",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        let reply = queue_reply(
            &db,
            world.lawyer,
            ingested.conversation_id,
            "Drafted by counsel.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        let lawyer_participation = projects::participations_for_project(&db, world.project)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.person_id == world.lawyer)
            .unwrap();
        projects::remove_participation(&db, lawyer_participation.id)
            .await
            .unwrap();
        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "should-not-send".into(),
        });
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.suppressed, 1);
        assert!(transport.calls().is_empty());
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::SUPPRESSED
        );
    }

    #[tokio::test]
    async fn duplicate_webhooks_do_not_create_a_second_message() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let first = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        let second = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(second.deduped);
        assert_eq!(first.message_id, second.message_id);
        let other_account = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "other-account",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(!other_account.deduped);
        assert_ne!(other_account.message_id, first.message_id);
        let other_firm = ingest_inbound(
            &db,
            &inbound(
                world.other_firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(!other_firm.deduped);
    }

    #[tokio::test]
    async fn tiktok_public_comment_reply_projects_explicit_channels_and_refuses_private_audience() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut input = inbound(
            world.firm,
            channel::TIKTOK,
            "synthetic-account",
            "comment-1",
            "reader",
        );
        input.interaction_type = Some("comment");
        input.visibility = Some("public");
        input.provider_post_id = Some("video-1");
        let received = ingest_inbound(&db, &input).await.unwrap();
        assign(&db, world.lawyer, received.conversation_id, world.project)
            .await
            .unwrap();
        assert!(queue_social_reply(
            &db,
            world.lawyer,
            received.conversation_id,
            "Private response",
            "private"
        )
        .await
        .is_err());
        queue_social_reply(
            &db,
            world.lawyer,
            received.conversation_id,
            "Public response",
            "public",
        )
        .await
        .unwrap();
        for unauthorized in [world.other_admin, world.client, world.outsider] {
            assert!(queue_social_reply(
                &db,
                unauthorized,
                received.conversation_id,
                "Cross-Firm response",
                "public"
            )
            .await
            .is_err());
        }
        input.provider_post_id = None;
        input.provider_message_id = "no-video-comment";
        input.external_address = "another-reader";
        let no_video = ingest_inbound(&db, &input).await.unwrap();
        assert!(queue_social_reply(
            &db,
            world.owner,
            no_video.conversation_id,
            "Missing context",
            "public"
        )
        .await
        .is_err());
        let rows = communications::for_project(&db, world.project)
            .await
            .unwrap();
        assert!(rows
            .iter()
            .any(|row| row.channel == communications::channel::TIKTOK_INBOUND));
        assert!(rows
            .iter()
            .any(|row| row.channel == communications::channel::TIKTOK_OUTBOUND));
    }

    async fn assert_instagram_replies_are_attributed(
        db: &SurrealDb,
        actor: Uuid,
        project_id: Uuid,
        conversation_id: Uuid,
    ) {
        assign(db, actor, conversation_id, project_id)
            .await
            .unwrap();
        for (body, visibility) in [
            ("Public response", "public"),
            ("Private response", "private"),
        ] {
            let reply = queue_social_reply(db, actor, conversation_id, body, visibility)
                .await
                .unwrap();
            assert_eq!(
                message_by_id(db, reply)
                    .await
                    .unwrap()
                    .unwrap()
                    .visibility
                    .as_deref(),
                Some(visibility)
            );
        }
    }

    async fn assert_threads_refuses_private_reply(
        db: &SurrealDb,
        world: &World,
        inbound: &IngestInbound<'_>,
    ) {
        let mut threads_inbound = inbound.clone();
        threads_inbound.channel = channel::THREADS;
        threads_inbound.provider_account = "threads-account";
        threads_inbound.provider_message_id = "thread-reply";
        let received = ingest_inbound(db, &threads_inbound).await.unwrap();
        assign(db, world.lawyer, received.conversation_id, world.project)
            .await
            .unwrap();
        assert!(queue_social_reply(
            db,
            world.lawyer,
            received.conversation_id,
            "Private response",
            "private",
        )
        .await
        .is_err());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn social_comment_keeps_brand_post_thread_and_projects_explicit_channel() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut inbound = inbound(
            world.firm,
            channel::INSTAGRAM,
            "ig-account",
            "comment-1",
            "sender",
        );
        inbound.brand_key = Some("neon");
        inbound.interaction_type = Some("comment");
        inbound.provider_post_id = Some("ig-post-42");
        inbound.provider_thread_ref = Some("ig-thread-9");
        inbound.visibility = Some("public");
        let received = ingest_inbound(&db, &inbound).await.unwrap();
        let conversation = conversation_by_id(&db, received.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(conversation.brand_key.as_deref(), Some("neon"));
        assert_eq!(conversation.provider_post_id.as_deref(), Some("ig-post-42"));
        assert_eq!(
            conversation.provider_thread_ref.as_deref(),
            Some("ig-thread-9")
        );
        assert_eq!(conversation.interaction_type, "comment");
        assert_eq!(
            message_by_id(&db, received.message_id)
                .await
                .unwrap()
                .unwrap()
                .visibility
                .as_deref(),
            Some("public")
        );
        assert_instagram_replies_are_attributed(
            &db,
            world.lawyer,
            world.project,
            received.conversation_id,
        )
        .await;
        assert_threads_refuses_private_reply(&db, &world, &inbound).await;
        let messages = communications::for_project(&db, world.project)
            .await
            .unwrap();
        assert!(messages.iter().any(|message| {
            message.channel == communications::channel::INSTAGRAM_INBOUND
                && message.source_ref.as_deref()
                    == Some(format!("inbox:{}", received.message_id).as_str())
        }));
    }

    #[tokio::test]
    async fn owner_opens_a_text_with_consent_and_replies_in_the_same_thread() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let open = |actor: Uuid, to: &'static str, consent: bool| {
            let db = db.clone();
            let firm = world.firm;
            async move {
                open_sms_conversation(
                    &db,
                    actor,
                    firm,
                    Some("neon"),
                    "+15550100000",
                    to,
                    "Hello from the firm",
                    consent,
                )
                .await
            }
        };

        assert!(matches!(
            open(world.owner, "+15555550123", false).await,
            Err(InboxError::Denied)
        ));
        for bad in ["5555550123", "+0555550123", "+1555abc0123", "+15550100000"] {
            assert!(matches!(
                open(world.owner, bad, true).await,
                Err(InboxError::Denied)
            ));
        }
        assert!(matches!(
            open(world.other_admin, "+15555550123", true).await,
            Err(InboxError::Denied)
        ));
        assert!(matches!(
            open(world.outsider, "+15555550123", true).await,
            Err(InboxError::Denied)
        ));
        assert!(matches!(
            open_sms_conversation(
                &db,
                world.owner,
                world.firm,
                None,
                "+15550100000",
                "+15555550123",
                "   ",
                true,
            )
            .await,
            Err(InboxError::Empty)
        ));

        let (conversation_id, _) = open(world.owner, "+15555550123", true).await.unwrap();
        let (again, _) = open(world.owner, "+15555550123", true).await.unwrap();
        assert_eq!(conversation_id, again);
        let conversation = conversation_by_id(&db, conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(conversation.channel, channel::SMS);
        assert_eq!(conversation.external_address, "+15555550123");
        assert!(conversation.matter_consent);

        queue_sms_reply(&db, world.owner, conversation_id, "Second text")
            .await
            .unwrap();
        assert!(
            queue_sms_reply(&db, world.other_admin, conversation_id, "Cross-Firm")
                .await
                .is_err()
        );
        let detail = intake_detail(&db, world.owner, conversation_id)
            .await
            .unwrap();
        assert!(detail.can_sms_reply);
        assert!(detail.bodies_visible);

        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "SM1".into(),
        });
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.submitted, 3);
        assert_eq!(report.suppressed, 0);
        let calls = transport.calls();
        assert!(calls.iter().all(|call| {
            call.channel == channel::SMS
                && call.account == "+15550100000"
                && call.to == "+15555550123"
        }));
    }

    #[tokio::test]
    async fn brand_scoping_keeps_the_firm_scope_when_narrowing() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut branded = inbound(
            world.firm,
            channel::EMAIL,
            "support",
            "msg-vesta-1",
            "vesta-sender@example.com",
        );
        branded.brand_key = Some("vesta");
        let firm_vesta = ingest_inbound(&db, &branded).await.unwrap();
        let mut other_branded = inbound(
            world.other_firm,
            channel::EMAIL,
            "support",
            "msg-vesta-2",
            "other-sender@example.com",
        );
        other_branded.brand_key = Some("vesta");
        let other_firm_vesta = ingest_inbound(&db, &other_branded).await.unwrap();
        let mut firm_neon = inbound(
            world.firm,
            channel::EMAIL,
            "support",
            "msg-neon-1",
            "neon-sender@example.com",
        );
        firm_neon.brand_key = Some("neon");
        ingest_inbound(&db, &firm_neon).await.unwrap();
        let mut firm_other_brand = inbound(
            world.other_firm,
            channel::EMAIL,
            "support",
            "msg-ob-1",
            "ob-sender@example.com",
        );
        firm_other_brand.brand_key = Some("otherfirm-brand");
        ingest_inbound(&db, &firm_other_brand).await.unwrap();

        // The Owner reads across Firms: one Brand view carries both Firms'
        // rows, and the brand vocabulary is everything visible.
        let scoped = list_intake_in_brand(&db, world.owner, "vesta")
            .await
            .unwrap();
        assert_eq!(scoped.len(), 2);
        assert!(scoped
            .iter()
            .any(|row| row.id == firm_vesta.conversation_id));
        assert!(scoped
            .iter()
            .any(|row| row.id == other_firm_vesta.conversation_id));
        assert_eq!(
            intake_brands(&db, world.owner).await.unwrap(),
            vec![
                "neon".to_string(),
                "otherfirm-brand".to_string(),
                "vesta".to_string()
            ]
        );

        // The firm member sees the same Brand narrowed to their Firms, and
        // never a Brand that only exists in a Firm they cannot see.
        let scoped = list_intake_in_brand(&db, world.lawyer, "vesta")
            .await
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, firm_vesta.conversation_id);
        assert_eq!(scoped[0].firm_id, world.firm);
        assert_eq!(
            intake_brands(&db, world.lawyer).await.unwrap(),
            vec!["neon".to_string(), "vesta".to_string()]
        );

        // A Client reads the matter log, not the inbox — brand views included.
        assert!(matches!(
            list_intake_in_brand(&db, world.client, "vesta")
                .await
                .map(|_| ())
                .unwrap_err(),
            InboxError::Denied
        ));
        assert!(matches!(
            intake_brands(&db, world.client).await.unwrap_err(),
            InboxError::Denied
        ));
    }

    #[tokio::test]
    async fn sender_identity_resolves_each_channel_through_its_address_book() {
        let db = mem_surreal().await;
        let world = world(&db).await;

        // Email resolves by the address a Person records — and an unknown
        // sender grows the CRM: the ingest itself created Pat's Person, so
        // the lookup (not a create) names it here.
        let email = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "email-id-1",
                "pat.sender@example.com",
            ),
        )
        .await
        .unwrap();
        let pat = persons::find_by_any_email(&db, "pat.sender@example.com")
            .await
            .unwrap()
            .expect("the ingest auto-created the unknown sender's Person")
            .id;
        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == email.conversation_id)
            .unwrap();
        assert_eq!(row.contact.as_ref().map(|c| c.person_id), Some(pat));

        // SMS keeps resolving by the phone a Person records.
        let text = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "account",
                "sms-id-1",
                "+15555550123",
            ),
        )
        .await
        .unwrap();
        let sam = person(
            &db,
            "synthetic contact",
            "sam.texter@example.com",
            Role::Client,
        )
        .await;
        link_contact_phone(
            &db,
            world.owner,
            text.conversation_id,
            "sam.texter@example.com",
        )
        .await
        .unwrap();
        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == text.conversation_id)
            .unwrap();
        assert_eq!(row.contact.as_ref().map(|c| c.person_id), Some(sam));

        // A social channel resolves through the identity address book keyed
        // on the provider id the webhook recorded — and until that link
        // exists, the row shows the bare id, not a guess.
        let social = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::FACEBOOK,
                "page-account",
                "fb-msg-1",
                "fb-psid-123",
            ),
        )
        .await
        .unwrap();
        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == social.conversation_id)
            .unwrap();
        assert_eq!(row.contact, None);

        external_identities::link(
            &db,
            pat,
            external_identities::ExternalSystem::Facebook,
            "fb-psid-123",
            Some("Pat on Facebook"),
        )
        .await
        .unwrap();

        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == social.conversation_id)
            .unwrap();
        assert_eq!(row.contact.as_ref().map(|c| c.person_id), Some(pat));
        assert_eq!(
            row.contact.as_ref().map(|c| c.role.as_str()),
            Some("client")
        );
    }

    #[test]
    fn a_preview_collapses_whitespace_and_ellipsizes_past_the_limit() {
        assert_eq!(
            preview_snippet("  Multi\nline \t body  ").as_deref(),
            Some("Multi line body")
        );
        let long = "x".repeat(PREVIEW_CHAR_LIMIT + 5);
        let snippet = preview_snippet(&long).unwrap();
        assert_eq!(snippet.chars().count(), PREVIEW_CHAR_LIMIT + 1);
        assert!(snippet.ends_with('\u{2026}'));
        assert_eq!(preview_snippet("   \n\t ").as_deref(), None);
        assert_eq!(preview_snippet("").as_deref(), None);
    }

    #[tokio::test]
    async fn rows_carry_the_latest_message_preview_and_the_persisted_person() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let first = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "fixture-account",
                "email-preview-1",
                "client@example.com",
            ),
        )
        .await
        .unwrap();
        // A second message on the same conversation moves the preview to it.
        let mut reply = inbound(
            world.firm,
            channel::EMAIL,
            "fixture-account",
            "email-preview-2",
            "client@example.com",
        );
        reply.body = "A later reply.";
        reply.email_conversation_id = Some(first.conversation_id);
        let _ = ingest_inbound(&db, &reply).await.unwrap();

        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == first.conversation_id)
            .unwrap();
        assert_eq!(row.preview.as_deref(), Some("A later reply."));
        let created = persons::find_by_any_email(&db, "client@example.com")
            .await
            .unwrap()
            .expect("ingest resolved the sender to a Person");
        let person = row.person.as_ref().expect("the persisted Person badge");
        assert_eq!(person.person_id, created.id);
        // The fixture already holds a client-tier Person at this address, so
        // ingest resolved to it instead of creating one — the badge mirrors
        // the persisted Person's own name, not the message's display name.
        assert_eq!(person.name, created.name);

        // SMS ingests persist no association, so no badge — even when the
        // phone still resolves a contact at view time.
        let text = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "account",
                "sms-preview-1",
                "+15555550123",
            ),
        )
        .await
        .unwrap();
        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == text.conversation_id)
            .unwrap();
        assert_eq!(row.person, None);
        assert_eq!(row.preview.as_deref(), Some("Hello from outside."));
    }

    #[tokio::test]
    async fn an_acknowledgment_neither_previews_nor_counts_as_sent() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "fixture-account",
                "sms-ack-1",
                "sms:fixture-sender",
            ),
        )
        .await
        .unwrap();
        let conversation_id = ingested.conversation_id;
        assign(&db, world.owner, conversation_id, world.project)
            .await
            .unwrap();
        reserve_acknowledgment(&db, &ingested, "We received your message.")
            .await
            .unwrap()
            .expect("the acknowledgment is reserved");
        let row = |rows: Vec<IntakeRow>| {
            rows.into_iter()
                .find(|row| row.id == conversation_id)
                .unwrap()
        };
        let listed = row(list_intake(&db, world.owner).await.unwrap());
        assert_eq!(listed.preview.as_deref(), Some("Hello from outside."));
        assert!(!listed.sent);

        queue_reply(
            &db,
            world.lawyer,
            conversation_id,
            "A lawyer wrote this.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        let listed = row(list_intake(&db, world.owner).await.unwrap());
        assert_eq!(listed.preview.as_deref(), Some("A lawyer wrote this."));
        assert!(listed.sent);
    }

    #[tokio::test]
    async fn a_sender_identity_never_satisfies_a_dispatch_or_privilege_check() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut incoming = inbound(
            world.firm,
            channel::FACEBOOK,
            "page-account",
            "fb-msg-9",
            "fb-psid-9",
        );
        incoming.interaction_type = Some("comment");
        incoming.visibility = Some("public");
        let saved = ingest_inbound(&db, &incoming).await.unwrap();
        let sender = person(
            &db,
            "synthetic sender",
            "identity-sender@example.com",
            Role::Client,
        )
        .await;

        // Before the link exists: a Lawyer with no participation on the
        // conversation cannot queue a social reply.
        assert!(
            queue_social_reply(&db, world.lawyer, saved.conversation_id, "Reply", "public")
                .await
                .is_err()
        );

        // The address book learns who the sender is...
        external_identities::link(
            &db,
            sender,
            external_identities::ExternalSystem::Facebook,
            "fb-psid-9",
            None,
        )
        .await
        .unwrap();

        // ...and nothing changes about who may act: the link is display
        // data, not authority.
        assert!(
            queue_social_reply(&db, world.lawyer, saved.conversation_id, "Reply", "public")
                .await
                .is_err()
        );
        assert!(
            queue_social_reply(&db, world.owner, saved.conversation_id, "Reply", "public")
                .await
                .is_ok()
        );

        // The privilege rule layer never consults the address book either:
        // the social channel's structural rule stands with the identity
        // present, and no classifier is admitted for it.
        assert_eq!(
            crate::inbox_triage::privilege_label(channel::FACEBOOK, true, None),
            crate::inbox_triage::PrivilegeLabel::NotPrivileged
        );
        assert!(!crate::inbox_triage::classifier_may_run(channel::FACEBOOK));
    }

    #[tokio::test]
    async fn a_text_shows_which_person_owns_the_number_once_it_is_linked() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let text = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "+15550100000",
                "sms-link-1",
                "+15555550123",
            ),
        )
        .await
        .unwrap();
        let unlinked = intake_detail(&db, world.owner, text.conversation_id)
            .await
            .unwrap();
        assert_eq!(unlinked.contact, None);

        let client_email = "pat.client@example.com";
        let pat = person(&db, "Pat Client", client_email, Role::Client).await;
        let linked = link_contact_phone(&db, world.owner, text.conversation_id, client_email)
            .await
            .unwrap();
        assert_eq!(linked.person_id, pat);
        assert_eq!(linked.role, "client");
        assert_eq!(
            persons::find_by_id(&db, pat).await.unwrap().unwrap().phone,
            Some("+15555550123".to_string())
        );

        // The punctuation a person typed years ago still matches the E.164 number.
        let detail = intake_detail(&db, world.owner, text.conversation_id)
            .await
            .unwrap();
        assert_eq!(
            detail.contact.as_ref().map(|c| c.name.as_str()),
            Some("Pat Client")
        );
        let rows = list_intake(&db, world.owner).await.unwrap();
        let row = rows
            .iter()
            .find(|row| row.id == text.conversation_id)
            .unwrap();
        assert_eq!(row.contact.as_ref().map(|c| c.person_id), Some(pat));

        // Linking again is idempotent; a second person cannot claim the number.
        link_contact_phone(&db, world.owner, text.conversation_id, client_email)
            .await
            .unwrap();
        person(&db, "Sam Other", "sam.other@example.com", Role::Client).await;
        assert!(matches!(
            link_contact_phone(
                &db,
                world.owner,
                text.conversation_id,
                "sam.other@example.com"
            )
            .await,
            Err(InboxError::Contact(_))
        ));
    }

    #[tokio::test]
    async fn linking_a_number_refuses_unknown_emails_other_phones_and_outsiders() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let text = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "+15550100000",
                "sms-link-2",
                "+15555550199",
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            link_contact_phone(&db, world.owner, text.conversation_id, "nobody@example.com").await,
            Err(InboxError::Contact(_))
        ));
        let has_phone = person(&db, "Has Phone", "has.phone@example.com", Role::Client).await;
        persons::set_phone(&db, has_phone, Some("(555) 555-0000"))
            .await
            .unwrap();
        assert!(matches!(
            link_contact_phone(
                &db,
                world.owner,
                text.conversation_id,
                "has.phone@example.com"
            )
            .await,
            Err(InboxError::Contact(_))
        ));
        assert!(matches!(
            link_contact_phone(
                &db,
                world.other_admin,
                text.conversation_id,
                "has.phone@example.com"
            )
            .await,
            Err(InboxError::Denied)
        ));
        let email = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "email-3",
                "a@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            link_contact_phone(
                &db,
                world.owner,
                email.conversation_id,
                "has.phone@example.com"
            )
            .await,
            Err(InboxError::Denied)
        ));
    }

    #[tokio::test]
    async fn an_email_conversation_refuses_a_text_reply() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let email = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "email-2",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            queue_sms_reply(&db, world.owner, email.conversation_id, "Hi").await,
            Err(InboxError::Denied)
        ));
    }

    #[tokio::test]
    async fn social_reply_allows_owner_and_own_firm_admin_without_weakening_matter_replies() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        firms::ensure_membership(
            &db,
            &NewPersonFirmRole {
                person_id: world.admin,
                firm_id: world.firm,
                membership: FirmMembership::Admin,
                is_dri: true,
            },
        )
        .await
        .unwrap();
        let brand_key = "social-reply-brand";
        crate::brands::create(
            &db,
            Role::Admin,
            Some(world.admin),
            &crate::brands::NewBrand {
                name: "Social Reply Brand".into(),
                key: brand_key.into(),
                firm_id: Some(world.firm),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        let mut social_inbound = inbound(
            world.firm,
            channel::FACEBOOK,
            "page-1",
            "comment-1",
            "sender-1",
        );
        social_inbound.brand_key = Some(brand_key);
        social_inbound.interaction_type = Some("comment");
        social_inbound.provider_post_id = Some("page-post-1");
        social_inbound.provider_thread_ref = Some("comment-thread-1");
        social_inbound.visibility = Some("public");
        let received = ingest_inbound(&db, &social_inbound).await.unwrap();
        let admin_detail = intake_detail(&db, world.admin, received.conversation_id)
            .await
            .unwrap();
        assert!(admin_detail.bodies_visible);
        assert!(admin_detail.can_social_reply);
        assert_eq!(admin_detail.messages.len(), 1);

        for (actor, body) in [
            (world.owner, "Owner reply"),
            (world.admin, "Firm Admin reply"),
        ] {
            queue_social_reply(&db, actor, received.conversation_id, body, "public")
                .await
                .unwrap();
        }
        assert!(queue_social_reply(
            &db,
            world.other_admin,
            received.conversation_id,
            "Cross-Firm reply",
            "public",
        )
        .await
        .is_err());

        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "social-reply-1".into(),
        });
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.submitted, 2);
        let calls = transport.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| {
            call.provider_post_id.as_deref() == Some("page-post-1")
                && call.reply_to_provider_message_id.as_deref() == Some("comment-1")
                && call.provider_thread_ref.as_deref() == Some("comment-thread-1")
        }));

        let email = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "email-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        assert!(queue_reply(
            &db,
            world.admin,
            email.conversation_id,
            "Admin cannot send a matter email",
            purpose::MATTER,
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn a_direct_message_cannot_queue_a_public_reply() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut incoming = inbound(world.firm, channel::INSTAGRAM, "account", "dm-1", "sender");
        incoming.interaction_type = Some("direct_message");
        incoming.visibility = Some("private");
        let saved = ingest_inbound(&db, &incoming).await.unwrap();
        assert!(
            queue_social_reply(&db, world.owner, saved.conversation_id, "Reply", "public")
                .await
                .is_err()
        );
        assert_eq!(
            messages_for(&db, saved.conversation_id)
                .await
                .unwrap()
                .len(),
            1
        );
        queue_social_reply(&db, world.owner, saved.conversation_id, "Reply", "private")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn comments_and_direct_messages_have_separate_transcripts() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut comment = inbound(
            world.firm,
            channel::INSTAGRAM,
            "account",
            "comment-id",
            "sender",
        );
        comment.interaction_type = Some("comment");
        comment.visibility = Some("public");
        let public = ingest_inbound(&db, &comment).await.unwrap();
        let mut dm = comment;
        dm.interaction_type = Some("direct_message");
        dm.visibility = Some("private");
        dm.provider_message_id = "private-id";
        let private = ingest_inbound(&db, &dm).await.unwrap();
        assert_ne!(public.conversation_id, private.conversation_id);
    }

    #[tokio::test]
    async fn private_history_blocks_both_queueing_and_previously_queued_public_sends() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut incoming = inbound(
            world.firm,
            channel::INSTAGRAM,
            "account",
            "comment-id",
            "sender",
        );
        incoming.interaction_type = Some("comment");
        incoming.visibility = Some("public");
        let saved = ingest_inbound(&db, &incoming).await.unwrap();
        queue_social_reply(
            &db,
            world.owner,
            saved.conversation_id,
            "Synthetic reply",
            "public",
        )
        .await
        .unwrap();
        db.query("UPDATE $id SET visibility = 'private'")
            .bind(("id", record_id(MESSAGE, saved.message_id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(queue_social_reply(
            &db,
            world.owner,
            saved.conversation_id,
            "Synthetic reply",
            "public"
        )
        .await
        .is_err());
        let transport = RecordingTransport::new(TransportOutcome::Unknown);
        assert_eq!(
            dispatch_queued(&db, &transport).await.unwrap().suppressed,
            1
        );
        assert!(transport.calls().is_empty());
    }

    #[tokio::test]
    async fn concurrent_dispatchers_send_one_provider_request() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        queue_reply(
            &db,
            world.clerk,
            conversation_id,
            "Synthetic reply",
            purpose::MATTER,
        )
        .await
        .unwrap();
        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "synthetic-receipt".into(),
        });
        let (first, second) = tokio::join!(
            dispatch_queued(&db, &transport),
            dispatch_queued(&db, &transport)
        );
        assert_eq!(first.unwrap().submitted + second.unwrap().submitted, 1);
        assert_eq!(transport.calls().len(), 1);
    }

    #[tokio::test]
    async fn unknown_delivery_is_not_resent() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        queue_reply(
            &db,
            world.clerk,
            conversation_id,
            "Checking status.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        let transport = RecordingTransport::new(TransportOutcome::Unknown);
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.unknown, 1);
        assert_eq!(transport.calls().len(), 1);
        let again = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(again.unknown, 0);
        assert_eq!(
            transport.calls().len(),
            1,
            "unknown must not transmit again"
        );
        assert!(apply_delivery(
            &db,
            world.firm,
            channel::SMS,
            "fixture-account",
            "missing-ref",
            delivery::DELIVERED,
        )
        .await
        .unwrap()
        .is_none());
    }

    #[tokio::test]
    async fn a_later_callback_can_deliver_unknown_without_regressing() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let conversation_id = assigned_sms(&db, &world).await;
        let reply = queue_reply(
            &db,
            world.clerk,
            conversation_id,
            "Checking status.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        set_delivery(&db, reply, delivery::UNKNOWN, Some("provider-9"))
            .await
            .unwrap();
        apply_delivery(
            &db,
            world.other_firm,
            channel::SMS,
            "fixture-account",
            "provider-9",
            delivery::DELIVERED,
        )
        .await
        .unwrap();
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::UNKNOWN,
            "another firm's callback must not settle this delivery"
        );
        apply_delivery(
            &db,
            world.firm,
            channel::SMS,
            "fixture-account",
            "provider-9",
            delivery::SUBMITTED,
        )
        .await
        .unwrap();
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::UNKNOWN,
            "an earlier submitted callback must not clear unknown"
        );
        apply_delivery(
            &db,
            world.firm,
            channel::SMS,
            "fixture-account",
            "provider-9",
            delivery::DELIVERED,
        )
        .await
        .unwrap();
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::DELIVERED
        );
        apply_delivery(
            &db,
            world.firm,
            channel::SMS,
            "fixture-account",
            "provider-9",
            delivery::SUBMITTED,
        )
        .await
        .unwrap();
        assert_eq!(
            message_by_id(&db, reply)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::DELIVERED,
            "a late callback must not regress delivery"
        );
    }

    #[tokio::test]
    async fn promotional_opt_out_suppresses_only_promotional_sends() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::SMS,
                "fixture-account",
                "sms-1",
                "sms:fixture-sender",
            ),
        )
        .await
        .unwrap();
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        let promotional = queue_reply(
            &db,
            world.clerk,
            ingested.conversation_id,
            "A newsletter.",
            purpose::PROMOTIONAL,
        )
        .await
        .unwrap();
        record_promotional_opt_out(&db, world.firm, channel::SMS, "sms:fixture-sender")
            .await
            .unwrap();
        assert!(queue_reply(
            &db,
            world.clerk,
            ingested.conversation_id,
            "Another newsletter.",
            purpose::PROMOTIONAL,
        )
        .await
        .is_err());
        let matter = queue_reply(
            &db,
            world.clerk,
            ingested.conversation_id,
            "A matter update.",
            purpose::MATTER,
        )
        .await
        .unwrap();
        let transport = RecordingTransport::new(TransportOutcome::Submitted {
            provider_ref: "provider-2".into(),
        });
        dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(
            message_by_id(&db, promotional)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::SUPPRESSED
        );
        assert_eq!(
            message_by_id(&db, matter)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            delivery::SUBMITTED
        );
        assert_eq!(transport.calls().len(), 1);
        assert_eq!(transport.calls()[0].body, "A matter update.");
    }

    #[tokio::test]
    async fn attachment_access_follows_the_reader() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let asset_id = Uuid::now_v7();
        let mut inbound = inbound(
            world.firm,
            channel::EMAIL,
            "support",
            "msg-1",
            "sender@example.com",
        );
        inbound.attachments = vec![attachment(asset_id, "complaint.pdf", "sha-reader")];
        let ingested = ingest_inbound(&db, &inbound).await.unwrap();
        assert_eq!(
            readable_attachment(&db, world.owner, ingested.message_id)
                .await
                .unwrap(),
            Some(asset_id)
        );
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        assert!(readable_attachment(&db, world.owner, ingested.message_id)
            .await
            .is_err());
        assert_eq!(
            readable_attachment(&db, world.lawyer, ingested.message_id)
                .await
                .unwrap(),
            Some(asset_id)
        );
        assert!(readable_attachment(&db, world.client, ingested.message_id)
            .await
            .is_err());
        assert!(
            readable_attachment(&db, world.outsider, ingested.message_id)
                .await
                .is_err()
        );
        let note = post_note(
            &db,
            world.lawyer,
            ingested.conversation_id,
            "Internal file note.",
        )
        .await
        .unwrap();
        db.query("UPDATE $id SET attachment_asset_id = $asset")
            .bind(("id", record_id(MESSAGE, note)))
            .bind(("asset", record_id("asset", asset_id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(readable_attachment(&db, world.client, note).await.is_err());
        assert_eq!(
            readable_attachment(&db, world.clerk, note).await.unwrap(),
            Some(asset_id)
        );
    }

    #[tokio::test]
    async fn assigned_intake_hides_bodies_from_an_owner_who_does_not_participate() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let ingested = ingest_inbound(
            &db,
            &inbound(
                world.firm,
                channel::EMAIL,
                "support",
                "msg-1",
                "sender@example.com",
            ),
        )
        .await
        .unwrap();
        let open = intake_detail(&db, world.owner, ingested.conversation_id)
            .await
            .unwrap();
        assert!(open.bodies_visible);
        assert_eq!(open.messages.len(), 1);
        assert_eq!(open.next_action, "Assign to a project");
        assign(&db, world.owner, ingested.conversation_id, world.project)
            .await
            .unwrap();
        let assigned = intake_detail(&db, world.owner, ingested.conversation_id)
            .await
            .unwrap();
        assert!(!assigned.bodies_visible);
        assert!(assigned.messages.is_empty());
        assert_eq!(assigned.next_action, "Reassign");
        let listed = list_intake(&db, world.owner).await.unwrap();
        assert_eq!(listed[0].assignee, "Alpha Matter");
        assert_eq!(listed[0].channel, channel::EMAIL);
    }

    #[tokio::test]
    async fn a_call_opens_one_thread_per_caller_and_dedups_by_call_sid() {
        fn call(firm: Uuid, call_sid: &str) -> IngestInbound<'_> {
            IngestInbound {
                firm_id: firm,
                project_id: None,
                brand_key: Some("neon"),
                interaction_type: Some("message"),
                provider_post_id: None,
                parent_provider_message_id: None,
                visibility: Some("private"),
                channel: channel::VOICE,
                provider_account: "+15550100000",
                provider_message_id: call_sid,
                provider_thread_ref: None,
                external_address: "+15555550100",
                external_name: None,
                subject: None,
                body: "Call from +15555550100 — completed (42s).",
                source_page: Some("+15550100000"),
                source_channel: Some(channel::VOICE),
                matter_consent: false,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachments: Vec::new(),
                occurred_at: "2026-10-30T18:00:00+00:00",
                raw_storage_key: None,
            }
        }
        let db = mem_surreal().await;
        let world = world(&db).await;
        let first = ingest_inbound(&db, &call(world.firm, "CA1111111"))
            .await
            .unwrap();
        assert!(!first.deduped);
        let again = ingest_inbound(&db, &call(world.firm, "CA1111111"))
            .await
            .unwrap();
        assert!(
            again.deduped,
            "a redelivered CallSid returns the original row"
        );
        assert_eq!(again.message_id, first.message_id);
        assert_eq!(again.conversation_id, first.conversation_id);

        // A later call from the same caller threads onto the open one.
        let second_call = ingest_inbound(&db, &call(world.firm, "CA2222222"))
            .await
            .unwrap();
        assert!(!second_call.deduped);
        assert_eq!(second_call.conversation_id, first.conversation_id);

        let listed = list_intake(&db, world.owner).await.unwrap();
        let row = listed
            .iter()
            .find(|row| row.id == first.conversation_id)
            .unwrap();
        assert_eq!(row.channel, channel::VOICE);
        assert_eq!(row.brand_key.as_deref(), Some("neon"));
    }

    /// The whitelist stays closed: adding the voice lane admits no others.
    #[tokio::test]
    async fn an_unlisted_channel_is_still_denied() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let mut fax = inbound(world.firm, "fax", "+15550100000", "fax-1", "+15555550100");
        fax.brand_key = Some("neon");
        assert!(matches!(
            ingest_inbound(&db, &fax).await,
            Err(InboxError::Denied)
        ));
    }

    #[test]
    fn delivery_does_not_move_backward() {
        assert_eq!(
            advance_delivery(delivery::DELIVERED, delivery::SUBMITTED),
            delivery::DELIVERED
        );
        assert_eq!(
            advance_delivery(delivery::UNKNOWN, delivery::DELIVERED),
            delivery::DELIVERED
        );
        assert_eq!(
            advance_delivery(delivery::UNKNOWN, delivery::SUBMITTED),
            delivery::UNKNOWN
        );
    }

    async fn ingest_for(db: &SurrealDb, firm: Uuid, provider_id: &str) -> Uuid {
        let message = inbound(
            firm,
            channel::EMAIL,
            "process-account",
            provider_id,
            "client@example.com",
        );
        ingest_inbound(db, &message).await.unwrap().message_id
    }

    #[tokio::test]
    async fn unprocessed_inbox_scopes_by_firm_membership() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let firm_message = ingest_for(&db, world.firm, "process-scope-1").await;
        let foreign_message = ingest_for(&db, world.other_firm, "process-scope-2").await;

        let owner_view = unprocessed_inbox(&db, world.owner).await.unwrap();
        let owner_ids: Vec<Uuid> = owner_view.iter().map(|row| row.id).collect();
        assert!(owner_ids.contains(&firm_message));
        assert!(owner_ids.contains(&foreign_message));

        let lawyer_view = unprocessed_inbox(&db, world.lawyer).await.unwrap();
        let lawyer_ids: Vec<Uuid> = lawyer_view.iter().map(|row| row.id).collect();
        assert!(lawyer_ids.contains(&firm_message));
        assert!(!lawyer_ids.contains(&foreign_message));

        assert!(matches!(
            unprocessed_inbox(&db, world.client).await,
            Err(InboxError::Denied)
        ));
    }

    #[tokio::test]
    async fn unprocessed_inbox_lists_only_unprocessed_messages() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let first = ingest_for(&db, world.firm, "process-list-1").await;
        let second = ingest_for(&db, world.firm, "process-list-2").await;
        mark_processed(&db, first, route::NEF, Utc::now(), None)
            .await
            .unwrap();

        let listed = unprocessed_inbox(&db, world.lawyer).await.unwrap();
        let ids: Vec<Uuid> = listed.iter().map(|row| row.id).collect();
        assert!(ids.contains(&second));
        assert!(!ids.contains(&first));
    }

    #[tokio::test]
    async fn unprocessed_inbox_orders_oldest_first() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let first = ingest_for(&db, world.firm, "process-order-1").await;
        let second = ingest_for(&db, world.firm, "process-order-2").await;
        db.query("UPDATE $id SET inserted_at = <datetime>'2026-01-01T00:00:00Z'")
            .bind(("id", record_id(MESSAGE, second)))
            .await
            .unwrap()
            .check()
            .unwrap();

        let listed = unprocessed_inbox(&db, world.lawyer).await.unwrap();
        let ids: Vec<Uuid> = listed.iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![second, first]);
    }

    #[tokio::test]
    async fn mark_processed_records_route_and_time_idempotently() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let message_id = ingest_for(&db, world.firm, "process-mark-1").await;
        let at = Utc::now();
        mark_processed(&db, message_id, route::NEF, at, None)
            .await
            .unwrap();

        let state = process_state(&db, message_id).await.unwrap().unwrap();
        assert_eq!(state.route.as_deref(), Some(route::NEF));
        assert_eq!(state.processed_at, Some(at));

        // A re-run overwrites with the same truth instead of failing.
        mark_processed(&db, message_id, route::CHAMBERS, at, None)
            .await
            .unwrap();
        let state = process_state(&db, message_id).await.unwrap().unwrap();
        assert_eq!(state.route.as_deref(), Some(route::CHAMBERS));

        assert!(matches!(
            mark_processed(&db, message_id, "garbage", at, None).await,
            Err(InboxError::InvalidRoute(_))
        ));
    }

    #[tokio::test]
    async fn messages_written_before_process_fields_read_as_unprocessed() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let message_id = ingest_for(&db, world.firm, "process-historical-1").await;
        // A row written before the process fields existed carries none.
        db.query("UPDATE $id UNSET route, processed_at, nef_outcome")
            .bind(("id", record_id(MESSAGE, message_id)))
            .await
            .unwrap()
            .check()
            .unwrap();

        let state = process_state(&db, message_id).await.unwrap().unwrap();
        assert_eq!(state.processed_at, None);
        assert_eq!(state.route, None);
        assert_eq!(state.nef_outcome, None);

        // And the tolerant read still lets a process run record the route.
        mark_processed(&db, message_id, route::OTHER, Utc::now(), None)
            .await
            .unwrap();
        let state = process_state(&db, message_id).await.unwrap().unwrap();
        assert_eq!(state.route.as_deref(), Some(route::OTHER));
    }

    #[tokio::test]
    async fn process_state_of_a_missing_message_is_none() {
        let db = mem_surreal().await;
        assert!(process_state(&db, Uuid::now_v7()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn process_context_scopes_the_single_message_like_the_backlog() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        let firm_message = ingest_for(&db, world.firm, "process-ctx-1").await;
        let foreign_message = ingest_for(&db, world.other_firm, "process-ctx-2").await;
        mark_processed(&db, firm_message, route::NEF, Utc::now(), None)
            .await
            .unwrap();

        // The Owner sees any firm's message, with its recorded state.
        let owner = process_context(&db, world.owner, firm_message)
            .await
            .unwrap();
        assert_eq!(owner.state.route.as_deref(), Some(route::NEF));
        assert!(!owner.row.body.is_empty());
        assert_eq!(owner.row.external_address, "client@example.com");

        // A firm member sees their own firm's message.
        let member = process_context(&db, world.lawyer, firm_message)
            .await
            .unwrap();
        assert_eq!(member.row.id, firm_message);

        // Another firm's message is not found, never disclosed.
        assert!(matches!(
            process_context(&db, world.lawyer, foreign_message).await,
            Err(InboxError::NotFound)
        ));

        // A client-tier actor is denied.
        assert!(matches!(
            process_context(&db, world.client, firm_message).await,
            Err(InboxError::Denied)
        ));

        // A missing message is not found.
        assert!(matches!(
            process_context(&db, world.owner, Uuid::now_v7()).await,
            Err(InboxError::NotFound)
        ));
    }
    #[tokio::test]
    async fn voice_threads_allow_internal_notes_without_offering_replies() {
        let db = mem_surreal().await;
        let w = world(&db).await;
        let mut input = inbound(
            w.firm,
            channel::VOICE,
            "+12025550100",
            "CAvoice-view-guard",
            "+12025550101",
        );
        input.project_id = Some(w.project);
        let saved = ingest_inbound(&db, &input).await.unwrap();
        for actor in [w.lawyer, w.clerk] {
            let view = project_thread(&db, actor, w.project, saved.conversation_id)
                .await
                .unwrap();
            assert!(view.can_note, "participating firm people may note a call");
            assert!(!view.can_send, "VOICE has no outbound reply transport");
            assert_eq!(view.next_action, "Leave an internal note");
            let rows = list_project(&db, actor, w.project).await.unwrap();
            assert_eq!(
                rows.iter()
                    .find(|r| r.id == saved.conversation_id)
                    .unwrap()
                    .next_action,
                "Leave an internal note"
            );
            let note = post_note(
                &db,
                actor,
                saved.conversation_id,
                "Synthetic call follow-up",
            )
            .await
            .unwrap();
            assert_eq!(
                message_by_id(&db, note).await.unwrap().unwrap().direction,
                direction::INTERNAL
            );
        }
    }

    #[tokio::test]
    async fn voice_reply_refusal_creates_no_pending_outbound_message() {
        let db = mem_surreal().await;
        let w = world(&db).await;
        let mut input = inbound(
            w.firm,
            channel::VOICE,
            "+12025550100",
            "CAvoice-queue-guard",
            "+12025550101",
        );
        input.project_id = Some(w.project);
        let saved = ingest_inbound(&db, &input).await.unwrap();
        for actor in [w.lawyer, w.clerk] {
            assert!(
                matches!(
                    queue_reply(
                        &db,
                        actor,
                        saved.conversation_id,
                        "Synthetic reply",
                        purpose::MATTER
                    )
                    .await,
                    Err(InboxError::Denied)
                ),
                "VOICE replies must be refused before queue creation"
            );
        }
        assert!(
            messages_for(&db, saved.conversation_id)
                .await
                .unwrap()
                .iter()
                .all(|m| m.direction != direction::OUTBOUND),
            "refused call replies leave no pending outbound Messages"
        );
    }
    #[tokio::test]
    async fn existing_voice_outbound_rows_never_reach_the_transport() {
        let db = mem_surreal().await;
        let w = world(&db).await;
        let mut input = inbound(
            w.firm,
            channel::VOICE,
            "+12025550100",
            "CAvoice-pending-guard",
            "+12025550101",
        );
        input.project_id = Some(w.project);
        let saved = ingest_inbound(&db, &input).await.unwrap();
        let mut ids = Vec::new();
        for state in [delivery::FAILED, delivery::QUEUED] {
            ids.push(
                create_message(
                    &db,
                    saved.conversation_id,
                    direction::OUTBOUND,
                    "Synthetic prior reply",
                    Some(w.lawyer),
                    None,
                    None,
                    state,
                    purpose::MATTER,
                    None,
                    None,
                    "2026-11-02T00:00:00Z",
                    None,
                )
                .await
                .unwrap(),
            );
        }
        let transport = RecordingTransport::new(TransportOutcome::Unknown);
        let report = dispatch_queued(&db, &transport).await.unwrap();
        assert_eq!(report.suppressed, 1);
        assert!(
            transport.calls().is_empty(),
            "unsupported call replies never reach a provider"
        );
        for (id, state) in ids
            .into_iter()
            .zip([delivery::FAILED, delivery::SUPPRESSED])
        {
            assert_eq!(
                message_by_id(&db, id)
                    .await
                    .unwrap()
                    .unwrap()
                    .delivery_state,
                state
            );
        }
    }

    /// An inbound message's acknowledgment receipt is its ID with the
    /// sign bit flipped: always distinct from the inbound ID, and
    /// involutive, so a reply to the receipt resolves to the original.
    #[test]
    fn acknowledgment_ids_are_involutive_and_distinct() {
        let inbound = Uuid::now_v7();
        let receipt = acknowledgment_id(inbound);
        assert_ne!(inbound, receipt);
        assert_eq!(acknowledgment_id(receipt), inbound);
    }

    #[test]
    fn e164_requires_a_plus_an_eligible_length_and_no_leading_zero() {
        assert!(is_e164("+15551234567"));
        assert!(is_e164("+33123456789"));
        // Too short, too long, missing `+`, a leading zero, non-digits.
        assert!(!is_e164("+1555123"));
        assert!(!is_e164("+1555123456789012345"));
        assert!(!is_e164("15551234567"));
        assert!(!is_e164("+05551234567"));
        assert!(!is_e164("+1555-1234567"));
        assert!(!is_e164(""));
    }

    #[test]
    fn social_channels_exclude_the_direct_channels() {
        for name in [
            channel::FACEBOOK,
            channel::INSTAGRAM,
            channel::THREADS,
            channel::YOUTUBE,
            channel::TIKTOK,
            channel::X,
            channel::LINKEDIN,
        ] {
            assert!(is_social_channel(name), "{name}");
        }
        for name in [
            channel::SMS,
            channel::VOICE,
            channel::EMAIL,
            channel::SCRAPER,
        ] {
            assert!(!is_social_channel(name), "{name}");
        }
    }

    #[test]
    fn processed_at_rejects_a_non_timestamp_with_the_stored_value_named() {
        let parsed = parse_processed_at("2026-01-15T12:30:00Z").expect("rfc3339");
        assert_eq!(parsed.to_rfc3339(), "2026-01-15T12:30:00+00:00");
        let error = parse_processed_at("yesterday-ish").expect_err("invalid");
        assert!(error.to_string().contains("yesterday-ish"), "{error}");
    }

    #[test]
    fn terminal_delivery_states_stay_terminal_and_the_last_signal_wins() {
        use super::delivery;
        // A terminal state never regresses, whatever arrives later.
        assert_eq!(
            advance_delivery(delivery::FAILED, delivery::DELIVERED),
            delivery::FAILED
        );
        assert_eq!(
            advance_delivery(delivery::SUPPRESSED, delivery::SUBMITTED),
            delivery::SUPPRESSED
        );
        // A non-terminal state adopts the incoming signal, unknown stays
        // unknown while only a submit is known, and an unrecognized signal
        // reads as unknown rather than inventing a state.
        assert_eq!(
            advance_delivery(delivery::SUBMITTED, "carrier-pigeon"),
            delivery::UNKNOWN
        );
        assert_eq!(
            advance_delivery(delivery::RECEIVED, delivery::FAILED),
            delivery::FAILED
        );
    }

    #[tokio::test]
    async fn unread_count_for_project_counts_only_that_matters_unread_threads() {
        let db = mem_surreal().await;
        let world = world(&db).await;
        // Two unread threads on the matter, one elsewhere, and the count
        // read stays a bare-number decode (SELECT VALUE count()).
        for thread in ["sms-a", "sms-b"] {
            let ingested = ingest_inbound(
                &db,
                &inbound(
                    world.firm,
                    channel::SMS,
                    "fixture-account",
                    thread,
                    "sms:fixture-sender",
                ),
            )
            .await
            .unwrap();
            assign(&db, world.owner, ingested.conversation_id, world.project)
                .await
                .unwrap();
        }
        assert_eq!(
            unread_count_for_project(&db, world.project).await.unwrap(),
            2
        );
        assert_eq!(
            unread_count_for_project(&db, world.other_project)
                .await
                .unwrap(),
            0
        );
    }
}
