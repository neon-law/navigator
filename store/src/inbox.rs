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

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::access::can_see_project;
use crate::communications::{self, IngestArgs};
use crate::persons::{self, Role};
use crate::postgres_mirror::{after_write, MirrorKey};
use crate::projects::{self, ProjectStoreError};
use crate::surreal::{record_id, record_uuid, SurrealDb};

const CONVERSATION: &str = "inbox_conversation";
const MESSAGE: &str = "message";
const INGRESS: &str = "inbox_ingress";
const ASSIGNMENT: &str = "inbox_assignment";
const ATTEMPT: &str = "inbox_delivery_attempt";

pub mod channel {
    pub const EMAIL: &str = "email";
    pub const SMS: &str = "sms";
    pub const FACEBOOK: &str = "facebook";
    pub const INSTAGRAM: &str = "instagram";
    pub const THREADS: &str = "threads";
    pub const YOUTUBE: &str = "youtube";
    pub const TIKTOK: &str = "tiktok";
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

#[derive(Debug, Clone)]
pub struct IngestInbound<'a> {
    pub firm_id: Uuid,
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
    pub attachment_asset_id: Option<Uuid>,
    pub occurred_at: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestedInbound {
    pub conversation_id: Uuid,
    pub message_id: Uuid,
    pub deduped: bool,
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
    fn of(person: &persons::Person) -> Self {
        Self {
            person_id: person.id,
            name: person.name.clone(),
            role: person.role.as_str().to_string(),
        }
    }
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
    #[error("that project is in another firm")]
    CrossFirm,
    #[error("empty message")]
    Empty,
    #[error("promotional send is opted out")]
    OptedOut,
    #[error("{0}")]
    Contact(String),
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
    email_conversation_id, inserted_at, updated_at";

const MESSAGE_SELECT: &str =
    "id, conversation_id, parent_message_id, visibility, direction, body, author_person_id, \
    provider_message_id, provider_ref, delivery_state, purpose, attachment_asset_id, \
    communication_id, occurred_at, inserted_at, updated_at";

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
    let channel_supported = matches!(inbound.channel, channel::EMAIL | channel::SMS)
        || is_social_channel(inbound.channel);
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
        let Some(message) = message_by_id(db, existing).await? else {
            return Err(InboxError::NotFound);
        };
        return Ok(IngestedInbound {
            conversation_id: message.conversation_id,
            message_id: message.id,
            deduped: true,
        });
    }

    let conversation = match find_open_thread(db, inbound).await? {
        Some(existing) => existing,
        None => create_conversation(db, inbound).await?,
    };
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
        inbound.attachment_asset_id,
        inbound.occurred_at,
    )
    .await?;
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
            });
        }
        return Err(error);
    }
    let _ = db
        .query("UPDATE $id SET unread = true, updated_at = time::now()")
        .bind(("id", record_id(CONVERSATION, conversation.id)))
        .await;
    after_write(db, MirrorKey::InboxConversation(conversation.id)).await;
    Ok(IngestedInbound {
        conversation_id: conversation.id,
        message_id,
        deduped: false,
    })
}

/// Attribute or reassign a conversation to a Project in the same Firm.
///
/// # Errors
///
/// [`InboxError::Denied`] when the actor is not an Owner or a participating Lawyer/Clerk.
/// [`InboxError::CrossFirm`] when the Project belongs to another Firm.
/// [`InboxError::NotFound`] when the conversation or Project is missing.
pub async fn assign(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
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
    if !matches!(actor.role, Role::Owner)
        && (!matches!(actor.role, Role::Lawyer | Role::Clerk)
            || !sees_project(db, actor_id, actor.role, project_id).await?)
    {
        return Err(InboxError::Denied);
    }
    let from_project_id = conversation.project_id;
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
    )
    .await?;
    let message = message_by_id(db, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    project_message(db, &conversation, &message).await?;
    Ok(message_id)
}

/// The people with a recorded phone, read once when any listed conversation
/// is a text.
async fn phone_directory(
    db: &SurrealDb,
    conversations: &[Conversation],
) -> Result<Vec<persons::Person>, InboxError> {
    if conversations
        .iter()
        .any(|conversation| conversation.channel == channel::SMS)
    {
        Ok(persons::list_with_phone(db).await?)
    } else {
        Ok(Vec::new())
    }
}

fn contact_for(phones: &[persons::Person], conversation: &Conversation) -> Option<ContactLink> {
    if conversation.channel != channel::SMS {
        return None;
    }
    phones
        .iter()
        .find(|person| {
            person
                .phone
                .as_deref()
                .is_some_and(|phone| persons::same_phone(phone, &conversation.external_address))
        })
        .map(ContactLink::of)
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
const SMS_BODY_LIMIT: usize = 1600;

/// A text starts only toward an E.164 number.
fn is_e164(number: &str) -> bool {
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
        attachment_asset_id: None,
        occurred_at: &Utc::now().to_rfc3339(),
    };
    let conversation = match find_open_thread(db, &thread).await? {
        Some(existing) => existing,
        None => create_conversation(db, &thread).await?,
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
        let allowed = match message.author_person_id {
            Some(author_id)
                if is_social_channel(&conversation.channel)
                    || conversation.channel == channel::SMS =>
            {
                // A text is sent by the Owner or the Firm's Brand-content Admin,
                // exactly like a social reply, besides a participating Lawyer or
                // Clerk, so the admin inbox can open and answer text threads.
                social_reply_actor_still_authorized(db, author_id, &conversation).await?
            }
            Some(author_id) => {
                participant_still_may_reply(db, author_id, conversation.project_id).await?
            }
            None => false,
        };
        if !allowed {
            set_delivery(db, message.id, delivery::SUPPRESSED, None).await?;
            journal_attempt(
                db,
                message.id,
                delivery::SUPPRESSED,
                None,
                Some("participation revoked"),
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

fn outbound_envelope(
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
    let mut conversations = all_conversations(db).await?;
    if let Some(firms) = visible_firms {
        conversations.retain(|conversation| firms.contains(&conversation.firm_id));
    }
    let phones = phone_directory(db, &conversations).await?;
    let mut rows = Vec::with_capacity(conversations.len());
    for conversation in conversations {
        let delivery_state = latest_outbound_state(db, conversation.id).await?;
        let assignee = assignee_label(db, conversation.project_id).await?;
        rows.push(IntakeRow {
            contact: contact_for(&phones, &conversation),
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
            updated_at: conversation.updated_at,
        });
    }
    Ok(rows)
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
    let contact = if conversation.channel == channel::SMS {
        persons::find_by_phone(db, &conversation.external_address)
            .await?
            .as_ref()
            .map(ContactLink::of)
    } else {
        None
    };
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
    let can_send = matches!(actor.role, Role::Lawyer | Role::Clerk);
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
    let phones = phone_directory(db, &conversations).await?;
    let mut listed = Vec::new();
    for conversation in conversations {
        let delivery_state = latest_outbound_state(db, conversation.id).await?;
        listed.push(IntakeRow {
            contact: contact_for(&phones, &conversation),
            id: conversation.id,
            firm_id: conversation.firm_id,
            brand_key: conversation.brand_key.clone(),
            project_id: conversation.project_id,
            channel: conversation.channel.clone(),
            assignee: assignee_label(db, conversation.project_id).await?,
            unread: conversation.unread,
            next_action: next_action(actor.role, true, &delivery_state, can_send).to_string(),
            delivery: delivery_state,
            external_address: conversation.external_address,
            external_name: conversation.external_name,
            subject: conversation.subject,
            updated_at: conversation.updated_at,
        });
    }
    Ok(listed)
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
    let mut response = db
        .query(format!("SELECT {CONVERSATION_SELECT} FROM ONLY $id"))
        .bind(("id", record_id(CONVERSATION, id)))
        .await?
        .check()?;
    let row: Option<ConversationRow> = response.take(0)?;
    Ok(row.and_then(ConversationRow::into_conversation))
}

pub async fn message_by_id(db: &SurrealDb, id: Uuid) -> Result<Option<Message>, InboxError> {
    let mut response = db
        .query(format!("SELECT {MESSAGE_SELECT} FROM ONLY $id"))
        .bind(("id", record_id(MESSAGE, id)))
        .await?
        .check()?;
    let row: Option<MessageRow> = response.take(0)?;
    Ok(row.and_then(MessageRow::into_message))
}

// This stable key is part of the receipt's persistent idempotency contract.
fn acknowledgment_id(inbound_message_id: Uuid) -> Uuid {
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
    db.query("UPDATE $id SET delivery_state = 'submitted', provider_ref = $provider, updated_at = time::now()")
        .bind(("id", record_id(MESSAGE, id)))
        .bind(("provider", provider_ref))
        .await?.check()?;
    after_write(db, MirrorKey::InboxMessage(id)).await;
    Ok(())
}

fn advance_delivery(current: &str, incoming: &str) -> &'static str {
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

async fn participant_still_may_reply(
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

fn is_social_channel(channel_name: &str) -> bool {
    matches!(
        channel_name,
        channel::FACEBOOK
            | channel::INSTAGRAM
            | channel::THREADS
            | channel::YOUTUBE
            | channel::TIKTOK
    )
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
async fn social_reply_actor_still_authorized(
    db: &SurrealDb,
    actor_id: Uuid,
    conversation: &Conversation,
) -> Result<bool, InboxError> {
    reply_actor_authorized(db, actor_id, conversation.firm_id, conversation.project_id).await
}

/// The same authority for a Firm and optional Project, so an outbound text can
/// be authorized before its conversation exists.
async fn reply_actor_authorized(
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

async fn sees_project(
    db: &SurrealDb,
    person_id: Uuid,
    role: Role,
    project_id: Uuid,
) -> Result<bool, InboxError> {
    can_see_project(db, Some(person_id), role, project_id)
        .await
        .map_err(InboxError::Db)
}

async fn load_person(db: &SurrealDb, person_id: Uuid) -> Result<persons::Person, InboxError> {
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
) -> Result<Conversation, InboxError> {
    let id = Uuid::now_v7();
    let mut response = db
        .query(format!(
            "CREATE $id SET firm_id = $firm, brand_key = $brand, interaction_type = $interaction_type, \
             provider_post_id = $provider_post_id, project_id = NONE, channel = $channel, \
             external_address = $address, external_name = $name, subject = $subject, \
             unread = true, provider_account = $account, provider_thread_ref = $thread, \
             source_page = $source_page, source_channel = $source_channel, \
             matter_consent = $matter_consent, promotional_opt_in = $promotional, \
             promotional_opt_out = false, email_conversation_id = $email_conversation, \
             inserted_at = time::now(), updated_at = time::now() RETURN {CONVERSATION_SELECT}"
        ))
        .bind(("id", record_id(CONVERSATION, id)))
        .bind(("firm", record_id(crate::firms::TABLE, inbound.firm_id)))
        .bind(("brand", inbound.brand_key.map(str::to_string)))
        .bind(("interaction_type", inbound.interaction_type.unwrap_or("message").to_string()))
        .bind(("provider_post_id", inbound.provider_post_id.map(str::to_string)))
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
) -> Result<Uuid, InboxError> {
    let id = Uuid::now_v7();
    db.query(
        "CREATE $id SET conversation_id = $conversation, direction = $direction, body = $body, \
         visibility = $visibility, alert_state = $alert_state, \
         author_person_id = $author, provider_message_id = $provider_message, \
         provider_ref = $provider_ref, delivery_state = $delivery, purpose = $purpose, \
         attachment_asset_id = $attachment, communication_id = NONE, occurred_at = $occurred, \
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

fn spine_channel(channel_name: &str, direction_value: &str) -> &'static str {
    match (channel_name, direction_value) {
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

async fn assignee_label(db: &SurrealDb, project_id: Option<Uuid>) -> Result<String, InboxError> {
    let Some(project_id) = project_id else {
        return Ok("Unassigned".to_string());
    };
    Ok(projects::find_by_id(db, project_id)
        .await?
        .map_or_else(|| "Unassigned".to_string(), |project| project.name))
}

async fn projects_in_firm(db: &SurrealDb, firm_id: Uuid) -> Result<Vec<ProjectChoice>, InboxError> {
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
            attachment_asset_id: None,
            occurred_at: "2026-09-27T00:00:00Z",
        }
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
        inbound.attachment_asset_id = Some(asset_id);
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
}
