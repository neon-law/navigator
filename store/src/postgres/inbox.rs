//! PostgreSQL authority for the Inbox slice: Conversation, Message,
//! `inbox_ingress`, `inbox_assignment`, and `inbox_delivery_attempt`, plus the
//! reads the renderers use.
//!
//! Whenever `NAVIGATOR_POSTGRES_HOST` is set ([`PostgresDb::configured`]),
//! every `store::inbox` entry point dispatches here instead of SurrealDB:
//! ingest, dedup, threading, attribution, replies, notes, SMS,
//! dispatch, delivery callbacks, and the intake/project reads. The dispatcher
//! semantics are unchanged — the Surreal statement flow is ported statement
//! for statement, with one improvement: ingest writes inside a single
//! transaction so a lost ingress race cannot strand an orphan Message, which
//! the Surreal path repaired by deleting after the fact.
//!
//! The Communication spine projection (`communications::ingest`) and the
//! person/project/brand authorization lookups stay with their own modules,
//! reached through the handle the callers already hold. This module writes no
//! Surreal Inbox rows, and the inbox mirror refuses to repair the slice from
//! SurrealDB (`docs/postgres-tooling.md`).

use std::collections::HashMap;

use chrono::Utc;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::inbox::{
    self, channel, delivery, direction, purpose, Assignment, ContactLink, Conversation,
    DispatchReport, InboxError, IngestInbound, IngestedInbound, IntakeCursor, IntakeDetail,
    IntakeFilters, IntakePage, IntakeRow, Message, ThreadView,
};
use crate::persons::Role;
use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub(crate) enum InboxAuthorityError {
    #[error("PostgreSQL is unavailable for the Inbox")]
    Unavailable,
}

/// The pool to dispatch to, or `None` when no PostgreSQL host is configured.
pub(crate) async fn authority() -> Result<PostgresDb, InboxAuthorityError> {
    PostgresDb::application()
        .await
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
    promotional_opt_out, email_conversation_id, person_id, trashed_at, inserted_at, updated_at";

/// The read image of a Message: the columns `store::inbox`'s Surreal reads
/// select. The alert trio stays delivery-alert surface, outside the reads
/// this slice serves; the mirror's whole-row parity still compares it.
const MESSAGE_COLUMNS: &str = "id, conversation_id, parent_message_id, visibility, direction, \
    body, author_person_id, provider_message_id, provider_ref, delivery_state, purpose, \
    attachment_asset_id, raw_storage_key, communication_id, occurred_at, inserted_at, updated_at";

const ASSIGNMENT_COLUMNS: &str = "id, conversation_id, firm_id, from_project_id, to_project_id, \
    actor_person_id, inserted_at";

/// The Firm's open Projects, with the context the Inbox picker and classifier use.
pub(crate) async fn projects_in_firm_on(
    pg: &PostgresDb,
    firm_id: Uuid,
) -> Result<Vec<inbox::ProjectChoice>, InboxError> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT id, code, name, description FROM project \
         WHERE firm_id = $1 AND lower(status) = 'open' ORDER BY name",
    )
    .bind(firm_id)
    .fetch_all(pg.pool())
    .await
    .map_err(db_error)?;
    rows.iter()
        .map(|row| {
            Ok(inbox::ProjectChoice {
                id: row.try_get("id")?,
                code: row.try_get("code")?,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(db_error)
}

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
        person_id: row.try_get("person_id")?,
        trashed_at: row.try_get("trashed_at")?,
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

/// The inbound email sender's Person under the PostgreSQL People mirror:
/// primary first, then any `person_mailbox` alias claim — the same exact,
/// case-insensitive resolution `persons::find_by_any_email` serves on the
/// Surreal lane. The [`crate::inbox::counterparty_decision`] guard applies
/// here too: a lawyer- or clerk-tier sender never links.
pub(crate) async fn inbound_counterparty_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
    inbound: &IngestInbound<'_>,
) -> Result<Option<Uuid>, InboxError> {
    resolve_or_create_sender_on(pg, db, inbound.external_address, inbound.external_name).await
}

/// Resolve a sender through primary email and mailbox claims. Only client-tier
/// counterparties link to the inbox. The unique primary address keeps concurrent
/// creation of the same unknown sender idempotent.
pub(crate) async fn resolve_or_create_sender_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
    external_address: &str,
    display_name: Option<&str>,
) -> Result<Option<Uuid>, InboxError> {
    let address = external_address.trim();
    let person = match crate::postgres::people::by_email(pg, address).await? {
        Some(person) => Some(person),
        None => {
            match crate::postgres::people::mailbox_holder(pg, address.to_lowercase().as_str())
                .await?
            {
                Some(holder) => crate::postgres::people::by_id(pg, holder).await?,
                None => None,
            }
        }
    };
    if let Some(person) = person {
        // A known lawyer- or clerk-tier sender is outgoing mail: decision
        // None, and certainly no second Person for the same address.
        return Ok(crate::inbox::counterparty_decision(&person));
    }
    let created = crate::postgres::people::find_or_create(
        pg,
        db,
        &crate::persons::NewPerson::new(
            crate::inbox::sender_display_name(display_name, address),
            address,
        ),
    )
    .await
    .map_err(|error| InboxError::Db(error.to_string()))?;
    Ok(Some(created.id))
}

/// Backfills `person_id` on still-unassociated email conversations — the
/// same resolve-or-create the ingest uses — and reports how many
/// conversations gained a link. Idempotent: only rows with `person_id IS
/// NULL` are selected, so a re-run finds nothing and writes nothing.
pub(crate) async fn backfill_person_links_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
) -> Result<u64, InboxError> {
    let unassociated: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT id, external_address, external_name FROM inbox_conversation \
         WHERE channel = $1 AND person_id IS NULL",
    )
    .bind(channel::EMAIL)
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    let mut linked = 0u64;
    for (conversation_id, external_address, external_name) in unassociated {
        let Some(person_id) =
            resolve_or_create_sender_on(pg, db, &external_address, external_name.as_deref())
                .await?
        else {
            continue;
        };
        // First association wins: the WHERE guard keeps a concurrent
        // ingest or backfill pass from double-writing.
        let updated = sqlx::query(
            "UPDATE inbox_conversation SET person_id = $2, updated_at = now() \
             WHERE id = $1 AND person_id IS NULL",
        )
        .bind(conversation_id)
        .bind(person_id)
        .execute(&pg.0)
        .await
        .map_err(db_error)?
        .rows_affected();
        if updated > 0 {
            linked += 1;
            crate::inbox::record_association_log(
                db,
                conversation_id,
                Some(person_id),
                channel::EMAIL,
                &external_address,
            )
            .await;
        }
    }
    Ok(linked)
}

/// The ingest entry for provider-webhook-shaped deliveries: direction
/// `inbound`, no author, exactly `store::inbox::ingest_inbound`'s contract.
#[cfg(test)]
pub(crate) async fn ingest_inbound_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
    inbound: &IngestInbound<'_>,
) -> Result<IngestedInbound, InboxError> {
    ingest_threaded_on(pg, db, inbound, None).await
}

/// The inbound ingest entry behind `store::inbox::ingest_inbound` (`thread`
/// is `None`) and `store::inbox::ingest_inbound_threaded` (`thread` is the
/// reference chain, nearest ancestor first).
pub(crate) async fn ingest_threaded_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
    inbound: &IngestInbound<'_>,
    thread: Option<&[String]>,
) -> Result<IngestedInbound, InboxError> {
    let sender_person_id = match inbound.channel {
        channel::EMAIL => inbound_counterparty_on(pg, db, inbound).await?,
        _ => None,
    };
    ingest_on(
        pg,
        direction::INBOUND,
        None,
        sender_person_id,
        inbound,
        thread,
    )
    .await
}

/// The direction-aware ingest entry. Inbound traffic behaves exactly like
/// `ingest_threaded_on` without references; lawyer-sent imports (LAW-238 D1)
/// arrive with direction `outbound` and the mailbox owner as author — no
/// counterparty resolution. Delivery is `received` — historical mail is
/// already delivered — and `dispatch_queued` never re-sends it, because only
/// `queued` is dispatched.
pub(crate) async fn ingest_message_on(
    pg: &PostgresDb,
    direction_value: &str,
    author_person_id: Option<Uuid>,
    sender_person_id: Option<Uuid>,
    inbound: &IngestInbound<'_>,
) -> Result<IngestedInbound, InboxError> {
    ingest_on(
        pg,
        direction_value,
        author_person_id,
        sender_person_id,
        inbound,
        None,
    )
    .await
}

// The transactional ingest mirrors the Surreal `ingest_inbound` flow step
// for step; splitting it would hide the ordering that makes the dedup
// race-safe.
#[allow(clippy::too_many_lines)]
async fn ingest_on(
    pg: &PostgresDb,
    direction_value: &str,
    author_person_id: Option<Uuid>,
    sender_person_id: Option<Uuid>,
    inbound: &IngestInbound<'_>,
    thread: Option<&[String]>,
) -> Result<IngestedInbound, InboxError> {
    let channel_supported = matches!(
        inbound.channel,
        channel::EMAIL | channel::SMS | channel::SCRAPER | channel::VOICE
    ) || inbox::is_social_channel(inbound.channel);
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
    let threaded = match thread {
        Some(references) => find_referenced_thread_on(&mut tx, inbound, references).await?,
        None => None,
    };
    let conversation = match threaded {
        Some(existing) => existing,
        None => match find_open_thread_on(&mut *tx, inbound).await? {
            Some(existing) => existing,
            None => create_conversation_on(&mut *tx, inbound, None).await?,
        },
    };
    let person_linked =
        if let Some(person_id) = sender_person_id.filter(|_| conversation.person_id.is_none()) {
            // First association wins: record provenance only for the transaction
            // that actually installs the link, including a newly created thread.
            sqlx::query(
                "UPDATE inbox_conversation SET person_id = $2, updated_at = now() \
             WHERE id = $1 AND person_id IS NULL",
            )
            .bind(conversation.id)
            .bind(person_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected()
                > 0
        } else {
            false
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
            attachment_asset_id: inbound
                .attachments
                .first()
                .map(|attachment| attachment.asset_id),
            raw_storage_key: inbound.raw_storage_key,
            occurred_at: inbound.occurred_at,
        },
    )
    .await?;
    link_message_assets_in(&mut tx, inbound.firm_id, message_id, &inbound.attachments).await?;
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
    if let Some(person_id) = sender_person_id.filter(|_| person_linked) {
        let entry = inbox::association_log(
            conversation.id,
            person_id,
            inbound.channel,
            inbound.external_address,
        );
        crate::relationship_logs::record_on(&mut *tx, Uuid::now_v7(), &entry)
            .await
            .map_err(db_error)?;
    }
    tx.commit().await.map_err(db_error)?;

    // A new delivery on an existing thread re-flags it unread. The Surreal
    // ingest treats this update as best effort — a stale flag is cosmetic —
    // so a failure here is logged, not fatal.
    if let Err(error) = sqlx::query(
        "UPDATE inbox_conversation SET unread = true, trashed_at = NULL, updated_at = now() \
             WHERE id = $1",
    )
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
        person_linked,
    })
}

/// Fi imports reserve destination and immutable history in the caller's transaction.
pub(crate) async fn ingest_fi_message_in(
    connection: &mut sqlx::PgConnection,
    scope: &crate::fi_import::Scope,
    row: &crate::fi_import::Row,
) -> Result<IngestedInbound, crate::fi_import::FiImportError> {
    use crate::fi_import::{FiImportError as Error, IngestRow};
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT message_id FROM inbox_ingress WHERE firm_id=$1 AND provider_account=$2 AND provider_message_id=$3")
        .bind(scope.firm).bind(&scope.account).bind(&row.provider_message_id)
        .fetch_all(&mut *connection).await.map_err(|_|Error::Storage)?;
    if ids.len() > 1 {
        return Err(Error::Conflict);
    }
    let existing = ids.first().copied();
    let inbound = row.ingest(scope.firm, &scope.brand, &scope.account);
    let conversation_id = if let Some(id) = existing {
        sqlx::query_scalar("SELECT conversation_id FROM message WHERE id=$1")
            .bind(id)
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| Error::Storage)?
    } else {
        fi_conversation_id_in(connection, &inbound, scope.project)
            .await
            .map_err(|_| Error::Storage)?
    };
    let acquired: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,168))")
            .bind(conversation_id.to_string())
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| Error::Storage)?;
    if !acquired {
        return Err(Error::Busy);
    }
    let saved = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONVERSATION_COLUMNS} FROM inbox_conversation WHERE id=$1 FOR UPDATE"
    )))
    .bind(conversation_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|_| Error::Storage)?;
    let conversation = conversation(&saved).map_err(|_| Error::Storage)?;
    if conversation.firm_id != scope.firm
        || conversation.brand_key.as_deref() != Some(scope.brand.as_str())
        || conversation.provider_account != scope.account
        || conversation.channel != inbound.channel
        || scope.project.is_some_and(|project| {
            conversation
                .project_id
                .is_some_and(|actual| actual != project)
        })
    {
        return Err(Error::Conflict);
    }
    if let Some(id) = existing {
        let saved = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {MESSAGE_COLUMNS} FROM message WHERE id=$1"
        )))
        .bind(id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| Error::Storage)?;
        let message = message(&saved).map_err(|_| Error::Storage)?;
        if !crate::fi_import::matches_facts(scope, row, &message, &conversation)? {
            return Err(Error::Conflict);
        }
    }
    pin_fi_project_in(connection, scope, &conversation).await?;
    let message_id = if let Some(id) = existing {
        id
    } else {
        insert_fi_history_in(
            connection,
            &inbound,
            row.metadata.direction.as_str(),
            conversation_id,
        )
        .await?
    };
    if existing.is_none() {
        sqlx::query("UPDATE inbox_conversation SET unread=true,trashed_at=NULL,updated_at=now() WHERE id=$1")
            .bind(conversation_id)
            .execute(&mut *connection)
            .await
            .map_err(|_| Error::Storage)?;
    }
    Ok(IngestedInbound {
        conversation_id,
        message_id,
        deduped: existing.is_some(),
        person_linked: false,
    })
}

async fn fi_conversation_id_in(
    connection: &mut sqlx::PgConnection,
    inbound: &IngestInbound<'_>,
    project: Option<Uuid>,
) -> Result<Uuid, InboxError> {
    if let Some(project) = project {
        let mut selected = inbound.clone();
        selected.project_id = Some(project);
        if let Some(conversation) = find_open_thread_on(&mut *connection, &selected).await? {
            return Ok(conversation.id);
        }
    }
    match find_open_thread_on(&mut *connection, inbound).await? {
        Some(conversation) => Ok(conversation.id),
        // Start unassigned so pin_fi_project_in records the initial None-to-Project audit.
        // Historical imports never auto-create a counterparty.
        None => Ok(create_conversation_on(&mut *connection, inbound, None)
            .await?
            .id),
    }
}

async fn pin_fi_project_in(
    connection: &mut sqlx::PgConnection,
    scope: &crate::fi_import::Scope,
    conversation: &crate::inbox::Conversation,
) -> Result<(), crate::fi_import::FiImportError> {
    use crate::fi_import::FiImportError as Error;
    let Some(project) = scope.project else {
        return Ok(());
    };
    let valid: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project WHERE id=$1 AND firm_id=$2)")
            .bind(project)
            .bind(scope.firm)
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| Error::Storage)?;
    if !valid {
        return Err(Error::Conflict);
    }
    if conversation.project_id.is_none() {
        sqlx::query("UPDATE inbox_conversation SET project_id=$2,unread=true,updated_at=now() WHERE id=$1 AND project_id IS NULL")
            .bind(conversation.id).bind(project).execute(&mut *connection).await.map_err(|_|Error::Storage)?;
        sqlx::query("INSERT INTO inbox_assignment(id,conversation_id,firm_id,from_project_id,to_project_id,actor_person_id,inserted_at) VALUES($1,$2,$3,NULL,$4,$5,now())")
            .bind(Uuid::now_v7()).bind(conversation.id).bind(scope.firm).bind(project).bind(scope.actor)
            .execute(&mut *connection).await.map_err(|_|Error::Storage)?;
    }
    Ok(())
}

async fn insert_fi_history_in(
    connection: &mut sqlx::PgConnection,
    inbound: &IngestInbound<'_>,
    direction_value: &str,
    conversation_id: Uuid,
) -> Result<Uuid, crate::fi_import::FiImportError> {
    use crate::fi_import::FiImportError as Error;
    let id = insert_message(
        &mut *connection,
        &MessageInsert {
            conversation_id,
            direction: direction_value,
            body: inbound.body,
            author_person_id: None,
            provider_message_id: Some(inbound.provider_message_id),
            provider_ref: None,
            delivery_state: delivery::RECEIVED,
            purpose: purpose::MATTER,
            visibility: inbound.visibility,
            attachment_asset_id: None,
            raw_storage_key: None,
            occurred_at: inbound.occurred_at,
        },
    )
    .await
    .map_err(|_| Error::Storage)?;
    sqlx::query("INSERT INTO inbox_ingress(id,firm_id,channel,provider_account,provider_message_id,message_id,inserted_at) VALUES($1,$2,$3,$4,$5,$6,now())")
        .bind(Uuid::now_v7()).bind(inbound.firm_id).bind(inbound.channel).bind(inbound.provider_account)
        .bind(inbound.provider_message_id).bind(id).execute(&mut *connection).await.map_err(|_|Error::Storage)?;
    Ok(id)
}

async fn deduped_ingest(pg: &PostgresDb, message_id: Uuid) -> Result<IngestedInbound, InboxError> {
    let existing = message_by_id_on(pg, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    Ok(IngestedInbound {
        conversation_id: existing.conversation_id,
        message_id: existing.id,
        deduped: true,
        person_linked: false,
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

/// The Conversation `store::inbox::ingest_inbound_threaded` joins: same
/// Firm, channel, and Project attribution, keyed on the provider thread id
/// first and then on the nearest referenced Message.
async fn find_referenced_thread_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    inbound: &IngestInbound<'_>,
    references: &[String],
) -> Result<Option<crate::inbox::Conversation>, InboxError> {
    let mut conversation_id: Option<Uuid> = None;
    if let Some(thread) = inbound.provider_thread_ref {
        conversation_id = sqlx::query_scalar(
            "SELECT id FROM inbox_conversation WHERE firm_id = $1 AND channel = $2 \
             AND project_id IS NOT DISTINCT FROM $3 AND provider_thread_ref = $4 \
             ORDER BY inserted_at DESC LIMIT 1",
        )
        .bind(inbound.firm_id)
        .bind(inbound.channel)
        .bind(inbound.project_id)
        .bind(thread)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_error)?;
    }
    if conversation_id.is_none() && !references.is_empty() {
        let hits: Vec<(String, Uuid)> = sqlx::query_as(
            "SELECT m.provider_message_id, m.conversation_id FROM message m \
             JOIN inbox_conversation c ON c.id = m.conversation_id \
             WHERE m.provider_message_id = ANY($1) AND c.firm_id = $2 AND c.channel = $3 \
             AND c.project_id IS NOT DISTINCT FROM $4",
        )
        .bind(references)
        .bind(inbound.firm_id)
        .bind(inbound.channel)
        .bind(inbound.project_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(db_error)?;
        conversation_id = inbox::nearest_referenced_conversation(references, &hits);
    }
    let Some(conversation_id) = conversation_id else {
        return Ok(None);
    };
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONVERSATION_COLUMNS} FROM inbox_conversation WHERE id = $1"
    )))
    .bind(conversation_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_error)?;
    row.map(|row| conversation(&row))
        .transpose()
        .map_err(db_error)
}

async fn find_open_thread_on(
    executor: impl sqlx::PgExecutor<'_>,
    inbound: &IngestInbound<'_>,
) -> Result<Option<crate::inbox::Conversation>, InboxError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONVERSATION_COLUMNS} FROM inbox_conversation WHERE firm_id = $1 \
         AND channel = $2 AND brand_key IS NOT DISTINCT FROM $3 AND provider_account = $4 \
         AND interaction_type = $5 AND ($6::text IS NULL OR provider_thread_ref = $6) \
         AND external_address = $7 AND project_id IS NOT DISTINCT FROM $8 ORDER BY inserted_at DESC LIMIT 1"
    )))
    .bind(inbound.firm_id)
    .bind(inbound.channel)
    .bind(inbound.brand_key.map(str::to_string))
    .bind(inbound.provider_account)
    .bind(inbound.interaction_type.unwrap_or("message"))
    .bind(inbound.provider_thread_ref.map(str::to_string))
    .bind(inbound.external_address)
    .bind(inbound.project_id)
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
    person_id: Option<Uuid>,
) -> Result<crate::inbox::Conversation, InboxError> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO inbox_conversation (id, firm_id, brand_key, interaction_type, provider_post_id, \
         project_id, channel, external_address, external_name, subject, unread, provider_account, \
         provider_thread_ref, source_page, source_channel, matter_consent, promotional_opt_in, \
         promotional_opt_out, email_conversation_id, person_id, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $17, $6, $7, $8, $9, TRUE, $10, $11, $12, $13, $14, $15, \
         FALSE, $16, $18, now(), now())",
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
    .bind(inbound.project_id)
    .bind(person_id)
    .execute(executor)
    .await
    .map_err(db_error)?;
    Ok(crate::inbox::Conversation {
        id,
        firm_id: inbound.firm_id,
        brand_key: inbound.brand_key.map(str::to_string),
        interaction_type: inbound.interaction_type.unwrap_or("message").to_string(),
        provider_post_id: inbound.provider_post_id.map(str::to_string),
        project_id: inbound.project_id,
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
        person_id,
        trashed_at: None,
        inserted_at: Utc::now(),
        updated_at: Utc::now(),
    })
}

// ---------------------------------------------------------------------------
// Attribution and the Communication spine projection
// ---------------------------------------------------------------------------

async fn project_message_on(
    // Communication accessors use the shared application authority context.
    _pg: &PostgresDb,
    db: &ApplicationStore,
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
    db: &ApplicationStore,
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
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    let _fi_guard = fi_assignment_guard_on(pg, conversation_id).await?;
    let conversation = conversation_by_id_on(pg, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    inbox::require_assignment_authorization(db, actor_id, conversation.firm_id, project_id).await?;
    assign_write_on(pg, db, actor_id, &conversation, project_id).await
}

/// Apply the review queue's existing Firm and Project participation rules
/// before writing its assignment to the selected Inbox store.
pub(crate) async fn assign_for_triage_review_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation_id: Uuid,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    let actor = inbox::load_person(db, actor_id).await?;
    let _fi_guard = fi_assignment_guard_on(pg, conversation_id).await?;
    let conversation = conversation_by_id_on(pg, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let project = crate::projects::find_by_id(db, project_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if project.firm_id != Some(conversation.firm_id) {
        return Err(InboxError::CrossFirm);
    }
    if !inbox::triage_reviewer_may_assign(db, &actor, project_id).await? {
        return Err(InboxError::Denied);
    }
    if matches!(actor.role, Role::Lawyer | Role::Clerk) {
        if let Some(source_project) = conversation.project_id {
            if !inbox::triage_reviewer_may_assign(db, &actor, source_project).await? {
                return Err(InboxError::Denied);
            }
        }
    }
    assign_write_on(pg, db, actor_id, &conversation, project_id).await
}

async fn fi_assignment_guard_on(
    pg: &PostgresDb,
    conversation_id: Uuid,
) -> Result<Option<sqlx::Transaction<'_, sqlx::Postgres>>, InboxError> {
    let conversation = conversation_by_id_on(pg, conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if !crate::fi_import::is_account(&conversation.provider_account) {
        return Ok(None);
    }
    crate::fi_import::lock_on(pg)
        .await
        .map(Some)
        .map_err(|error| match error {
            crate::fi_import::FiImportError::Busy => InboxError::Invalid(
                "Fi import or assignment is in progress; retry assignment".into(),
            ),
            _ => InboxError::Db("Fi assignment coordination is unavailable".into()),
        })
}

async fn assign_write_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
    actor_id: Uuid,
    conversation: &crate::inbox::Conversation,
    project_id: Uuid,
) -> Result<Assignment, InboxError> {
    let conversation_id = conversation.id;
    let from_project_id = conversation.project_id;
    if from_project_id == Some(project_id) {
        // A retry must finish any interrupted projection without recording a
        // second attribution. Authorization still runs before this branch.
        project_all_messages_on(pg, db, conversation).await?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {ASSIGNMENT_COLUMNS} FROM inbox_assignment \
             WHERE conversation_id = $1 AND to_project_id = $2 \
             ORDER BY inserted_at DESC, id DESC LIMIT 1"
        )))
        .bind(conversation_id)
        .bind(project_id)
        .fetch_optional(&pg.0)
        .await
        .map_err(db_error)?;
        if let Some(row) = row {
            return assignment(&row).map_err(db_error);
        }
    }
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
    db: &ApplicationStore,
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
    db: &ApplicationStore,
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
    if !inbox::allows_external_reply(&conversation) {
        return Err(InboxError::Denied);
    }
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
    db: &ApplicationStore,
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
    db: &ApplicationStore,
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
    let person = crate::persons::find_by_any_email(db, email.trim())
        .await?
        .ok_or_else(|| InboxError::Contact("No person has that email".into()))?;
    if conversation
        .person_id
        .is_some_and(|holder| holder != person.id)
    {
        return Err(InboxError::Contact(
            "That conversation is already linked to another person".into(),
        ));
    }
    let structured_holders =
        crate::person_phones::person_ids_for_phone(db, &conversation.external_address)
            .await
            .map_err(|error| InboxError::Db(error.to_string()))?;
    if structured_holders.iter().any(|holder| *holder != person.id) {
        return Err(InboxError::Contact(
            "That number already belongs to another person".into(),
        ));
    }
    if crate::persons::find_by_phone(db, &conversation.external_address)
        .await?
        .is_some_and(|holder| holder.id != person.id)
    {
        return Err(InboxError::Contact(
            "That number already belongs to another person".into(),
        ));
    }
    crate::person_phones::seed_from_legacy_phone(db, person.id)
        .await
        .map_err(|error| InboxError::Db(error.to_string()))?;
    crate::person_phones::set_phone(
        db,
        person.id,
        &conversation.external_address,
        crate::person_phones::PhoneLabel::Mobile,
        false,
    )
    .await
    .map_err(|error| InboxError::Db(error.to_string()))?;
    if person.phone.is_none() {
        crate::persons::set_phone(db, person.id, Some(&conversation.external_address))
            .await?
            .ok_or(InboxError::NotFound)?;
    }
    let linked = sqlx::query(
        "UPDATE inbox_conversation SET person_id = $2, updated_at = now() \
         WHERE id = $1 AND (person_id IS NULL OR person_id = $2)",
    )
    .bind(conversation_id)
    .bind(person.id)
    .execute(&pg.0)
    .await
    .map_err(db_error)?;
    if linked.rows_affected() != 1 {
        return Err(InboxError::Contact(
            "That conversation is already linked to another person".into(),
        ));
    }
    let updated = crate::persons::find_by_id(db, person.id)
        .await?
        .ok_or(InboxError::NotFound)?;
    Ok(ContactLink::of(&updated))
}

pub(crate) async fn queue_sms_reply_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
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
    db: &ApplicationStore,
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
        project_id: None,
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
    let conversation = match find_open_thread_on(&pg.0, &thread).await? {
        Some(existing) => existing,
        None => create_conversation_on(&pg.0, &thread, None).await?,
    };
    let message_id = queue_sms_message_on(pg, db, &conversation, actor_id, body).await?;
    Ok((conversation.id, message_id))
}

async fn queue_sms_message_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
    conversation: &crate::inbox::Conversation,
    actor_id: Uuid,
    body: &str,
) -> Result<Uuid, InboxError> {
    if crate::fi_import::is_account(&conversation.provider_account) {
        return Err(InboxError::Denied);
    }
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
    db: &ApplicationStore,
    transport: &dyn inbox::OutboundTransport,
    message_id: Option<Uuid>,
) -> Result<DispatchReport, InboxError> {
    let queued = match message_id {
        Some(id) => message_by_id_on(pg, id)
            .await?
            .filter(|message| message.delivery_state == delivery::QUEUED)
            .into_iter()
            .collect(),
        None => messages_in_state_on(pg, delivery::QUEUED).await?,
    };
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
        if !allowed || !inbox::allows_external_reply(&conversation) {
            set_delivery_on(pg, message.id, delivery::SUPPRESSED, None).await?;
            journal_attempt_on(
                pg,
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

pub(crate) async fn set_delivery_on(
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
        "SELECT {MESSAGE_COLUMNS} FROM message \
         WHERE provider_ref = $1 AND conversation_id IN \
         (SELECT id FROM inbox_conversation WHERE firm_id = $2 \
          AND channel = $3 AND provider_account = $4) \
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
    db: &ApplicationStore,
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
    let scoped:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM message m JOIN inbox_conversation c ON c.id=m.conversation_id JOIN asset a ON a.id=$2 LEFT JOIN project p ON p.id=a.project_id WHERE m.id=$1 AND ((c.project_id IS NOT NULL AND a.project_id=c.project_id AND p.firm_id=c.firm_id) OR (c.project_id IS NULL AND (p.firm_id=c.firm_id OR (a.project_id IS NULL AND EXISTS(SELECT 1 FROM inbox_attachment_receipt r WHERE r.asset_id=a.id AND r.firm_id=c.firm_id AND r.sha256=a.sha256_hex))))))")
        .bind(message_id).bind(asset_id).fetch_one(&pg.0).await.map_err(db_error)?;
    if !scoped {
        return Err(InboxError::Denied);
    }
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

pub(crate) async fn messages_for_on(
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

/// Stamp or clear `trashed_at`. The caller has authorized the actor.
pub(crate) async fn set_trashed_on(
    pg: &PostgresDb,
    conversation_id: Uuid,
    trashed: bool,
) -> Result<(), InboxError> {
    sqlx::query(
        "UPDATE inbox_conversation SET trashed_at = CASE WHEN $2 THEN now() END WHERE id = $1",
    )
    .bind(conversation_id)
    .bind(trashed)
    .execute(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(())
}

/// None represents an Owner's cross-Firm visibility. An empty list grants no rows.
async fn intake_scope(
    db: &ApplicationStore,
    actor_id: Uuid,
) -> Result<(Role, Option<Vec<Uuid>>), InboxError> {
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
    Ok((actor.role, visible_firms))
}

pub(crate) async fn intake_brands_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
    actor_id: Uuid,
) -> Result<Vec<String>, InboxError> {
    let (_, firms) = intake_scope(db, actor_id).await?;
    let mut brands = sqlx::query_scalar(
        "SELECT DISTINCT brand_key FROM inbox_conversation
         WHERE brand_key IS NOT NULL AND ($1::uuid[] IS NULL OR firm_id = ANY($1))",
    )
    .bind(firms)
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    brands.sort();
    Ok(brands)
}

pub(crate) async fn list_intake_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
    actor_id: Uuid,
) -> Result<Vec<IntakeRow>, InboxError> {
    let (role, firms) = intake_scope(db, actor_id).await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CONVERSATION_COLUMNS} FROM inbox_conversation \
         WHERE ($1::uuid[] IS NULL OR firm_id = ANY($1)) ORDER BY updated_at DESC"
    )))
    .bind(firms)
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    let conversations = rows
        .iter()
        .map(conversation)
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    intake_rows(pg, db, role, conversations, false).await
}

// Date ordering uses the latest inbound or authored outbound email. Ignore historical
// date strings that are outside the provider timestamp format or invalid to PostgreSQL.
// Keep the fractional seconds as a decimal so cursor comparisons retain nanosecond precision.
const INTAKE_PAGE_SQL: &str = r"
WITH ranked AS (
    SELECT c.id, COALESCE(
        CASE WHEN c.channel = 'email' THEN (
            SELECT max(CASE
                WHEN m.occurred_at ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}[Tt ][0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?([Zz]|[+-][0-9]{2}:[0-9]{2})$'
                  AND pg_input_is_valid(m.occurred_at, 'timestamp with time zone')
                THEN extract(epoch FROM regexp_replace(m.occurred_at, '\.[0-9]+', '')::timestamptz)
                   + COALESCE(('0.' || substring(m.occurred_at FROM '\.([0-9]{1,9})'))::numeric, 0)
                END)
            FROM message m WHERE m.conversation_id = c.id
              AND (m.direction = 'inbound' OR (m.direction = 'outbound' AND m.author_person_id IS NOT NULL))
        ) END, extract(epoch FROM c.updated_at)) AS activity_at
    FROM inbox_conversation c
    WHERE ($1::uuid[] IS NULL OR c.firm_id = ANY($1))
      AND (c.trashed_at IS NOT NULL) = $2
      AND ($3::text IS NULL OR c.brand_key = $3)
      AND ($4::text IS NULL OR c.channel = $4)
      AND (NOT $5 OR c.unread)
      AND ($6::text IS NULL OR $6 = (
          SELECT m.direction FROM message m WHERE m.conversation_id = c.id
          ORDER BY m.occurred_at DESC, m.id DESC LIMIT 1
      ))
      AND (NOT $7 OR EXISTS (
          SELECT 1 FROM message m WHERE m.conversation_id = c.id AND m.attachment_asset_id IS NOT NULL
      ))
      AND (NOT $8 OR EXISTS (
          SELECT 1 FROM message m WHERE m.conversation_id = c.id
            AND m.direction = 'outbound' AND m.author_person_id IS NOT NULL
      ))
), page AS (
    SELECT id, activity_at FROM ranked
    WHERE ($9::bigint IS NULL OR (activity_at, id) <
        ($9::bigint + $10::integer::numeric / 1000000000, $11::uuid))
    ORDER BY activity_at DESC, id DESC LIMIT $12
)
SELECT c.*, floor(page.activity_at)::bigint AS activity_seconds,
       ((page.activity_at - floor(page.activity_at)) * 1000000000)::integer AS activity_nanos
FROM page JOIN inbox_conversation c ON c.id = page.id
ORDER BY page.activity_at DESC, page.id DESC
";

pub(crate) async fn intake_page_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
    actor_id: Uuid,
    filters: IntakeFilters<'_>,
    after: Option<IntakeCursor>,
    page_size: u16,
) -> Result<IntakePage, InboxError> {
    use sqlx::Row;
    let (role, firms) = intake_scope(db, actor_id).await?;
    let limit = page_size.clamp(1, 100);
    let mut page = sqlx::query(INTAKE_PAGE_SQL)
        .bind(firms)
        .bind(filters.trash_only)
        .bind(filters.brand)
        .bind(filters.channel)
        .bind(filters.unread_only)
        .bind(filters.direction)
        .bind(filters.attachments_only)
        .bind(filters.sent_only)
        .bind(after.map(|cursor| cursor.activity_at.timestamp()))
        .bind(after.map(|cursor| cursor.activity_at.timestamp_subsec_nanos().cast_signed()))
        .bind(after.map(|cursor| cursor.id))
        .bind(i64::from(limit) + 1)
        .fetch_all(&pg.0)
        .await
        .map_err(db_error)?;
    let next = if page.len() > usize::from(limit) {
        page.truncate(usize::from(limit));
        let last = &page[usize::from(limit) - 1];
        let seconds = last.try_get("activity_seconds").map_err(db_error)?;
        let nanos: i32 = last.try_get("activity_nanos").map_err(db_error)?;
        let activity_at = chrono::DateTime::from_timestamp(seconds, nanos.cast_unsigned())
            .ok_or_else(|| InboxError::Db("invalid Inbox activity timestamp".into()))?;
        Some(IntakeCursor {
            activity_at,
            id: last.try_get("id").map_err(db_error)?,
        })
    } else {
        None
    };
    let conversations = page
        .iter()
        .map(conversation)
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let rows = intake_rows(pg, db, role, conversations, false).await?;
    Ok(IntakePage { rows, next })
}

pub(crate) async fn intake_detail_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
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
        && !crate::fi_import::is_account(&conversation.provider_account)
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
    let contact = inbox::sender_identity(db, &conversation).await?;
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
/// phone contact, assignee, the next action, the persisted sender Person,
/// and the latest-message preview.
async fn intake_rows(
    pg: &PostgresDb,
    db: &ApplicationStore,
    role: Role,
    conversations: Vec<crate::inbox::Conversation>,
    assigned: bool,
) -> Result<Vec<IntakeRow>, InboxError> {
    let can_send_default = matches!(role, Role::Lawyer | Role::Clerk);
    let people = inbox::person_badges(db, &conversations).await?;
    let mut facts = intake_facts(pg, &conversations).await?;
    let mut listed = Vec::with_capacity(conversations.len());
    for conversation in conversations {
        let facts = facts.remove(&conversation.id).unwrap_or_default();
        let delivery_state = facts
            .delivery
            .unwrap_or_else(|| delivery::RECEIVED.to_owned());
        let assignee_name = inbox::assignee_label(db, conversation.project_id).await?;
        let person = conversation
            .person_id
            .and_then(|person_id| people.get(&person_id).cloned());
        listed.push(IntakeRow {
            contact: inbox::sender_identity(db, &conversation).await?,
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
                can_send_default && inbox::allows_external_reply(&conversation),
            )
            .to_string(),
            delivery: delivery_state,
            external_address: conversation.external_address,
            external_name: conversation.external_name,
            subject: conversation.subject,
            direction: facts.direction.unwrap_or_default(),
            preview: facts.preview.as_deref().and_then(inbox::preview_snippet),
            person,
            has_attachments: facts.has_attachments,
            sent: facts.sent,
            trashed: conversation.trashed_at.is_some(),
            email_sent_at: facts
                .email_dates
                .into_iter()
                .flatten()
                .filter_map(|date| chrono::DateTime::parse_from_rfc3339(&date).ok())
                .max()
                .map(|date| date.with_timezone(&Utc)),
            updated_at: conversation.updated_at,
        });
    }
    Ok(listed)
}

#[derive(Default)]
struct IntakeFacts {
    direction: Option<String>,
    has_attachments: bool,
    sent: bool,
    preview: Option<String>,
    email_dates: Option<Vec<String>>,
    delivery: Option<String>,
}

/// Read message facts for the selected conversations in one round trip.
async fn intake_facts(
    pg: &PostgresDb,
    conversations: &[Conversation],
) -> Result<HashMap<Uuid, IntakeFacts>, InboxError> {
    use sqlx::Row;
    if conversations.is_empty() {
        return Ok(HashMap::new());
    }
    let (ids, is_email): (Vec<_>, Vec<_>) = conversations
        .iter()
        .map(|c| (c.id, c.channel == channel::EMAIL))
        .unzip();
    let rows = sqlx::query(
        "SELECT c.id,
         (SELECT direction FROM message WHERE conversation_id = c.id
          ORDER BY occurred_at DESC, id DESC LIMIT 1) AS direction,
         EXISTS(SELECT 1 FROM message WHERE conversation_id = c.id
          AND attachment_asset_id IS NOT NULL) AS has_attachments,
         EXISTS(SELECT 1 FROM message WHERE conversation_id = c.id
          AND direction = 'outbound' AND author_person_id IS NOT NULL) AS sent,
         (SELECT left(body, 400) FROM message WHERE conversation_id = c.id
          AND (direction <> 'outbound' OR author_person_id IS NOT NULL)
          ORDER BY occurred_at DESC, id DESC LIMIT 1) AS preview,
         CASE WHEN c.is_email THEN ARRAY(SELECT occurred_at FROM message WHERE conversation_id = c.id
          AND (direction = 'inbound' OR (direction = 'outbound' AND author_person_id IS NOT NULL)))
          END AS email_dates,
         (SELECT delivery_state FROM message WHERE conversation_id = c.id
          AND direction = 'outbound' ORDER BY occurred_at DESC, id DESC LIMIT 1) AS delivery
         FROM unnest($1::uuid[], $2::bool[]) AS c(id, is_email)",
    )
    .bind(ids)
    .bind(is_email)
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("id")?,
                IntakeFacts {
                    direction: row.try_get("direction")?,
                    has_attachments: row.try_get("has_attachments")?,
                    sent: row.try_get("sent")?,
                    preview: row.try_get("preview")?,
                    email_dates: row.try_get("email_dates")?,
                    delivery: row.try_get("delivery")?,
                },
            ))
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(db_error)
}

pub(crate) async fn list_project_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
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

/// Unread conversations assigned to one Project — the matter dashboard's
/// correspondence tile. A read-only count over the conversations' existing
/// `unread` flag; the caller resolves its reader's access first (see
/// [`crate::inbox::unread_count_for_project`]).
pub(crate) async fn unread_count_for_project_on(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<i64, InboxError> {
    use sqlx::Row;
    let row = sqlx::query(sqlx::AssertSqlSafe(
        "SELECT count(*) FROM inbox_conversation WHERE project_id = $1 AND unread AND trashed_at IS NULL".to_string(),
    ))
    .bind(project_id)
    .fetch_one(&pg.0)
    .await
    .map_err(db_error)?;
    // `count(*)` always returns exactly one row; a bigint decodes to i64.
    row.try_get::<i64, _>(0).map_err(db_error)
}

pub(crate) async fn project_thread_on(
    pg: &PostgresDb,
    db: &ApplicationStore,
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
    let can_send = matches!(actor.role, Role::Lawyer | Role::Clerk)
        && inbox::allows_external_reply(&conversation);
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
    db: &ApplicationStore,
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
    db: &ApplicationStore,
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

async fn link_message_assets_in(
    connection: &mut sqlx::PgConnection,
    firm_id: Uuid,
    message_id: Uuid,
    attachments: &[inbox::IngestAttachment],
) -> Result<(), InboxError> {
    for (position, attachment) in attachments.iter().enumerate() {
        let scoped:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM message m JOIN inbox_conversation c ON c.id=m.conversation_id JOIN asset a ON a.id=$3 LEFT JOIN project p ON p.id=a.project_id WHERE m.id=$1 AND c.firm_id=$2 AND a.sha256_hex=$4 AND ((c.project_id IS NOT NULL AND a.project_id=c.project_id AND p.firm_id=c.firm_id) OR (c.project_id IS NULL AND (p.firm_id=c.firm_id OR (a.project_id IS NULL AND EXISTS(SELECT 1 FROM inbox_attachment_receipt r WHERE r.asset_id=a.id AND r.firm_id=c.firm_id AND r.sha256=a.sha256_hex))))))")
            .bind(message_id).bind(firm_id).bind(attachment.asset_id).bind(&attachment.sha256)
            .fetch_one(&mut *connection).await.map_err(db_error)?;
        if !scoped {
            return Err(InboxError::Denied);
        }
        let position = i32::try_from(position)
            .map_err(|_| InboxError::Db("too many message attachments".into()))?;
        sqlx::query(
            "INSERT INTO inbox_message_asset (id, firm_id, message_id, asset_id, filename, \
             position, sha256, inserted_at) VALUES ($1, $2, $3, $4, $5, $6, $7, now()) \
             ON CONFLICT ON CONSTRAINT inbox_message_asset_key DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(firm_id)
        .bind(message_id)
        .bind(attachment.asset_id)
        .bind(&attachment.filename)
        .bind(position)
        .bind(&attachment.sha256)
        .execute(&mut *connection)
        .await
        .map_err(db_error)?;
    }
    Ok(())
}

pub(crate) async fn link_message_assets_on(
    pg: &PostgresDb,
    firm_id: Uuid,
    message_id: Uuid,
    attachments: &[inbox::IngestAttachment],
) -> Result<(), InboxError> {
    let mut tx = pg.0.begin().await.map_err(db_error)?;
    let visible: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM message m JOIN inbox_conversation c ON c.id = m.conversation_id WHERE m.id = $1 AND c.firm_id = $2)",
    ).bind(message_id).bind(firm_id).fetch_one(&mut *tx).await.map_err(db_error)?;
    if !visible {
        return Err(InboxError::NotFound);
    }
    link_message_assets_in(&mut tx, firm_id, message_id, attachments).await?;
    tx.commit().await.map_err(db_error)
}

pub(crate) async fn message_assets_on(
    pg: &PostgresDb,
    firm_id: Uuid,
    message_id: Uuid,
) -> Result<Vec<inbox::MessageAttachment>, InboxError> {
    use sqlx::Row;
    let rows = sqlx::query("SELECT asset_id, filename, position::bigint AS position, sha256 FROM inbox_message_asset WHERE firm_id = $1 AND message_id = $2 ORDER BY position, id")
        .bind(firm_id).bind(message_id).fetch_all(&pg.0).await.map_err(db_error)?;
    rows.iter()
        .map(|row| {
            Ok(inbox::MessageAttachment {
                asset_id: row.try_get("asset_id")?,
                filename: row.try_get("filename")?,
                position: row.try_get("position")?,
                sha256: row.try_get("sha256")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(db_error)
}

pub(crate) async fn process_state_on(
    pg: &PostgresDb,
    message_id: Uuid,
) -> Result<Option<inbox::ProcessState>, InboxError> {
    let row: Option<(Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT route, processed_at, nef_outcome FROM message WHERE id = $1")
            .bind(message_id)
            .fetch_optional(&pg.0)
            .await
            .map_err(db_error)?;
    row.map(|(route, processed_at, nef_outcome)| {
        let processed_at = processed_at
            .map(|value| {
                chrono::DateTime::parse_from_rfc3339(&value)
                    .map(|at| at.with_timezone(&Utc))
                    .map_err(|error| InboxError::Db(error.to_string()))
            })
            .transpose()?;
        Ok(inbox::ProcessState {
            route,
            processed_at,
            nef_outcome,
        })
    })
    .transpose()
}

pub(crate) async fn mark_processed_on(
    pg: &PostgresDb,
    message_id: Uuid,
    route: &str,
    processed_at: chrono::DateTime<Utc>,
    outcome: Option<&str>,
) -> Result<(), InboxError> {
    sqlx::query(
        "UPDATE message SET route = $2, processed_at = $3, nef_outcome = $4, updated_at = now() WHERE id = $1",
    )
    .bind(message_id)
    .bind(route)
    .bind(processed_at.to_rfc3339())
    .bind(outcome)
    .execute(&pg.0)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn unprocessed_inbox_on(
    pg: &PostgresDb,
    visible_firms: Option<&[Uuid]>,
) -> Result<Vec<inbox::UnprocessedRow>, InboxError> {
    unprocessed_inbox_scoped_on(pg, visible_firms, None, None).await
}

pub(crate) struct AssignmentPreviewPage {
    pub after: Option<(chrono::DateTime<Utc>, Uuid)>,
    pub limit: i64,
}

/// A single page of an admitted mailbox's preview, using the same cursor as
/// the assignment proposal endpoint.
pub(crate) struct MailboxPreviewPage<'a> {
    pub actor_id: Uuid,
    pub firm_id: Uuid,
    pub brand_key: &'a str,
    pub provider_account: &'a str,
    pub includes_unbranded_intake: bool,
    pub page: AssignmentPreviewPage,
}

/// Read the admitted Inbox scope. PostgreSQL filters the mailbox before any
/// Message body is returned for review.
pub(crate) async fn unprocessed_inbox_scoped_on(
    pg: &PostgresDb,
    visible_firms: Option<&[Uuid]>,
    mailbox: Option<MailboxPreviewPage<'_>>,
    interactive: Option<AssignmentPreviewPage>,
) -> Result<Vec<inbox::UnprocessedRow>, InboxError> {
    use sqlx::Row;
    let page = mailbox
        .as_ref()
        .map(|scope| &scope.page)
        .or(interactive.as_ref());
    let (after_time, after_id) = page.and_then(|page| page.after).unzip();
    let rows = sqlx::query(
        "SELECT m.id, m.conversation_id, c.firm_id, c.project_id, c.channel, \
         m.provider_message_id, c.external_address, c.external_name, c.subject, m.body, \
         m.occurred_at, m.inserted_at FROM message m JOIN inbox_conversation c \
         ON c.id = m.conversation_id WHERE m.direction = 'inbound' \
         AND (m.processed_at IS NULL OR m.nef_outcome IN ('no_project', 'not_a_notice', 'no_case_number')) \
         AND ($1::uuid[] IS NULL OR c.firm_id = ANY($1)) \
         AND ($7::bigint IS NULL OR (c.channel = 'email' AND c.project_id IS NULL)) \
         AND ($2::uuid IS NULL OR (c.firm_id = $2
             AND (c.brand_key = $3 OR ($9 AND c.brand_key IS NULL AND c.source_channel = 'email'))
             AND c.provider_account = $4 AND c.trashed_at IS NULL
             AND EXISTS (SELECT 1 FROM firm_brand fb WHERE fb.firm_id = $2 AND fb.brand_key = $3)
             AND EXISTS (SELECT 1 FROM person actor WHERE actor.id = $8 AND
                 (actor.role = 'owner' OR (actor.role IN ('admin', 'lawyer', 'clerk')
                     AND EXISTS (SELECT 1 FROM person_firm_role pfr
                                 WHERE pfr.person_id = actor.id AND pfr.firm_id = $2)))))) \
         AND ($5::timestamptz IS NULL OR (m.inserted_at, m.id) > ($5, $6::uuid)) \
         ORDER BY m.inserted_at, m.id LIMIT $7",
    )
    .bind(visible_firms)
    .bind(mailbox.as_ref().map(|page| page.firm_id))
    .bind(mailbox.as_ref().map(|page| page.brand_key))
    .bind(mailbox.as_ref().map(|page| page.provider_account))
    .bind(after_time)
    .bind(after_id)
    .bind(page.map(|page| page.limit))
    .bind(mailbox.as_ref().map(|page| page.actor_id))
    .bind(mailbox.is_some_and(|page| page.includes_unbranded_intake))
    .fetch_all(&pg.0)
    .await
    .map_err(db_error)?;
    rows.iter()
        .map(|row| {
            Ok(inbox::UnprocessedRow {
                id: row.try_get("id")?,
                conversation_id: row.try_get("conversation_id")?,
                firm_id: row.try_get("firm_id")?,
                project_id: row.try_get("project_id")?,
                channel: row.try_get("channel")?,
                provider_message_id: row.try_get("provider_message_id")?,
                external_address: row.try_get("external_address")?,
                external_name: row.try_get("external_name")?,
                subject: row.try_get("subject")?,
                body: row.try_get("body")?,
                occurred_at: row.try_get("occurred_at")?,
                inserted_at: row.try_get("inserted_at")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(db_error)
}

/// Resolve a page's mailboxes and read live Project participation once per
/// resolved Person and receiving Firm. Keep the primary address precedence
/// of the People directory when a mailbox claim also exists.
pub(crate) async fn assignment_candidates_on(
    pg: &PostgresDb,
    addresses: &[(Uuid, String)],
    brand: Option<&str>,
) -> Result<Vec<(Uuid, inbox::assign_match::ResolvedAddress)>, InboxError> {
    let (firms, addresses): (Vec<_>, Vec<_>) = addresses.iter().cloned().unzip();
    let alias_keys: Vec<_> = addresses
        .iter()
        .map(|address| address.to_lowercase())
        .collect();
    let rows: Vec<(Uuid, String, Option<Uuid>, Vec<Uuid>)> = sqlx::query_as(
        "WITH resolved AS (
             SELECT input.firm_id, input.address, coalesce(p.id, pm.person_id) AS person_id
             FROM unnest($1::uuid[], $2::text[], $3::text[]) AS input(firm_id, address, alias_key)
             LEFT JOIN person p ON p.email = input.address::citext
             LEFT JOIN person_mailbox pm ON pm.id = lower(input.alias_key)
         ), open_projects AS (
             SELECT contact.firm_id, contact.person_id, array_agg(p.id ORDER BY p.id) AS ids
             FROM (SELECT DISTINCT firm_id, person_id FROM resolved WHERE person_id IS NOT NULL) contact
             JOIN person_project_role participation ON participation.person_id = contact.person_id
             JOIN project p ON p.id = participation.project_id AND p.firm_id = contact.firm_id
             WHERE p.status = 'open' AND ($4::text IS NULL OR p.brand = $4)
             GROUP BY contact.firm_id, contact.person_id
         )
         SELECT r.firm_id, r.address, r.person_id, coalesce(projects.ids, ARRAY[]::uuid[])
         FROM resolved r LEFT JOIN open_projects projects USING (firm_id, person_id)",
    )
    .bind(firms)
    .bind(addresses)
    .bind(alias_keys)
    .bind(brand)
    .fetch_all(pg.pool())
    .await
    .map_err(db_error)?;
    Ok(rows
        .into_iter()
        .map(|(firm_id, address, person_id, open_projects)| {
            (
                firm_id,
                inbox::assign_match::ResolvedAddress {
                    address,
                    person_id,
                    open_projects,
                },
            )
        })
        .collect())
}

/// Internal receipt projection. Only actual-byte ingestion or Source-authoritative
/// historical relationship verification calls this; no external UUID grant door.
pub(crate) async fn record_attachment_receipt_on(
    pg: &PostgresDb,
    firm_id: Uuid,
    asset_id: Uuid,
    sha256: &str,
) -> Result<(), InboxError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM asset WHERE id=$1 AND project_id IS NULL AND sha256_hex=$2)",
    )
    .bind(asset_id)
    .bind(sha256)
    .fetch_one(&pg.0)
    .await
    .map_err(db_error)?;
    if !exists {
        return Err(InboxError::Denied);
    }
    sqlx::query("INSERT INTO inbox_attachment_receipt(firm_id,asset_id,sha256) VALUES ($1,$2,$3) ON CONFLICT(firm_id,asset_id) DO NOTHING")
        .bind(firm_id).bind(asset_id).bind(sha256).execute(&pg.0).await.map_err(db_error)?;
    let retained:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM inbox_attachment_receipt WHERE firm_id=$1 AND asset_id=$2 AND sha256=$3)")
        .bind(firm_id).bind(asset_id).bind(sha256).fetch_one(&pg.0).await.map_err(db_error)?;
    if !retained {
        return Err(InboxError::Denied);
    }
    Ok(())
}
