//! PostgreSQL projections for privileged Communications and email threads.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use surrealdb::types::{Datetime, RecordId, SurrealValue};
use uuid::Uuid;

use super::{changed, micros, Columns, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{
    postgres::PostgresDb,
    surreal::{record_uuid, SurrealDb},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommunicationImage {
    id: Uuid,
    project_id: Uuid,
    channel: String,
    direction: String,
    author_person_id: Option<Uuid>,
    counterparty: Option<String>,
    subject: Option<String>,
    body: String,
    source_ref: Option<String>,
    counterparty_role: Option<String>,
    acknowledges: Option<Uuid>,
    asset_id: Option<Uuid>,
    occurred_at: DateTime<Utc>,
    call: Option<Value>,
    agreements: Value,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConversationImage {
    id: Uuid,
    token: String,
    external_email: String,
    external_name: Option<String>,
    person_id: Option<Uuid>,
    subject: String,
    status: String,
    notation_id: Option<Uuid>,
    firm_id: Option<Uuid>,
    project_id: Option<Uuid>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MessageImage {
    id: Uuid,
    conversation_id: Uuid,
    direction: String,
    from_addr: String,
    to_addr: String,
    subject: String,
    body_text: String,
    raw_storage_key: Option<String>,
    provider_message_id: Option<String>,
    in_reply_to: Option<String>,
    command_payload: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct CommRow {
    id: RecordId,
    project_id: RecordId,
    channel: String,
    direction: String,
    author_person_id: Option<RecordId>,
    counterparty: Option<String>,
    subject: Option<String>,
    body: String,
    source_ref: Option<String>,
    counterparty_role: Option<String>,
    acknowledges: Option<RecordId>,
    asset_id: Option<RecordId>,
    occurred_at: String,
    call: Option<Value>,
    agreements: Option<Value>,
    inserted_at: Datetime,
    updated_at: Datetime,
}
#[derive(SurrealValue)]
struct ConvRow {
    id: RecordId,
    token: String,
    external_email: String,
    external_name: Option<String>,
    person_id: Option<RecordId>,
    subject: String,
    status: String,
    notation_id: Option<RecordId>,
    firm_id: Option<RecordId>,
    project_id: Option<RecordId>,
    inserted_at: Datetime,
    updated_at: Datetime,
}
#[derive(SurrealValue)]
struct MsgRow {
    id: RecordId,
    conversation_id: RecordId,
    direction: String,
    from_addr: String,
    to_addr: String,
    subject: String,
    body_text: String,
    raw_storage_key: Option<String>,
    provider_message_id: Option<String>,
    in_reply_to: Option<String>,
    command_payload: Option<String>,
    inserted_at: Datetime,
    updated_at: Datetime,
}

fn uuid(value: &RecordId) -> Result<Uuid, MirrorError> {
    record_uuid(value).ok_or(MirrorError::Source)
}
fn optional(value: Option<&RecordId>) -> Result<Option<Uuid>, MirrorError> {
    value.map(uuid).transpose()
}
fn date(value: &str) -> Result<DateTime<Utc>, MirrorError> {
    DateTime::parse_from_rfc3339(value)
        .map(|d| micros(d.with_timezone(&Utc)))
        .map_err(|_| MirrorError::Source)
}
fn source_image(row: CommRow) -> Result<CommunicationImage, MirrorError> {
    Ok(CommunicationImage {
        id: uuid(&row.id)?,
        project_id: uuid(&row.project_id)?,
        channel: row.channel,
        direction: row.direction,
        author_person_id: optional(row.author_person_id.as_ref())?,
        counterparty: row.counterparty,
        subject: row.subject,
        body: row.body,
        source_ref: row.source_ref,
        counterparty_role: row.counterparty_role,
        acknowledges: optional(row.acknowledges.as_ref())?,
        asset_id: optional(row.asset_id.as_ref())?,
        occurred_at: date(&row.occurred_at)?,
        call: row.call,
        agreements: row.agreements.unwrap_or_else(|| Value::Array(Vec::new())),
        inserted_at: micros(row.inserted_at.into()),
        updated_at: micros(row.updated_at.into()),
    })
}
fn conversation_image(r: ConvRow) -> Result<ConversationImage, MirrorError> {
    Ok(ConversationImage {
        id: uuid(&r.id)?,
        token: r.token,
        external_email: r.external_email,
        external_name: r.external_name,
        person_id: optional(r.person_id.as_ref())?,
        subject: r.subject,
        status: r.status,
        notation_id: optional(r.notation_id.as_ref())?,
        firm_id: optional(r.firm_id.as_ref())?,
        project_id: optional(r.project_id.as_ref())?,
        inserted_at: micros(r.inserted_at.into()),
        updated_at: micros(r.updated_at.into()),
    })
}
fn message_image(r: MsgRow) -> Result<MessageImage, MirrorError> {
    Ok(MessageImage {
        id: uuid(&r.id)?,
        conversation_id: uuid(&r.conversation_id)?,
        direction: r.direction,
        from_addr: r.from_addr,
        to_addr: r.to_addr,
        subject: r.subject,
        body_text: r.body_text,
        raw_storage_key: r.raw_storage_key,
        provider_message_id: r.provider_message_id,
        in_reply_to: r.in_reply_to,
        command_payload: r.command_payload,
        inserted_at: micros(r.inserted_at.into()),
        updated_at: micros(r.updated_at.into()),
    })
}

async fn source<
    T: SurrealValue,
    I: Into<MirrorKey>,
    F: Fn(T) -> Result<(I, Columns), MirrorError>,
>(
    db: &SurrealDb,
    table: &str,
    fields: &str,
    convert: F,
) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows: Vec<T> = db
        .query(format!("SELECT {fields} FROM {table}"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(convert)
        .map(|r| r.map(|(k, v)| (k.into(), v)))
        .collect()
}

async fn target_comms(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows = sqlx::query("SELECT * FROM communication")
        .fetch_all(&pg.0)
        .await?;
    rows.into_iter()
        .map(|r| {
            let image = CommunicationImage {
                id: r.try_get("id")?,
                project_id: r.try_get("project_id")?,
                channel: r.try_get("channel")?,
                direction: r.try_get("direction")?,
                author_person_id: r.try_get("author_person_id")?,
                counterparty: r.try_get("counterparty")?,
                subject: r.try_get("subject")?,
                body: r.try_get("body")?,
                source_ref: r.try_get("source_ref")?,
                counterparty_role: r.try_get("counterparty_role")?,
                acknowledges: r.try_get("acknowledges")?,
                asset_id: r.try_get("asset_id")?,
                occurred_at: r.try_get("occurred_at")?,
                call: r.try_get("call")?,
                agreements: r.try_get("agreements")?,
                inserted_at: r.try_get("inserted_at")?,
                updated_at: r.try_get("updated_at")?,
            };
            Ok((
                MirrorKey::Communication(image.id),
                Columns::Communication(Box::new(image)),
            ))
        })
        .collect()
}
async fn target_convs(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows = sqlx::query("SELECT * FROM email_conversation")
        .fetch_all(&pg.0)
        .await?;
    rows.into_iter()
        .map(|r| {
            let i = ConversationImage {
                id: r.try_get("id")?,
                token: r.try_get("token")?,
                external_email: r.try_get("external_email")?,
                external_name: r.try_get("external_name")?,
                person_id: r.try_get("person_id")?,
                subject: r.try_get("subject")?,
                status: r.try_get("status")?,
                notation_id: r.try_get("notation_id")?,
                firm_id: r.try_get("firm_id")?,
                project_id: r.try_get("project_id")?,
                inserted_at: r.try_get("inserted_at")?,
                updated_at: r.try_get("updated_at")?,
            };
            Ok((
                MirrorKey::EmailConversation(i.id),
                Columns::EmailConversation(Box::new(i)),
            ))
        })
        .collect()
}
async fn target_msgs(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let rows = sqlx::query("SELECT * FROM email_conversation_message")
        .fetch_all(&pg.0)
        .await?;
    rows.into_iter()
        .map(|r| {
            let i = MessageImage {
                id: r.try_get("id")?,
                conversation_id: r.try_get("conversation_id")?,
                direction: r.try_get("direction")?,
                from_addr: r.try_get("from_addr")?,
                to_addr: r.try_get("to_addr")?,
                subject: r.try_get("subject")?,
                body_text: r.try_get("body_text")?,
                raw_storage_key: r.try_get("raw_storage_key")?,
                provider_message_id: r.try_get("provider_message_id")?,
                in_reply_to: r.try_get("in_reply_to")?,
                command_payload: r.try_get("command_payload")?,
                inserted_at: r.try_get("inserted_at")?,
                updated_at: r.try_get("updated_at")?,
            };
            Ok((
                MirrorKey::EmailConversationMessage(i.id),
                Columns::EmailConversationMessage(Box::new(i)),
            ))
        })
        .collect()
}
async fn source_all(
    db: &SurrealDb,
) -> Result<
    (
        BTreeMap<MirrorKey, Columns>,
        BTreeMap<MirrorKey, Columns>,
        BTreeMap<MirrorKey, Columns>,
    ),
    MirrorError,
> {
    let c=source(db,"communication","id, project_id, channel, direction, author_person_id, counterparty, subject, body, source_ref, counterparty_role, acknowledges, asset_id, occurred_at, call, agreements, inserted_at, updated_at",|r:CommRow| { let i=source_image(r)?; Ok((MirrorKey::Communication(i.id),Columns::Communication(Box::new(i)))) }).await?;
    let v=source(db,"email_conversation","id, token, external_email, external_name, person_id, subject, status, notation_id, firm_id, project_id, inserted_at, updated_at",|r:ConvRow| { let i=conversation_image(r)?; Ok((MirrorKey::EmailConversation(i.id),Columns::EmailConversation(Box::new(i)))) }).await?;
    let m=source(db,"email_conversation_message","id, conversation_id, direction, from_addr, to_addr, subject, body_text, raw_storage_key, provider_message_id, in_reply_to, command_payload, inserted_at, updated_at",|r:MsgRow| { let i=message_image(r)?; Ok((MirrorKey::EmailConversationMessage(i.id),Columns::EmailConversationMessage(Box::new(i)))) }).await?;
    Ok((c, v, m))
}

async fn upsert_communication(
    pg: &PostgresDb,
    i: &CommunicationImage,
) -> Result<Mirrored, MirrorError> {
    let r=sqlx::query("INSERT INTO communication (id,project_id,channel,direction,author_person_id,counterparty,subject,body,source_ref,asset_id,occurred_at,call,agreements,inserted_at,updated_at,counterparty_role,acknowledges) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17) ON CONFLICT (id) DO UPDATE SET project_id=EXCLUDED.project_id,channel=EXCLUDED.channel,direction=EXCLUDED.direction,author_person_id=EXCLUDED.author_person_id,counterparty=EXCLUDED.counterparty,subject=EXCLUDED.subject,body=EXCLUDED.body,counterparty_role=EXCLUDED.counterparty_role,acknowledges=EXCLUDED.acknowledges,source_ref=EXCLUDED.source_ref,asset_id=EXCLUDED.asset_id,occurred_at=EXCLUDED.occurred_at,call=EXCLUDED.call,agreements=EXCLUDED.agreements,inserted_at=EXCLUDED.inserted_at,updated_at=EXCLUDED.updated_at WHERE (communication.project_id,communication.channel,communication.direction,communication.author_person_id,communication.counterparty,communication.subject,communication.body,communication.source_ref,communication.counterparty_role,communication.acknowledges,communication.asset_id,communication.occurred_at,communication.call,communication.agreements,communication.inserted_at,communication.updated_at) IS DISTINCT FROM (EXCLUDED.project_id,EXCLUDED.channel,EXCLUDED.direction,EXCLUDED.author_person_id,EXCLUDED.counterparty,EXCLUDED.subject,EXCLUDED.body,EXCLUDED.source_ref,EXCLUDED.counterparty_role,EXCLUDED.acknowledges,EXCLUDED.asset_id,EXCLUDED.occurred_at,EXCLUDED.call,EXCLUDED.agreements,EXCLUDED.inserted_at,EXCLUDED.updated_at)")
      .bind(i.id).bind(i.project_id).bind(&i.channel).bind(&i.direction).bind(i.author_person_id).bind(&i.counterparty).bind(&i.subject).bind(&i.body).bind(&i.source_ref).bind(i.asset_id).bind(i.occurred_at).bind(&i.call).bind(&i.agreements).bind(i.inserted_at).bind(i.updated_at).bind(&i.counterparty_role).bind(i.acknowledges).execute(&pg.0).await?;
    Ok(changed(r.rows_affected(), Mirrored::Written))
}
async fn upsert_conversation(
    pg: &PostgresDb,
    i: &ConversationImage,
) -> Result<Mirrored, MirrorError> {
    let r=sqlx::query("INSERT INTO email_conversation (id,token,external_email,external_name,person_id,subject,status,notation_id,firm_id,project_id,inserted_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT (id) DO UPDATE SET token=EXCLUDED.token,external_email=EXCLUDED.external_email,external_name=EXCLUDED.external_name,person_id=EXCLUDED.person_id,subject=EXCLUDED.subject,status=EXCLUDED.status,notation_id=EXCLUDED.notation_id,firm_id=EXCLUDED.firm_id,project_id=EXCLUDED.project_id,inserted_at=EXCLUDED.inserted_at,updated_at=EXCLUDED.updated_at WHERE (email_conversation.token,email_conversation.external_email,email_conversation.external_name,email_conversation.person_id,email_conversation.subject,email_conversation.status,email_conversation.notation_id,email_conversation.firm_id,email_conversation.project_id,email_conversation.inserted_at,email_conversation.updated_at) IS DISTINCT FROM (EXCLUDED.token,EXCLUDED.external_email,EXCLUDED.external_name,EXCLUDED.person_id,EXCLUDED.subject,EXCLUDED.status,EXCLUDED.notation_id,EXCLUDED.firm_id,EXCLUDED.project_id,EXCLUDED.inserted_at,EXCLUDED.updated_at)")
      .bind(i.id).bind(&i.token).bind(&i.external_email).bind(&i.external_name).bind(i.person_id).bind(&i.subject).bind(&i.status).bind(i.notation_id).bind(i.firm_id).bind(i.project_id).bind(i.inserted_at).bind(i.updated_at).execute(&pg.0).await?;
    Ok(changed(r.rows_affected(), Mirrored::Written))
}
async fn upsert_message(pg: &PostgresDb, i: &MessageImage) -> Result<Mirrored, MirrorError> {
    let r=sqlx::query("INSERT INTO email_conversation_message (id,conversation_id,direction,from_addr,to_addr,subject,body_text,raw_storage_key,provider_message_id,in_reply_to,command_payload,inserted_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) ON CONFLICT (id) DO UPDATE SET conversation_id=EXCLUDED.conversation_id,direction=EXCLUDED.direction,from_addr=EXCLUDED.from_addr,to_addr=EXCLUDED.to_addr,subject=EXCLUDED.subject,body_text=EXCLUDED.body_text,raw_storage_key=EXCLUDED.raw_storage_key,provider_message_id=EXCLUDED.provider_message_id,in_reply_to=EXCLUDED.in_reply_to,command_payload=EXCLUDED.command_payload,inserted_at=EXCLUDED.inserted_at,updated_at=EXCLUDED.updated_at WHERE (email_conversation_message.conversation_id,email_conversation_message.direction,email_conversation_message.from_addr,email_conversation_message.to_addr,email_conversation_message.subject,email_conversation_message.body_text,email_conversation_message.raw_storage_key,email_conversation_message.provider_message_id,email_conversation_message.in_reply_to,email_conversation_message.command_payload,email_conversation_message.inserted_at,email_conversation_message.updated_at) IS DISTINCT FROM (EXCLUDED.conversation_id,EXCLUDED.direction,EXCLUDED.from_addr,EXCLUDED.to_addr,EXCLUDED.subject,EXCLUDED.body_text,EXCLUDED.raw_storage_key,EXCLUDED.provider_message_id,EXCLUDED.in_reply_to,EXCLUDED.command_payload,EXCLUDED.inserted_at,EXCLUDED.updated_at)")
      .bind(i.id).bind(i.conversation_id).bind(&i.direction).bind(&i.from_addr).bind(&i.to_addr).bind(&i.subject).bind(&i.body_text).bind(&i.raw_storage_key).bind(&i.provider_message_id).bind(&i.in_reply_to).bind(&i.command_payload).bind(i.inserted_at).bind(i.updated_at).execute(&pg.0).await?;
    Ok(changed(r.rows_affected(), Mirrored::Written))
}

async fn ensure_parent(s: &SurrealDb, pg: &PostgresDb, key: MirrorKey) -> Result<(), MirrorError> {
    match key {
        MirrorKey::Project(_) | MirrorKey::Firm(_) => super::tenancy::mirror(s, pg, &key).await?,
        MirrorKey::Person(_) => super::identity::mirror(s, pg, &key).await?,
        MirrorKey::Asset(_) => super::documents::mirror(s, pg, &key).await?,
        MirrorKey::Notation(_) => super::workflow::mirror(s, pg, &key).await?,
        _ => return Err(MirrorError::Source),
    };
    Ok(())
}

async fn mirror_communication(
    s: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let rows: Vec<CommRow> = s.query("SELECT id, project_id, channel, direction, author_person_id, counterparty, subject, body, source_ref, counterparty_role, acknowledges, asset_id, occurred_at, call, agreements, inserted_at, updated_at FROM communication WHERE id=$id")
        .bind(("id", crate::surreal::record_id("communication", id))).await
        .and_then(surrealdb::IndexedResults::check).map_err(|_| MirrorError::Source)?
        .take(0).map_err(|_| MirrorError::Source)?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(changed(
            sqlx::query("DELETE FROM communication WHERE id=$1")
                .bind(id)
                .execute(&pg.0)
                .await?
                .rows_affected(),
            Mirrored::Deleted,
        ));
    };
    let image = source_image(row)?;
    ensure_parent(s, pg, MirrorKey::Project(image.project_id)).await?;
    if let Some(id) = image.author_person_id {
        ensure_parent(s, pg, MirrorKey::Person(id)).await?;
    }
    if let Some(id) = image.asset_id {
        ensure_parent(s, pg, MirrorKey::Asset(id)).await?;
    }
    if let Some(id) = image.acknowledges {
        Box::pin(mirror_communication(s, pg, id)).await?;
    }
    upsert_communication(pg, &image).await
}

pub async fn mirror(
    s: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    match key {
        MirrorKey::Communication(id) => mirror_communication(s, pg, *id).await,
        MirrorKey::EmailConversation(id) => {
            let rows: Vec<ConvRow> = s.query("SELECT id, token, external_email, external_name, person_id, subject, status, notation_id, firm_id, project_id, inserted_at, updated_at FROM email_conversation WHERE id=$id")
                .bind(("id", crate::surreal::record_id("email_conversation", *id))).await
                .and_then(surrealdb::IndexedResults::check).map_err(|_| MirrorError::Source)?
                .take(0).map_err(|_| MirrorError::Source)?;
            let Some(row) = rows.into_iter().next() else {
                return Ok(changed(
                    sqlx::query("DELETE FROM email_conversation WHERE id=$1")
                        .bind(id)
                        .execute(&pg.0)
                        .await?
                        .rows_affected(),
                    Mirrored::Deleted,
                ));
            };
            let image = conversation_image(row)?;
            if let Some(id) = image.person_id {
                ensure_parent(s, pg, MirrorKey::Person(id)).await?;
            }
            if let Some(id) = image.firm_id {
                ensure_parent(s, pg, MirrorKey::Firm(id)).await?;
            }
            if let Some(id) = image.project_id {
                ensure_parent(s, pg, MirrorKey::Project(id)).await?;
            }
            if let Some(id) = image.notation_id {
                ensure_parent(s, pg, MirrorKey::Notation(id)).await?;
            }
            upsert_conversation(pg, &image).await
        }
        MirrorKey::EmailConversationMessage(id) => {
            let rows: Vec<MsgRow> = s.query("SELECT id, conversation_id, direction, from_addr, to_addr, subject, body_text, raw_storage_key, provider_message_id, in_reply_to, command_payload, inserted_at, updated_at FROM email_conversation_message WHERE id=$id")
                .bind(("id", crate::surreal::record_id("email_conversation_message", *id))).await
                .and_then(surrealdb::IndexedResults::check).map_err(|_| MirrorError::Source)?
                .take(0).map_err(|_| MirrorError::Source)?;
            let Some(row) = rows.into_iter().next() else {
                return Ok(changed(
                    sqlx::query("DELETE FROM email_conversation_message WHERE id=$1")
                        .bind(id)
                        .execute(&pg.0)
                        .await?
                        .rows_affected(),
                    Mirrored::Deleted,
                ));
            };
            let image = message_image(row)?;
            let parents: Vec<ConvRow> = s.query("SELECT id, token, external_email, external_name, person_id, subject, status, notation_id, firm_id, project_id, inserted_at, updated_at FROM email_conversation WHERE id=$id")
                .bind(("id", crate::surreal::record_id("email_conversation", image.conversation_id))).await
                .and_then(surrealdb::IndexedResults::check).map_err(|_| MirrorError::Source)?
                .take(0).map_err(|_| MirrorError::Source)?;
            let Some(parent) = parents.into_iter().next() else {
                return Err(MirrorError::Source);
            };
            let parent = conversation_image(parent)?;
            if let Some(id) = parent.person_id {
                ensure_parent(s, pg, MirrorKey::Person(id)).await?;
            }
            if let Some(id) = parent.firm_id {
                ensure_parent(s, pg, MirrorKey::Firm(id)).await?;
            }
            if let Some(id) = parent.project_id {
                ensure_parent(s, pg, MirrorKey::Project(id)).await?;
            }
            if let Some(id) = parent.notation_id {
                ensure_parent(s, pg, MirrorKey::Notation(id)).await?;
            }
            upsert_conversation(pg, &parent).await?;
            upsert_message(pg, &image).await
        }
        _ => Err(MirrorError::Source),
    }
}
pub async fn parity(s: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let (a, b, c) = source_all(s).await?;
    Ok(ParityReport {
        tables: vec![
            super::compare("communication", &a, &target_comms(pg).await?),
            super::compare("email_conversation", &b, &target_convs(pg).await?),
            super::compare("email_conversation_message", &c, &target_msgs(pg).await?),
        ],
    })
}
pub async fn reconcile(s: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let (a, b, c) = source_all(s).await?;
    for (src, tgt) in [
        (a, target_comms(pg).await?),
        (b, target_convs(pg).await?),
        (c, target_msgs(pg).await?),
    ] {
        let keys: Vec<_> = src
            .iter()
            .filter(|(k, v)| tgt.get(*k) != Some(*v))
            .map(|(k, _)| k.clone())
            .chain(tgt.keys().filter(|k| !src.contains_key(*k)).cloned())
            .collect();
        for k in keys {
            mirror(s, pg, &k).await?;
        }
    }
    parity(s, pg).await
}

#[cfg(test)]
#[path = "conversations_tests.rs"]
mod tests;
