//! PostgreSQL authority for the Inbox slice: Conversation, Message,
//! `inbox_ingress`, `inbox_assignment`, and `inbox_delivery_attempt`, plus the
//! reads the renderers use.
//!
//! Opt-in per deployment through `NAVIGATOR_POSTGRES_INBOX_AUTHORITY`. When
//! enabled, every `store::inbox` entry point dispatches here instead of
//! SurrealDB: ingest, dedup, threading, attribution, replies, notes, SMS,
//! dispatch, delivery callbacks, and the intake/project reads. The dispatcher
//! semantics are unchanged — the Surreal statement flow is ported statement
//! for statement, with one improvement: ingest writes inside a single
//! transaction so a lost ingress race cannot strand an orphan Message, which
//! the Surreal path repaired by deleting after the fact.
//!
//! Two kinds of state deliberately stay on their existing authorities and are
//! read through the Surreal handle the callers already hold: the
//! Communication spine projection (`communications::ingest`) and the
//! person/project/brand authorization lookups. This module writes no new
//! Surreal Inbox rows — once the deployment cuts over, the Surreal Inbox
//! tables go stale by design and the recurring inbox mirror reconciliation
//! must be retired for the slice (`docs/postgres-tooling.md`).

use chrono::Utc;
use uuid::Uuid;

use crate::inbox::{
    self, channel, delivery, direction, purpose, Assignment, ContactLink, Conversation,
    DispatchReport, InboxError, IngestInbound, IngestedInbound, IntakeDetail, IntakeRow, Message,
    ThreadView,
};
use crate::persons::Role;
use crate::postgres::PostgresDb;
use crate::surreal::SurrealDb;

/// Opt-in switch for the postgres-authoritative Inbox slice.
pub(crate) const AUTHORITY_ENV: &str = "NAVIGATOR_POSTGRES_INBOX_AUTHORITY";

#[derive(Debug, thiserror::Error)]
pub(crate) enum InboxAuthorityError {
    #[error("PostgreSQL inbox authority setting must be one of 0, 1, false, or true")]
    Configuration,
    #[error("PostgreSQL is unavailable for inbox authority")]
    Unavailable,
}

/// Strict parse, matching the people authority's accepted values.
pub(crate) fn enabled_from(setting: Option<&str>) -> Result<bool, InboxAuthorityError> {
    match setting {
        None | Some("0" | "false") => Ok(false),
        Some("1" | "true") => Ok(true),
        Some(_) => Err(InboxAuthorityError::Configuration),
    }
}

/// The pool to dispatch to, or `None` when the deployment keeps Surreal.
pub(crate) async fn authority() -> Result<Option<PostgresDb>, InboxAuthorityError> {
    let setting = match std::env::var(AUTHORITY_ENV) {
        Ok(setting) => Some(setting),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => return Err(InboxAuthorityError::Configuration),
    };
    if !enabled_from(setting.as_deref())? {
        return Ok(None);
    }
    PostgresDb::from_env()
        .await
        .map(Some)
        .map_err(|_| InboxAuthorityError::Unavailable)
}

// Used as a `map_err` function pointer throughout the module — taking the
// error by reference would force a closure at every call site.
#[allow(clippy::needless_pass_by_value)]
fn db_error(error: sqlx::Error) -> InboxError {
    InboxError::Db(error.to_string())
}

const CONVERSATION_COLUMNS: &str = "id, firm_id, brand_key, interaction_type, provider_post_id, \
    project_id, channel, external_address, external_name, subject, unread, provider_account, \
    provider_thread_ref, source_page, source_channel, matter_consent, promotional_opt_in, \
    promotional_opt_out, email_conversation_id, inserted_at, updated_at";

/// The read image of a Message: the columns `store::inbox`'s Surreal reads
/// select. The alert trio stays delivery-alert surface, outside the reads
/// this slice serves; the mirror's whole-row parity still compares it.
const MESSAGE_COLUMNS: &str = "id, conversation_id, parent_message_id, visibility, direction, \
    body, author_person_id, provider_message_id, provider_ref, delivery_state, purpose, \
    attachment_asset_id, raw_storage_key, communication_id, occurred_at, inserted_at, updated_at";

const ASSIGNMENT_COLUMNS: &str = "id, conversation_id, firm_id, from_project_id, to_project_id, \
    actor_person_id, inserted_at";

// ---------------------------------------------------------------------------
// Row mappers
// ---------------------------------------------------------------------------

fn conversation(row: &sqlx::postgres::PgRow) -> Result<Conversation, sqlx::Error> {
    use sqlx::Row;
    Ok(Conversation {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        brand_key: row.try_get("brand_key")?,
        interaction_type: row.try_get("interaction_type")?,
        provider_post_id: row.try_get("provider_post_id")?,
        project_id: row.try_get("project_id")?,
        channel: row.try_get("channel")?,
        external_address: row.try_get("external_address")?,
        external_name: row.try_get("external_name")?,
        subject: row.try_get("subject")?,
        unread: row.try_get("unread")?,
        provider_account: row.try_get("provider_account")?,
        provider_thread_ref: row.try_get("provider_thread_ref")?,
        source_page: row.try_get("source_page")?,
        source_channel: row.try_get("source_channel")?,
        matter_consent: row.try_get("matter_consent")?,
        promotional_opt_in: row.try_get("promotional_opt_in")?,
        promotional_opt_out: row.try_get("promotional_opt_out")?,
        email_conversation_id: row.try_get("email_conversation_id")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn message(row: &sqlx::postgres::PgRow) -> Result<Message, sqlx::Error> {
    use sqlx::Row;
    Ok(Message {
        id: row.try_get("id")?,
        conversation_id: row.try_get("conversation_id")?,
        parent_message_id: row.try_get("parent_message_id")?,
        visibility: row.try_get("visibility")?,
        direction: row.try_get("direction")?,
        body: row.try_get("body")?,
        author_person_id: row.try_get("author_person_id")?,
        provider_message_id: row.try_get("provider_message_id")?,
        provider_ref: row.try_get("provider_ref")?,
        delivery_state: row.try_get("delivery_state")?,
        purpose: row.try_get("purpose")?,
        attachment_asset_id: row.try_get("attachment_asset_id")?,
        raw_storage_key: row.try_get("raw_storage_key")?,
        communication_id: row.try_get("communication_id")?,
        occurred_at: row.try_get("occurred_at")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn assignment(row: &sqlx::postgres::PgRow) -> Result<Assignment, sqlx::Error> {
    use sqlx::Row;
    Ok(Assignment {
        id: row.try_get("id")?,
        conversation_id: row.try_get("conversation_id")?,
        firm_id: row.try_get("firm_id")?,
        from_project_id: row.try_get("from_project_id")?,
        to_project_id: row.try_get("to_project_id")?,
        actor_person_id: row.try_get("actor_person_id")?,
        inserted_at: row.try_get("inserted_at")?,
    })
}

// ---------------------------------------------------------------------------
// Ingest — direction-aware
// ---------------------------------------------------------------------------

/// Everything `create_message` writes, so the ingest transaction and the
/// reply/note paths share one INSERT.
struct MessageInsert<'a> {
    conversation_id: Uuid,
    direction: &'a str,
    body: &'a str,
    author_person_id: Option<Uuid>,
    provider_message_id: Option<&'a str>,
    provider_ref: Option<&'a str>,
    delivery_state: &'a str,
    purpose: &'a str,
    visibility: Option<&'a str>,
    attachment_asset_id: Option<Uuid>,
    raw_storage_key: Option<&'a str>,
    occurred_at: &'a str,
}

async fn insert_message(
    executor: impl sqlx::PgExecutor<'_>,
    insert: &MessageInsert<'_>,
) -> Result<Uuid, InboxError> {
    let id = Uuid::now_v7();
    let alert_state = (insert.direction == direction::INBOUND).then_some("pending");
    sqlx::query(
        "INSERT INTO message (id, conversation_id, parent_message_id, visibility, direction, body, \
         author_person_id, provider_message_id, provider_ref, delivery_state, purpose, \
         attachment_asset_id, raw_storage_key, communication_id, occurred_at, alert_state, \
         alert_channel_id, alert_timestamp, inserted_at, updated_at) \
         VALUES ($1, $2, NULL, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NULL, $13, $14, NULL, NULL, \
         now(), now())",
    )
    .bind(id)
    .bind(insert.conversation_id)
    .bind(insert.visibility.map(str::to_string))
    .bind(insert.direction)
    .bind(insert.body)
    .bind(insert.author_person_id)
    .bind(insert.provider_message_id.map(str::to_string))
    .bind(insert.provider_ref.map(str::to_string))
    .bind(insert.delivery_state)
    .bind(insert.purpose)
    .bind(insert.attachment_asset_id)
    .bind(insert.raw_storage_key.map(str::to_string))
    .bind(insert.occurred_at)
    .bind(alert_state)
    .execute(executor)
    .await
    .map_err(db_error)?;
    Ok(id)
}

/// The ingest entry for provider-webhook-shaped deliveries: direction
/// `inbound`, no author, exactly `store::inbox::ingest_inbound`'s contract.
pub(crate) async fn ingest_inbound_on(
    pg: &PostgresDb,
    inbound: &IngestInbound<'_>,
) -> Result<IngestedInbound, InboxError> {
    ingest_message_on(pg, direction::INBOUND, None, inbound).await
}

/// The direction-aware ingest entry. Inbound traffic behaves exactly like
/// `ingest_inbound_on`; lawyer-sent imports (LAW-238 D1) arrive with direction
/// `outbound` and the mailbox owner as author. Delivery is `received` —
/// historical mail is already delivered — and `dispatch_queued` never
/// re-sends it, because only `queued` is dispatched.
// The transactional ingest mirrors the Surreal `ingest_inbound` flow step
// for step; splitting it would hide the ordering that makes the dedup
// race-safe.
#[allow(clippy::too_many_lines)]
pub(crate) async fn ingest_message_on(
    pg: &PostgresDb,
    direction_value: &str,
    author_person_id: Option<Uuid>,
    inbound: &IngestInbound<'_>,
) -> Result<IngestedInbound, InboxError> {
    let channel_supported = matches!(inbound.channel, channel::EMAIL | channel::SMS)
        || inbox::is_social_channel(inbound.channel);
    if !channel_supported
        || !matches!(
            inbound.interaction_type.unwrap_or("message"),
            "message" | "comment" | "direct_message"
        )
    {
        return Err(InboxError::Denied);
    }

    // The pre-check keeps the common replay off the write path; the unique
    // `inbox_ingress` key inside the transaction remains the authority.
    if let Some(message_id) = find_ingress_on(
        pg,
        inbound.firm_id,
        inbound.channel,
        inbound.provider_account,
        inbound.provider_message_id,
    )
    .await?
    {
        return deduped_ingest(pg, message_id).await;
    }

    let mut tx = pg.0.begin().await.map_err(db_error)?;
    let conversation = match find_open_thread_on(&mut *tx, inbound).await? {
        Some(existing) => existing,
        None => create_conversation_on(&mut *tx, inbound).await?,
    };
    if let Some(email_conversation_id) = inbound.email_conversation_id {
        if conversation.email_conversation_id.is_none() {
            sqlx::query(
                "UPDATE inbox_conversation SET email_conversation_id = $2, updated_at = now() \
                 WHERE id = $1",
            )
            .bind(conversation.id)
            .bind(email_conversation_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
    }
    let message_id = insert_message(
        &mut *tx,
        &MessageInsert {
            conversation_id: conversation.id,
            direction: direction_value,
            body: inbound.body,
            author_person_id,
            provider_message_id: Some(inbound.provider_message_id),
            provider_ref: None,
            delivery_state: delivery::RECEIVED,
            purpose: purpose::MATTER,
            visibility: inbound.visibility,
            attachment_asset_id: inbound.attachment_asset_id,
            raw_storage_key: inbound.raw_storage_key,
            occurred_at: inbound.occurred_at,
        },
    )
    .await?;
    if let Some(parent_provider) = inbound.parent_provider_message_id {
        let parent: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM message WHERE conversation_id = $1 AND provider_message_id = $2 \
             LIMIT 1",
        )
        .bind(conversation.id)
        .bind(parent_provider)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        if let Some(parent_id) = parent {
            sqlx::query(
                "UPDATE message SET parent_message_id = $2, updated_at = now() WHERE id = $1",
            )
            .bind(message_id)
            .bind(parent_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
    }
    let recorded = sqlx::query(
        "INSERT INTO inbox_ingress (id, firm_id, channel, provider_account, provider_message_id, \
         message_id, inserted_at) VALUES ($1, $2, $3, $4, $5, $6, now()) \
         ON CONFLICT ON CONSTRAINT inbox_ingress_key DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(inbound.firm_id)
    .bind(inbound.channel)
    .bind(inbound.provider_account)
    .bind(inbound.provider_message_id)
    .bind(message_id)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    if recorded.rows_affected() == 0 {
        // Another delivery of the same provider id won the race; its message
        // is the canonical one. This transaction never committed, so the
        // message and (possibly) conversation written here roll back whole —
        // the Surreal path's delete-then-retry, without the orphan window.
        tx.rollback().await.map_err(db_error)?;
        if let Some(winner) = find_ingress_on(
            pg,
            inbound.firm_id,
            inbound.channel,
            inbound.provider_account,
            inbound.provider_message_id,
        )
        .await?
        {
            return deduped_ingest(pg, winner).await;
        }
        return Err(InboxError::Db(
            "ingress conflict without a readable winner".into(),
        ));
    }
    tx.commit().await.map_err(db_error)?;

    // A new delivery on an existing thread re-flags it unread. The Surreal
    // ingest treats this update as best effort — a stale flag is cosmetic —
    // so a failure here is logged, not fatal.
    if let Err(error) =
        sqlx::query("UPDATE inbox_conversation SET unread = true, updated_at = now() WHERE id = $1")
            .bind(conversation.id)
            .execute(&pg.0)
            .await
    {
        tracing::warn!(error = %error, "inbox unread re-flag failed");
    }
    Ok(IngestedInbound {
        conversation_id: conversation.id,
        message_id,
        deduped: false,
    })
}

async fn deduped_ingest(pg: &PostgresDb, message_id: Uuid) -> Result<IngestedInbound, InboxError> {
    let existing = message_by_id_on(pg, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    Ok(IngestedInbound {
        conversation_id: existing.conversation_id,
        message_id: existing.id,
        deduped: true,
    })
}

async fn find_ingress_on(
    pg: &PostgresDb,
    firm_id: Uuid,
    channel_name: &str,
    provider_account: &str,
    provider_message_id: &str,
) -> Result<Option<Uuid>, InboxError> {
    let message_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT message_id FROM inbox_ingress WHERE firm_id = $1 AND channel = $2 \
         AND provider_account = $3 AND provider_message_id = $4 LIMIT 1",
    )
    .bind(firm_id)
    .bind(channel_name)
    .bind(provider_account)
    .bind(provider_message_id)
    .fetch_optional(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(message_id)
}

async fn find_open_thread_on(
    executor: impl sqlx::PgExecutor<'_>,
    inbound: &IngestInbound<'_>,
) -> Result<Option<crate::inbox::Conversation>, InboxError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONVERSATION_COLUMNS} FROM inbox_conversation WHERE firm_id = $1 \
         AND channel = $2 AND brand_key IS NOT DISTINCT FROM $3 AND provider_account = $4 \
         AND interaction_type = $5 AND ($6::text IS NULL OR provider_thread_ref = $6) \
         AND external_address = $7 ORDER BY inserted_at DESC LIMIT 1"
    )))
    .bind(inbound.firm_id)
    .bind(inbound.channel)
    .bind(inbound.brand_key.map(str::to_string))
    .bind(inbound.provider_account)
    .bind(inbound.interaction_type.unwrap_or("message"))
    .bind(inbound.provider_thread_ref.map(str::to_string))
    .bind(inbound.external_address)
    .fetch_optional(executor)
    .await
    .map_err(db_error)?;
    row.map(|row| conversation(&row))
        .transpose()
        .map_err(db_error)
}

async fn create_conversation_on(
    executor: impl sqlx::PgExecutor<'_>,
    inbound: &IngestInbound<'_>,
) -> Result<crate::inbox::Conversation, InboxError> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO inbox_conversation (id, firm_id, brand_key, interaction_type, provider_post_id, \
         project_id, channel, external_address, external_name, subject, unread, provider_account, \
         provider_thread_ref, source_page, source_channel, matter_consent, promotional_opt_in, \
         promotional_opt_out, email_conversation_id, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, NULL, $6, $7, $8, $9, TRUE, $10, $11, $12, $13, $14, $15, \
         FALSE, $16, now(), now())",
    )
    .bind(id)
    .bind(inbound.firm_id)
    .bind(inbound.brand_key.map(str::to_string))
    .bind(inbound.interaction_type.unwrap_or("message"))
    .bind(inbound.provider_post_id.map(str::to_string))
    .bind(inbound.channel)
    .bind(inbound.external_address)
    .bind(inbound.external_name.map(str::to_string))
    .bind(inbound.subject.map(str::to_string))
    .bind(inbound.provider_account)
    .bind(inbound.provider_thread_ref.map(str::to_string))
    .bind(inbound.source_page.map(str::to_string))
    .bind(inbound.source_channel.map(str::to_string))
    .bind(inbound.matter_consent)
    .bind(inbound.promotional_opt_in)
    .bind(inbound.email_conversation_id)
    .execute(executor)
    .await
    .map_err(db_error)?;
    Ok(crate::inbox::Conversation {
        id,
        firm_id: inbound.firm_id,
        brand_key: inbound.brand_key.map(str::to_string),
        interaction_type: inbound.interaction_type.unwrap_or("message").to_string(),
        provider_post_id: inbound.provider_post_id.map(str::to_string),
        project_id: None,
        channel: inbound.channel.to_string(),
        external_address: inbound.external_address.to_string(),
        external_name: inbound.external_name.map(str::to_string),
        subject: inbound.subject.map(str::to_string),
        unread: true,
        provider_account: inbound.provider_account.to_string(),
        provider_thread_ref: inbound.provider_thread_ref.map(str::to_string),
        source_page: inbound.source_page.map(str::to_string),
        source_channel: inbound.source_channel.map(str::to_string),
        matter_consent: inbound.matter_consent,
        promotional_opt_in: inbound.promotional_opt_in,
        promotional_opt_out: false,
        email_conversation_id: inbound.email_conversation_id,
        inserted_at: Utc::now(),
        updated_at: Utc::now(),
    })
}

// ---------------------------------------------------------------------------
// Attribution and the Communication spine projection
// ---------------------------------------------------------------------------

async fn project_message_on(
    // Unused while the Communication spine is SurrealDB-authoritative; kept
    // in the signature so the spine's own cutover needs no call-site churn.
    _pg: &PostgresDb,
    db: &SurrealDb,
    conversation: &crate::inbox::Conversation,
    message: &Message,
) -> Result<(), InboxError> {
    let Some(project_id) = conversation.project_id else {
        return Ok(());
    };
    if let Some(communication_id) = message.communication_id {
        crate::communications::move_to_project(db, communication_id, project_id).await?;
        return Ok(());
    }
    let source_ref = format!("inbox:{}", message.id);
    let channel_name = inbox::spine_channel(&conversation.channel, &message.direction);
    if channel_name.is_empty() {
        return Err(InboxError::Denied);
    }
    // The spine's (channel, source_ref) unique key is the projection ledger.
    // An already-projected message moves with the matter instead of
    // re-ingesting — `communications::ingest` refuses a source ref that
    // belongs to another Project by design. The PostgreSQL-authoritative
    // Message cannot carry the spine's communication_id back-link (the spine
    // row has no PostgreSQL mirror row in this slice), so this lookup plays
    // the role the Surreal back-link plays there.
    if let Some(existing) =
        crate::communications::find_by_source(db, channel_name, &source_ref).await?
    {
        if existing.project_id != project_id {
            crate::communications::move_to_project(db, existing.id, project_id).await?;
        }
        return Ok(());
    }
    let spine_direction = if message.direction == direction::INTERNAL {
        crate::communications::direction::INTERNAL
    } else if message.direction == direction::OUTBOUND {
        crate::communications::direction::OUTBOUND
    } else {
        crate::communications::direction::INBOUND
    };
    let ingested = crate::communications::ingest(
        db,
        &crate::communications::IngestArgs {
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
    // A concurrent assignment won the ingest race: make sure the row lands
    // on this matter — ingest refuses cross-project source refs, so a
    // deduped hit means the row already exists and only needs moving.
    if ingested.deduped {
        crate::communications::move_to_project(db, ingested.communication_id, project_id).await?;
    }
    Ok(())
}

async fn project_all_messages_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    conversation: &crate::inbox::Conversation,
) -> Result<(), InboxError> {
    for message in messages_for_on(pg, conversation.id).await? {
        project_message_on(pg, db, conversation, &message).await?;
    }
    Ok(())
}

/// Attribute a conversation to a project. Authorization reads stay on the
/// Surreal handle; the conversation row, its messages' back-links, and the
/// immutable audit row are postgres writes.
pub(crate) async fn assign_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    let actor = inbox::load_person(db, actor_id).await?;
    let conversation = conversation_by_id_on(pg, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let project = crate::projects::find_by_id(db, project_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if project.firm_id != Some(conversation.firm_id) {
        return Err(InboxError::CrossFirm);
    }
    if !matches!(actor.role, Role::Owner)
        && (!matches!(actor.role, Role::Lawyer | Role::Clerk)
            || !inbox::sees_project(db, actor_id, actor.role, project_id).await?)
    {
        return Err(InboxError::Denied);
    }
    let from_project_id = conversation.project_id;
    sqlx::query(
        "UPDATE inbox_conversation SET project_id = $2, unread = true, updated_at = now() \
         WHERE id = $1",
    )
    .bind(conversation_id)
    .bind(project_id)
    .execute(&pg.0)
    .await
    .map_err(db_error)?;
    if let Some(email_id) = conversation.email_conversation_id {
        crate::email_conversations::set_scope(
            db,
            email_id,
            Some(conversation.firm_id),
            Some(project_id),
        )
        .await?;
    }
    let updated = conversation_by_id_on(pg, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    project_all_messages_on(pg, db, &updated).await?;
    let assignment_id = Uuid::now_v7();
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO inbox_assignment (id, conversation_id, firm_id, from_project_id, \
         to_project_id, actor_person_id, inserted_at) VALUES ($1, $2, $3, $4, $5, $6, now()) \
         RETURNING {ASSIGNMENT_COLUMNS}"
    )))
    .bind(assignment_id)
    .bind(conversation_id)
    .bind(conversation.firm_id)
    .bind(from_project_id)
    .bind(project_id)
    .bind(actor_id)
    .fetch_one(&pg.0)
    .await
    .map_err(db_error)?;
    assignment(&row).map_err(db_error)
}

// ---------------------------------------------------------------------------
// Replies, notes, and SMS
// ---------------------------------------------------------------------------

// Mirrors the Surreal `create_message` signature — the projection and
// dispatch paths pass the same fields.
#[allow(clippy::too_many_arguments)]
async fn create_message_on(
    pg: &PostgresDb,
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
    insert_message(
        &pg.0,
        &MessageInsert {
            conversation_id,
            direction: direction_value,
            body,
            author_person_id,
            provider_message_id,
            provider_ref,
            delivery_state,
            purpose: purpose_value,
            visibility,
            attachment_asset_id,
            raw_storage_key: None,
            occurred_at,
        },
    )
    .await
}

pub(crate) async fn post_note_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
) -> Result<Uuid, InboxError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(InboxError::Empty);
    }
    let actor = inbox::load_person(db, actor_id).await?;
    if !matches!(actor.role, Role::Lawyer | Role::Clerk) {
        return Err(InboxError::Denied);
    }
    let conversation = conversation_by_id_on(pg, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let Some(project_id) = conversation.project_id else {
        return Err(InboxError::Denied);
    };
    if !inbox::sees_project(db, actor_id, actor.role, project_id).await? {
        return Err(InboxError::Denied);
    }
    let message_id = create_message_on(
        pg,
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
    let message = message_by_id_on(pg, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    project_message_on(pg, db, &conversation, &message).await?;
    Ok(message_id)
}

pub(crate) async fn queue_reply_on(
    pg: &PostgresDb,
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
    let actor = inbox::load_person(db, actor_id).await?;
    if !matches!(actor.role, Role::Lawyer | Role::Clerk) {
        return Err(InboxError::Denied);
    }
    let conversation = conversation_by_id_on(pg, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let Some(project_id) = conversation.project_id else {
        return Err(InboxError::Denied);
    };
    if !inbox::sees_project(db, actor_id, actor.role, project_id).await? {
        return Err(InboxError::Denied);
    }
    if purpose_value == purpose::PROMOTIONAL && conversation.promotional_opt_out {
        return Err(InboxError::OptedOut);
    }
    create_message_on(
        pg,
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

pub(crate) async fn queue_social_reply_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
    visibility: &str,
) -> Result<Uuid, InboxError> {
    let conversation = conversation_by_id_on(pg, conversation_id)
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
    if !inbox::social_reply_actor_still_authorized(db, actor_id, &conversation).await? {
        return Err(InboxError::Denied);
    }
    if visibility == "public"
        && messages_for_on(pg, conversation_id)
            .await?
            .iter()
            .any(|message| {
                message.direction == direction::INBOUND
                    && message.visibility.as_deref() == Some("private")
            })
    {
        return Err(InboxError::Denied);
    }
    let message_id = create_message_on(
        pg,
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
    let message = message_by_id_on(pg, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    project_message_on(pg, db, &conversation, &message).await?;
    Ok(message_id)
}

pub(crate) async fn link_contact_phone_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    email: &str,
) -> Result<ContactLink, InboxError> {
    let conversation = conversation_by_id_on(pg, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.channel != channel::SMS {
        return Err(InboxError::Denied);
    }
    if !inbox::social_reply_actor_still_authorized(db, actor_id, &conversation).await? {
        return Err(InboxError::Denied);
    }
    let person = crate::persons::find_by_email_ci(db, email.trim())
        .await?
        .ok_or_else(|| InboxError::Contact("No person has that email".into()))?;
    if let Some(holder) = crate::persons::find_by_phone(db, &conversation.external_address).await? {
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
    let updated = crate::persons::set_phone(db, person.id, Some(&conversation.external_address))
        .await?
        .ok_or(InboxError::NotFound)?;
    Ok(ContactLink::of(&updated))
}

pub(crate) async fn queue_sms_reply_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
    body: &str,
) -> Result<Uuid, InboxError> {
    let body = body.trim();
    if body.is_empty() || body.chars().count() > inbox::SMS_BODY_LIMIT {
        return Err(InboxError::Empty);
    }
    let conversation = conversation_by_id_on(pg, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.channel != channel::SMS {
        return Err(InboxError::Denied);
    }
    if !inbox::social_reply_actor_still_authorized(db, actor_id, &conversation).await? {
        return Err(InboxError::Denied);
    }
    queue_sms_message_on(pg, db, &conversation, actor_id, body).await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn open_sms_conversation_on(
    pg: &PostgresDb,
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
    if body.is_empty() || body.chars().count() > inbox::SMS_BODY_LIMIT {
        return Err(InboxError::Empty);
    }
    if !recipient_consented || !inbox::is_e164(to) || to == provider_account {
        return Err(InboxError::Denied);
    }
    if !inbox::reply_actor_authorized(db, actor_id, firm_id, None).await? {
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
        raw_storage_key: None,
    };
    let conversation = match find_open_thread_on(&pg.0, &thread).await? {
        Some(existing) => existing,
        None => create_conversation_on(&pg.0, &thread).await?,
    };
    let message_id = queue_sms_message_on(pg, db, &conversation, actor_id, body).await?;
    Ok((conversation.id, message_id))
}

async fn queue_sms_message_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    conversation: &crate::inbox::Conversation,
    actor_id: Uuid,
    body: &str,
) -> Result<Uuid, InboxError> {
    let message_id = create_message_on(
        pg,
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
    let message = message_by_id_on(pg, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    project_message_on(pg, db, conversation, &message).await?;
    Ok(message_id)
}

// ---------------------------------------------------------------------------
// Delivery lifecycle
// ---------------------------------------------------------------------------

/// Send every queued reply, re-checking Lawyer or Clerk participation first.
/// An unknown provider result stays unknown; a later pass does not transmit
/// it again. A single transport failure is recorded on that message and does
/// not abort the rest of the queue.
// The queue walk mirrors the Surreal `dispatch_queued` state machine;
// splitting it would hide the delivery-state transitions.
#[allow(clippy::too_many_lines)]
pub(crate) async fn dispatch_queued_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    transport: &dyn inbox::OutboundTransport,
) -> Result<DispatchReport, InboxError> {
    let queued = messages_in_state_on(pg, delivery::QUEUED).await?;
    let mut report = DispatchReport::default();
    for message in queued {
        let Some(conversation) = conversation_by_id_on(pg, message.conversation_id).await? else {
            continue;
        };
        if message.purpose == purpose::PROMOTIONAL && conversation.promotional_opt_out {
            set_delivery_on(pg, message.id, delivery::SUPPRESSED, None).await?;
            journal_attempt_on(
                pg,
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
                if inbox::is_social_channel(&conversation.channel)
                    || conversation.channel == channel::SMS =>
            {
                inbox::social_reply_actor_still_authorized(db, author_id, &conversation).await?
            }
            Some(author_id) => {
                inbox::participant_still_may_reply(db, author_id, conversation.project_id).await?
            }
            None => false,
        };
        if !allowed {
            set_delivery_on(pg, message.id, delivery::SUPPRESSED, None).await?;
            journal_attempt_on(
                pg,
                message.id,
                delivery::SUPPRESSED,
                None,
                Some("participation revoked"),
            )
            .await?;
            report.suppressed += 1;
            continue;
        }
        let messages = messages_for_on(pg, conversation.id).await?;
        if inbox::is_social_channel(&conversation.channel)
            && message.visibility.as_deref() == Some("public")
            && (conversation.interaction_type == "direct_message"
                || messages.iter().any(|entry| {
                    entry.direction == direction::INBOUND
                        && entry.visibility.as_deref() == Some("private")
                }))
        {
            set_delivery_on(pg, message.id, delivery::SUPPRESSED, None).await?;
            journal_attempt_on(
                pg,
                message.id,
                delivery::SUPPRESSED,
                None,
                Some("private conversation cannot receive a public reply"),
            )
            .await?;
            report.suppressed += 1;
            continue;
        }
        let envelope = inbox::outbound_envelope(&conversation, &message, &messages);
        if !reserve_dispatch_on(pg, message.id).await? {
            continue;
        }
        match transport.deliver(&envelope).await {
            Ok(inbox::TransportOutcome::Submitted { provider_ref }) => {
                set_delivery_on(pg, message.id, delivery::SUBMITTED, Some(&provider_ref)).await?;
                journal_attempt_on(
                    pg,
                    message.id,
                    delivery::SUBMITTED,
                    Some(&provider_ref),
                    None,
                )
                .await?;
                if let Some(fresh) = message_by_id_on(pg, message.id).await? {
                    let _ = project_message_on(pg, db, &conversation, &fresh).await;
                }
                report.submitted += 1;
            }
            Ok(inbox::TransportOutcome::Unknown) => {
                set_delivery_on(pg, message.id, delivery::UNKNOWN, None).await?;
                journal_attempt_on(pg, message.id, delivery::UNKNOWN, None, Some("timeout"))
                    .await?;
                report.unknown += 1;
            }
            Ok(inbox::TransportOutcome::Rejected { detail }) | Err(detail) => {
                set_delivery_on(pg, message.id, delivery::FAILED, None).await?;
                journal_attempt_on(pg, message.id, delivery::FAILED, None, Some(&detail)).await?;
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

async fn set_delivery_on(
    pg: &PostgresDb,
    message_id: Uuid,
    state: &str,
    provider_ref: Option<&str>,
) -> Result<(), InboxError> {
    sqlx::query(
        "UPDATE message SET delivery_state = $2, provider_ref = $3, updated_at = now() \
         WHERE id = $1",
    )
    .bind(message_id)
    .bind(state)
    .bind(provider_ref.map(str::to_string))
    .execute(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// Conditionally claim one queued message for dispatch: the UPDATE only fires
/// while the row is still `queued`, so two workers cannot both send it.
async fn reserve_dispatch_on(pg: &PostgresDb, message_id: Uuid) -> Result<bool, InboxError> {
    let claimed = sqlx::query(
        "UPDATE message SET delivery_state = 'unknown', updated_at = now() \
         WHERE id = $1 AND direction = 'outbound' AND delivery_state = 'queued' RETURNING id",
    )
    .bind(message_id)
    .fetch_optional(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(claimed.is_some())
}

async fn journal_attempt_on(
    pg: &PostgresDb,
    message_id: Uuid,
    state: &str,
    provider_ref: Option<&str>,
    detail: Option<&str>,
) -> Result<(), InboxError> {
    sqlx::query(
        "INSERT INTO inbox_delivery_attempt (id, message_id, state, provider_ref, detail, \
         inserted_at) VALUES ($1, $2, $3, $4, $5, now())",
    )
    .bind(Uuid::now_v7())
    .bind(message_id)
    .bind(state)
    .bind(provider_ref.map(str::to_string))
    .bind(detail.map(str::to_string))
    .execute(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(())
}

async fn find_by_provider_ref_on(
    pg: &PostgresDb,
    firm_id: Uuid,
    channel_name: &str,
    provider_account: &str,
    provider_ref: &str,
) -> Result<Option<Message>, InboxError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {MESSAGE_COLUMNS} FROM message JOIN inbox_conversation \
         ON inbox_conversation.id = message.conversation_id \
         WHERE message.provider_ref = $1 AND inbox_conversation.firm_id = $2 \
         AND inbox_conversation.channel = $3 AND inbox_conversation.provider_account = $4 \
         LIMIT 1"
    )))
    .bind(provider_ref)
    .bind(firm_id)
    .bind(channel_name)
    .bind(provider_account)
    .fetch_optional(&pg.0)
    .await
    .map_err(db_error)?;
    row.map(|row| message(&row)).transpose().map_err(db_error)
}

/// Apply a provider delivery callback scoped by Firm, channel, and account.
/// An unknown provider reference records nothing and does not send. A
/// callback that would move delivery backward is ignored. `unknown` may
/// advance to `delivered`.
pub(crate) async fn apply_delivery_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    firm_id: Uuid,
    channel_name: &str,
    provider_account: &str,
    provider_ref: &str,
    incoming: &str,
) -> Result<Option<Uuid>, InboxError> {
    let Some(found) =
        find_by_provider_ref_on(pg, firm_id, channel_name, provider_account, provider_ref).await?
    else {
        return Ok(None);
    };
    let next = inbox::advance_delivery(&found.delivery_state, incoming);
    if next == found.delivery_state {
        return Ok(Some(found.id));
    }
    set_delivery_on(pg, found.id, next, Some(provider_ref)).await?;
    journal_attempt_on(pg, found.id, next, Some(provider_ref), None).await?;
    if next == delivery::DELIVERED || next == delivery::SUBMITTED {
        if let Some(conversation) = conversation_by_id_on(pg, found.conversation_id).await? {
            if let Some(fresh) = message_by_id_on(pg, found.id).await? {
                let _ = project_message_on(pg, db, &conversation, &fresh).await;
            }
        }
    }
    Ok(Some(found.id))
}

/// Honor a promotional opt-out and suppress queued promotional sends.
pub(crate) async fn record_promotional_opt_out_on(
    pg: &PostgresDb,
    firm_id: Uuid,
    channel_name: &str,
    external_address: &str,
) -> Result<usize, InboxError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONVERSATION_COLUMNS} FROM inbox_conversation \
         WHERE firm_id = $1 AND channel = $2 AND external_address = $3"
    )))
    .bind(firm_id)
    .bind(channel_name)
    .bind(external_address)
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    let mut suppressed = 0usize;
    for row in rows {
        let row = conversation(&row).map_err(db_error)?;
        sqlx::query(
            "UPDATE inbox_conversation SET promotional_opt_out = true, promotional_opt_in = false, \
             updated_at = now() WHERE id = $1",
        )
        .bind(row.id)
        .execute(&pg.0)
        .await
        .map_err(db_error)?;
        for message in messages_for_on(pg, row.id).await? {
            if message.purpose == purpose::PROMOTIONAL && message.delivery_state == delivery::QUEUED
            {
                set_delivery_on(pg, message.id, delivery::SUPPRESSED, None).await?;
                journal_attempt_on(
                    pg,
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

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

pub(crate) async fn conversation_by_id_on(
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Option<crate::inbox::Conversation>, InboxError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONVERSATION_COLUMNS} FROM inbox_conversation WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(&pg.0)
    .await
    .map_err(db_error)?;
    row.map(|row| conversation(&row))
        .transpose()
        .map_err(db_error)
}

pub(crate) async fn message_by_id_on(
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Option<Message>, InboxError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {MESSAGE_COLUMNS} FROM message WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(&pg.0)
    .await
    .map_err(db_error)?;
    row.map(|row| message(&row)).transpose().map_err(db_error)
}

/// Point an imported Message at its first filed attachment Asset (LAW-238
/// D1). The remaining attachments of the same message resolve through
/// `MailProvenance.source_message_id`, so only the first occupies the slot.
/// A no-op when the slot is already set — imports re-run idempotently.
pub(crate) async fn set_attachment_asset_on(
    pg: &PostgresDb,
    message_id: Uuid,
    asset_id: Uuid,
) -> Result<(), InboxError> {
    sqlx::query(
        "UPDATE message SET attachment_asset_id = $2, updated_at = now() \
         WHERE id = $1 AND attachment_asset_id IS NULL",
    )
    .bind(message_id)
    .bind(asset_id)
    .execute(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(())
}

async fn messages_for_on(
    pg: &PostgresDb,
    conversation_id: Uuid,
) -> Result<Vec<Message>, InboxError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {MESSAGE_COLUMNS} FROM message WHERE conversation_id = $1 \
         ORDER BY occurred_at, id"
    )))
    .bind(conversation_id)
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    rows.iter()
        .map(message)
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)
}

async fn messages_in_state_on(pg: &PostgresDb, state: &str) -> Result<Vec<Message>, InboxError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {MESSAGE_COLUMNS} FROM message WHERE delivery_state = $1"
    )))
    .bind(state)
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    rows.iter()
        .map(message)
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)
}

/// The delivery state of the most recent outbound message, in
/// (occurred_at, id) order — the same row `store::inbox` picks.
async fn latest_outbound_state_on(
    pg: &PostgresDb,
    conversation_id: Uuid,
) -> Result<String, InboxError> {
    let state: Option<String> = sqlx::query_scalar(
        "SELECT delivery_state FROM message WHERE conversation_id = $1 AND direction = $2 \
         ORDER BY occurred_at DESC, id DESC LIMIT 1",
    )
    .bind(conversation_id)
    .bind(direction::OUTBOUND)
    .fetch_optional(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(state.unwrap_or_else(|| delivery::RECEIVED.to_string()))
}

async fn mark_read_on(pg: &PostgresDb, conversation_id: Uuid) -> Result<(), InboxError> {
    sqlx::query("UPDATE inbox_conversation SET unread = false, updated_at = now() WHERE id = $1")
        .bind(conversation_id)
        .execute(&pg.0)
        .await
        .map_err(db_error)?;
    Ok(())
}

async fn find_by_email_conversation_on(
    pg: &PostgresDb,
    email_conversation_id: Uuid,
) -> Result<Option<crate::inbox::Conversation>, InboxError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONVERSATION_COLUMNS} FROM inbox_conversation \
         WHERE email_conversation_id = $1 LIMIT 1"
    )))
    .bind(email_conversation_id)
    .fetch_optional(&pg.0)
    .await
    .map_err(db_error)?;
    row.map(|row| conversation(&row))
        .transpose()
        .map_err(db_error)
}

pub(crate) async fn list_intake_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor_id: Uuid,
) -> Result<Vec<IntakeRow>, InboxError> {
    let actor = inbox::load_person(db, actor_id).await?;
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
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONVERSATION_COLUMNS} FROM inbox_conversation ORDER BY updated_at DESC"
    )))
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    let mut conversations = rows
        .iter()
        .map(conversation)
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    if let Some(firms) = visible_firms {
        conversations.retain(|conversation| firms.contains(&conversation.firm_id));
    }
    intake_rows(pg, db, actor.role, conversations, false).await
}

pub(crate) async fn intake_detail_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor_id: Uuid,
    conversation_id: Uuid,
) -> Result<IntakeDetail, InboxError> {
    let actor = inbox::load_person(db, actor_id).await?;
    let conversation = conversation_by_id_on(pg, conversation_id)
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
        Some(project_id) => inbox::sees_project(db, actor_id, actor.role, project_id).await?,
        None => false,
    };
    let can_social_reply = inbox::is_social_channel(&conversation.channel)
        && inbox::social_reply_actor_still_authorized(db, actor_id, &conversation).await?;
    let can_sms_reply = conversation.channel == channel::SMS
        && inbox::social_reply_actor_still_authorized(db, actor_id, &conversation).await?;
    let bodies_visible =
        conversation.project_id.is_none() || participates || can_social_reply || can_sms_reply;
    let messages = if bodies_visible {
        messages_for_on(pg, conversation.id).await?
    } else {
        Vec::new()
    };
    if bodies_visible {
        mark_read_on(pg, conversation.id).await?;
    }
    let delivery_state = latest_outbound_state_on(pg, conversation.id).await?;
    let contact = if conversation.channel == channel::SMS {
        crate::persons::find_by_phone(db, &conversation.external_address)
            .await?
            .as_ref()
            .map(ContactLink::of)
    } else {
        None
    };
    Ok(IntakeDetail {
        contact,
        projects: inbox::projects_in_firm(db, conversation.firm_id).await?,
        next_action: if can_social_reply || can_sms_reply {
            inbox::next_action(actor.role, true, &delivery_state, true).to_string()
        } else {
            inbox::next_action(
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

/// Shared body of both intake lists: decorate conversations with delivery,
/// phone contact, assignee, and the next action.
async fn intake_rows(
    pg: &PostgresDb,
    db: &SurrealDb,
    role: Role,
    conversations: Vec<crate::inbox::Conversation>,
    assigned: bool,
) -> Result<Vec<IntakeRow>, InboxError> {
    let can_send_default = matches!(role, Role::Lawyer | Role::Clerk);
    let phones = inbox::phone_directory(db, &conversations).await?;
    let mut listed = Vec::with_capacity(conversations.len());
    for conversation in conversations {
        let delivery_state = latest_outbound_state_on(pg, conversation.id).await?;
        let assignee_name = inbox::assignee_label(db, conversation.project_id).await?;
        listed.push(IntakeRow {
            contact: inbox::contact_for(&phones, &conversation),
            id: conversation.id,
            firm_id: conversation.firm_id,
            brand_key: conversation.brand_key.clone(),
            project_id: conversation.project_id,
            channel: conversation.channel.clone(),
            assignee: assignee_name,
            unread: conversation.unread,
            next_action: inbox::next_action(
                role,
                assigned || conversation.project_id.is_some(),
                &delivery_state,
                can_send_default,
            )
            .to_string(),
            delivery: delivery_state,
            external_address: conversation.external_address,
            external_name: conversation.external_name,
            subject: conversation.subject,
            updated_at: conversation.updated_at,
        });
    }
    Ok(listed)
}

pub(crate) async fn list_project_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<IntakeRow>, InboxError> {
    let actor = inbox::load_person(db, actor_id).await?;
    if actor.role == Role::Client
        || !inbox::sees_project(db, actor_id, actor.role, project_id).await?
    {
        return Err(InboxError::Denied);
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONVERSATION_COLUMNS} FROM inbox_conversation WHERE project_id = $1 \
         ORDER BY updated_at DESC"
    )))
    .bind(project_id)
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    let conversations = rows
        .iter()
        .map(conversation)
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    intake_rows(pg, db, actor.role, conversations, true).await
}

pub(crate) async fn project_thread_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor_id: Uuid,
    project_id: Uuid,
    conversation_id: Uuid,
) -> Result<ThreadView, InboxError> {
    let actor = inbox::load_person(db, actor_id).await?;
    if actor.role == Role::Client
        || !inbox::sees_project(db, actor_id, actor.role, project_id).await?
    {
        return Err(InboxError::Denied);
    }
    let conversation = conversation_by_id_on(pg, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.project_id != Some(project_id) {
        return Err(InboxError::NotFound);
    }
    let can_note = matches!(actor.role, Role::Lawyer | Role::Clerk);
    let can_send = matches!(actor.role, Role::Lawyer | Role::Clerk);
    let delivery_state = latest_outbound_state_on(pg, conversation.id).await?;
    let messages = messages_for_on(pg, conversation.id).await?;
    mark_read_on(pg, conversation.id).await?;
    Ok(ThreadView {
        assignee: inbox::assignee_label(db, conversation.project_id).await?,
        delivery: delivery_state.clone(),
        next_action: inbox::next_action(actor.role, true, &delivery_state, can_send).to_string(),
        conversation,
        messages,
        can_note,
        can_send,
        can_attribute: matches!(actor.role, Role::Lawyer | Role::Clerk),
    })
}

pub(crate) async fn clerk_may_relay_email_thread_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    person_id: Uuid,
    email_conversation_id: Uuid,
) -> Result<bool, InboxError> {
    let Ok(found) = inbox::load_person(db, person_id).await else {
        return Ok(false);
    };
    if !matches!(found.role, Role::Lawyer | Role::Clerk) {
        return Ok(false);
    }
    let Some(conversation) = find_by_email_conversation_on(pg, email_conversation_id).await? else {
        return Ok(false);
    };
    inbox::participant_still_may_reply(db, person_id, conversation.project_id).await
}

pub(crate) async fn assignments_on(
    pg: &PostgresDb,
    conversation_id: Uuid,
) -> Result<Vec<Assignment>, InboxError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {ASSIGNMENT_COLUMNS} FROM inbox_assignment WHERE conversation_id = $1 \
         ORDER BY inserted_at"
    )))
    .bind(conversation_id)
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    rows.iter()
        .map(assignment)
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)
}

pub(crate) async fn readable_attachment_on(
    pg: &PostgresDb,
    db: &SurrealDb,
    actor_id: Uuid,
    message_id: Uuid,
) -> Result<Option<Uuid>, InboxError> {
    let found = message_by_id_on(pg, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let conversation = conversation_by_id_on(pg, found.conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let actor = inbox::load_person(db, actor_id).await?;
    let allowed = match (actor.role, conversation.project_id) {
        (Role::Owner, None) => true,
        (Role::Owner, Some(project_id)) => {
            inbox::sees_project(db, actor_id, Role::Owner, project_id).await?
        }
        (Role::Client, _) => false,
        (_, Some(project_id)) => {
            let sees = inbox::sees_project(db, actor_id, actor.role, project_id).await?;
            if found.direction == direction::INTERNAL {
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
    Ok(found.attachment_asset_id)
}

// ---------------------------------------------------------------------------
// Reserved acknowledgments
// ---------------------------------------------------------------------------

pub(crate) async fn reserve_acknowledgment_on(
    pg: &PostgresDb,
    inbound: &IngestedInbound,
    body: &str,
) -> Result<Option<Uuid>, InboxError> {
    let id = inbox::acknowledgment_id(inbound.message_id);
    if message_by_id_on(pg, id).await?.is_some() {
        return Ok(None);
    }
    let reserved = sqlx::query(
        "INSERT INTO message (id, conversation_id, parent_message_id, visibility, direction, body, \
         author_person_id, provider_message_id, provider_ref, delivery_state, purpose, \
         attachment_asset_id, communication_id, occurred_at, alert_state, alert_channel_id, \
         alert_timestamp, inserted_at, updated_at) \
         VALUES ($1, $2, NULL, NULL, 'outbound', $3, NULL, NULL, NULL, 'unknown', 'matter', \
         NULL, NULL, $4, NULL, NULL, NULL, now(), now()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(inbound.conversation_id)
    .bind(body)
    .bind(Utc::now().to_rfc3339())
    .execute(&pg.0)
    .await
    .map_err(db_error)?;
    if reserved.rows_affected() == 0 {
        // The same race the Surreal CREATE ONLY covered: the row appeared
        // between the pre-check and the insert.
        return Ok(None);
    }
    Ok(Some(id))
}

pub(crate) async fn acknowledgment_submitted_on(
    pg: &PostgresDb,
    id: Uuid,
    provider_ref: Option<String>,
) -> Result<(), InboxError> {
    sqlx::query(
        "UPDATE message SET delivery_state = 'submitted', provider_ref = $2, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(provider_ref)
    .execute(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Slack notice lifecycle (crate::inbox_alerts' message-table writes)
// ---------------------------------------------------------------------------

/// Load the notice receipt retained on an inbound Message.
pub(crate) async fn alert_receipt_on(
    pg: &PostgresDb,
    message_id: Uuid,
) -> Result<crate::inbox_alerts::AlertReceipt, InboxError> {
    let row: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT alert_state, alert_channel_id, alert_timestamp FROM message WHERE id = $1",
    )
    .bind(message_id)
    .fetch_optional(&pg.0)
    .await
    .map_err(db_error)?;
    let (state, channel_id, timestamp) = row.ok_or(InboxError::NotFound)?;
    Ok(crate::inbox_alerts::AlertReceipt {
        state,
        channel_id,
        timestamp,
    })
}

/// Reserve before the network call; reserved iff the row was still `pending`.
pub(crate) async fn reserve_alert_on(
    pg: &PostgresDb,
    message_id: Uuid,
    channel_id: &str,
) -> Result<bool, InboxError> {
    let reserved: Option<Uuid> = sqlx::query_scalar(
        "UPDATE message SET alert_state = 'unknown', alert_channel_id = $2, updated_at = now() \
         WHERE id = $1 AND direction = 'inbound' AND alert_state = 'pending' RETURNING id",
    )
    .bind(message_id)
    .bind(channel_id)
    .fetch_optional(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(reserved.is_some())
}

/// Record a notice result without regressing a confirmed delivery.
pub(crate) async fn record_alert_on(
    pg: &PostgresDb,
    message_id: Uuid,
    state: &str,
    timestamp: Option<&str>,
) -> Result<(), InboxError> {
    // IS DISTINCT FROM keeps the guard true for a row with no alert state
    // yet — Surreal's `!=` treats NULL the same way on direction='inbound'
    // rows, and a pending row always has a state, so this only widens the
    // degenerate case.
    sqlx::query(
        "UPDATE message SET alert_state = $2, alert_timestamp = $3, updated_at = now() \
         WHERE id = $1 AND direction = 'inbound' \
         AND (alert_state IS DISTINCT FROM 'notified' OR $2 = 'notified')",
    )
    .bind(message_id)
    .bind(state)
    .bind(timestamp)
    .execute(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// New pending notices and ambiguous sends are recovered; historical rows
/// are excluded.
pub(crate) async fn awaiting_alert_reconciliation_on(
    pg: &PostgresDb,
    limit: u32,
) -> Result<Vec<Uuid>, InboxError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM message WHERE direction = 'inbound' \
         AND alert_state IN ('pending', 'unknown') \
         ORDER BY alert_state ASC, updated_at ASC LIMIT $1",
    )
    .bind(i64::from(limit))
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(ids)
}
