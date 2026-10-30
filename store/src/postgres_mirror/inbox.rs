//! Whole-row Inbox mirror, compared at PostgreSQL's timestamp precision.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use super::{changed, micros, MirrorError, MirrorKey, Mirrored, ParityReport, TableParity};
use crate::postgres::PostgresDb;
use crate::postgres_shadow::{compare_row, Comparison, ShadowKey};
use crate::surreal::SurrealDb;

struct Table {
    name: &'static str,
    columns: &'static str,
    references: &'static str,
    immutable: bool,
}

// The same five table names and columns are declared in the Inbox migration.
// `references` names only RecordId columns; every other value retains its
// source type through jsonb_populate_record.
const TABLES: &[Table] = &[
    Table {
        name: "inbox_conversation",
        columns: "id,firm_id,brand_key,interaction_type,provider_post_id,project_id,channel,external_address,external_name,subject,unread,provider_account,provider_thread_ref,source_page,source_channel,matter_consent,promotional_opt_in,promotional_opt_out,email_conversation_id,inserted_at,updated_at",
        references: "id,firm_id,project_id,email_conversation_id",
        immutable: false,
    },
    Table {
        name: "message",
        columns: "id,conversation_id,parent_message_id,visibility,direction,body,author_person_id,provider_message_id,provider_ref,delivery_state,purpose,attachment_asset_id,communication_id,occurred_at,alert_state,alert_channel_id,alert_timestamp,inserted_at,updated_at",
        references: "id,conversation_id,parent_message_id,author_person_id,attachment_asset_id,communication_id",
        immutable: false,
    },
    Table {
        name: "inbox_ingress",
        columns: "id,firm_id,channel,provider_account,provider_message_id,message_id,inserted_at",
        references: "id,firm_id,message_id",
        immutable: true,
    },
    Table {
        name: "inbox_assignment",
        columns: "id,conversation_id,firm_id,from_project_id,to_project_id,actor_person_id,inserted_at",
        references: "id,conversation_id,firm_id,from_project_id,to_project_id,actor_person_id",
        immutable: true,
    },
    Table {
        name: "inbox_delivery_attempt",
        columns: "id,message_id,state,provider_ref,detail,inserted_at",
        references: "id,message_id",
        immutable: true,
    },
];

fn table(name: &str) -> Result<&'static Table, MirrorError> {
    TABLES
        .iter()
        .find(|table| table.name == name)
        .ok_or(MirrorError::Source)
}

fn uuid_in(value: &Value) -> Option<Uuid> {
    match value {
        Value::String(text) => {
            let text = text.rsplit(':').next()?.trim_matches(['⟨', '⟩', '"']);
            let text = text
                .strip_prefix("u'")
                .and_then(|text| text.strip_suffix('\''))
                .unwrap_or(text);
            Uuid::parse_str(text).ok()
        }
        Value::Object(fields) => fields.values().find_map(uuid_in),
        _ => None,
    }
}

fn key(name: &str, id: Uuid) -> Result<MirrorKey, MirrorError> {
    Ok(match name {
        "inbox_conversation" => MirrorKey::InboxConversation(id),
        "message" => MirrorKey::InboxMessage(id),
        "inbox_ingress" => MirrorKey::InboxIngress(id),
        "inbox_assignment" => MirrorKey::InboxAssignment(id),
        "inbox_delivery_attempt" => MirrorKey::InboxDeliveryAttempt(id),
        _ => return Err(MirrorError::Source),
    })
}

fn normalize(table: &Table, row: Value) -> Result<(MirrorKey, Value), MirrorError> {
    let Value::Object(mut fields) = row else {
        return Err(MirrorError::Source);
    };
    for name in table.columns.split(',') {
        let value = fields.entry(name).or_insert(Value::Null);
        if table
            .references
            .split(',')
            .any(|reference| reference == name)
            && !value.is_null()
        {
            *value = Value::String(uuid_in(value).ok_or(MirrorError::Source)?.to_string());
        } else if matches!(name, "inserted_at" | "updated_at") && !value.is_null() {
            let parsed = DateTime::parse_from_rfc3339(value.as_str().ok_or(MirrorError::Source)?)
                .map_err(|_| MirrorError::SourceTimestamp)?;
            *value = Value::String(
                parsed
                    .with_timezone(&Utc)
                    .to_rfc3339_opts(SecondsFormat::Micros, true),
            );
        }
    }
    fields.retain(|name, _| table.columns.split(',').any(|column| column == name));
    let id = uuid_in(fields.get("id").ok_or(MirrorError::Source)?).ok_or(MirrorError::Source)?;
    Ok((key(table.name, id)?, Value::Object(fields)))
}

async fn source(
    surreal: &SurrealDb,
    table: &Table,
) -> Result<BTreeMap<MirrorKey, Value>, MirrorError> {
    let rows: Vec<surrealdb::types::Value> = surreal
        .query(format!("SELECT * FROM {}", table.name))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    rows.into_iter()
        .map(|row| normalize(table, row.into_json_value()))
        .collect()
}

async fn source_one(
    surreal: &SurrealDb,
    table: &Table,
    id: Uuid,
) -> Result<Option<Value>, MirrorError> {
    let row: Option<surrealdb::types::Value> = surreal
        .query("SELECT * FROM ONLY $id")
        .bind(("id", crate::surreal::record_id(table.name, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    row.map(|row| normalize(table, row.into_json_value()).map(|(_, value)| value))
        .transpose()
}

async fn target(pg: &PostgresDb, table: &Table) -> Result<BTreeMap<MirrorKey, Value>, MirrorError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT to_jsonb(t) AS row FROM {} AS t",
        table.name
    )))
    .fetch_all(&pg.0)
    .await?;
    rows.iter()
        .map(|row| {
            let value: Value = row.try_get("row")?;
            normalize(table, value)
        })
        .collect()
}

fn upsert_sql(table: &Table) -> String {
    if table.immutable {
        return format!("INSERT INTO {} SELECT * FROM jsonb_populate_record(NULL::{}, $1::jsonb) ON CONFLICT (id) DO NOTHING", table.name, table.name);
    }
    let columns: Vec<&str> = table.columns.split(',').collect();
    let updates = columns
        .iter()
        .skip(1)
        .map(|column| format!("{column}=EXCLUDED.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let source = columns
        .iter()
        .skip(1)
        .map(|column| format!("{}.{column}", table.name))
        .collect::<Vec<_>>()
        .join(",");
    let excluded = columns
        .iter()
        .skip(1)
        .map(|column| format!("EXCLUDED.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("INSERT INTO {} SELECT * FROM jsonb_populate_record(NULL::{}, $1::jsonb) ON CONFLICT (id) DO UPDATE SET {updates} WHERE ROW({source}) IS DISTINCT FROM ROW({excluded})", table.name, table.name)
}

fn parent(column: &str, id: Uuid) -> Option<MirrorKey> {
    Some(match column {
        "firm_id" => MirrorKey::Firm(id),
        "project_id" | "from_project_id" | "to_project_id" => MirrorKey::Project(id),
        "email_conversation_id" => MirrorKey::EmailConversation(id),
        "conversation_id" => MirrorKey::InboxConversation(id),
        "parent_message_id" | "message_id" => MirrorKey::InboxMessage(id),
        "author_person_id" | "actor_person_id" => MirrorKey::Person(id),
        "attachment_asset_id" => MirrorKey::Asset(id),
        "communication_id" => MirrorKey::Communication(id),
        _ => return None,
    })
}

/// Match one source Inbox row in PostgreSQL. Missing mutable rows remove their
/// projection; audit rows remain as drift for review.
pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    let table = table(key.table())?;
    let id = match key {
        MirrorKey::InboxConversation(id)
        | MirrorKey::InboxMessage(id)
        | MirrorKey::InboxIngress(id)
        | MirrorKey::InboxAssignment(id)
        | MirrorKey::InboxDeliveryAttempt(id) => *id,
        _ => return Err(MirrorError::Source),
    };
    if let Some(row) = source_one(surreal, table, id).await? {
        for column in table.references.split(',').filter(|column| *column != "id") {
            if let Some(parent) = row
                .get(column)
                .and_then(uuid_in)
                .and_then(|id| parent(column, id))
            {
                Box::pin(super::mirror(surreal, pg, &parent)).await?;
            }
        }
        let result = sqlx::query(sqlx::AssertSqlSafe(upsert_sql(table)))
            .bind(row)
            .execute(&pg.0)
            .await?;
        Ok(changed(result.rows_affected(), Mirrored::Written))
    } else {
        if table.immutable {
            return Ok(Mirrored::Unchanged);
        }
        let result = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {} WHERE id=$1",
            table.name
        )))
        .bind(id)
        .execute(&pg.0)
        .await?;
        Ok(changed(result.rows_affected(), Mirrored::Deleted))
    }
}

/// Compare the complete Inbox transcript and its audit rows.
pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let mut tables = Vec::with_capacity(TABLES.len());
    for table in TABLES {
        let source = source(surreal, table).await?;
        let target = target(pg, table).await?;
        tables.push(super::compare(
            table.name,
            &source
                .into_iter()
                .map(|(key, row)| (key, super::Columns::Inbox(row)))
                .collect(),
            &target
                .into_iter()
                .map(|(key, row)| (key, super::Columns::Inbox(row)))
                .collect(),
        ));
    }
    Ok(ParityReport { tables })
}

/// Repair mutable projections and insert missing audit rows. Changed or extra
/// audit rows remain drift; mutable target-only rows delete in reverse order.
pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let report = parity(surreal, pg).await?;
    let mut deletes: Vec<&TableParity> = Vec::new();
    for table_report in &report.tables {
        let source = source(surreal, table(table_report.table)?).await?;
        for key in &table_report.divergent {
            if source.contains_key(key) {
                mirror(surreal, pg, key).await?;
            }
        }
        deletes.push(table_report);
    }
    for table_report in deletes.into_iter().rev() {
        let source = source(surreal, table(table_report.table)?).await?;
        for key in &table_report.divergent {
            if !source.contains_key(key) {
                mirror(surreal, pg, key).await?;
            }
        }
    }
    parity(surreal, pg).await
}

// ---------------------------------------------------------------------------
// Shadow reads (crate::postgres_shadow) — one row per lookup
// ---------------------------------------------------------------------------

const CONVERSATION_BY_ID: &str = "SELECT id, firm_id, brand_key, interaction_type, \
     provider_post_id, project_id, channel, external_address, external_name, subject, \
     unread, provider_account, provider_thread_ref, source_page, source_channel, \
     matter_consent, promotional_opt_in, promotional_opt_out, email_conversation_id, \
     inserted_at, updated_at FROM inbox_conversation WHERE id = $1";

const MESSAGE_BY_ID: &str = "SELECT id, conversation_id, parent_message_id, visibility, \
     direction, body, author_person_id, provider_message_id, provider_ref, delivery_state, \
     purpose, attachment_asset_id, communication_id, occurred_at, alert_state, \
     alert_channel_id, alert_timestamp, inserted_at, updated_at FROM message WHERE id = $1";

// Independent mirrored flags — mirrors Conversation's own flag columns.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConversationImage {
    id: Uuid,
    firm_id: Uuid,
    brand_key: Option<String>,
    interaction_type: String,
    provider_post_id: Option<String>,
    project_id: Option<Uuid>,
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
    email_conversation_id: Option<Uuid>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ConversationImage {
    fn from_source(row: &crate::inbox::Conversation) -> Self {
        Self {
            id: row.id,
            firm_id: row.firm_id,
            brand_key: row.brand_key.clone(),
            interaction_type: row.interaction_type.clone(),
            provider_post_id: row.provider_post_id.clone(),
            project_id: row.project_id,
            channel: row.channel.clone(),
            external_address: row.external_address.clone(),
            external_name: row.external_name.clone(),
            subject: row.subject.clone(),
            unread: row.unread,
            provider_account: row.provider_account.clone(),
            provider_thread_ref: row.provider_thread_ref.clone(),
            source_page: row.source_page.clone(),
            source_channel: row.source_channel.clone(),
            matter_consent: row.matter_consent,
            promotional_opt_in: row.promotional_opt_in,
            promotional_opt_out: row.promotional_opt_out,
            email_conversation_id: row.email_conversation_id,
            inserted_at: micros(row.inserted_at),
            updated_at: micros(row.updated_at),
        }
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageImage {
    id: Uuid,
    conversation_id: Uuid,
    parent_message_id: Option<Uuid>,
    visibility: Option<String>,
    direction: String,
    body: String,
    author_person_id: Option<Uuid>,
    provider_message_id: Option<String>,
    provider_ref: Option<String>,
    delivery_state: String,
    purpose: String,
    attachment_asset_id: Option<Uuid>,
    communication_id: Option<Uuid>,
    occurred_at: String,
    alert_state: Option<String>,
    alert_channel_id: Option<String>,
    alert_timestamp: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl MessageImage {
    fn from_source(row: &crate::inbox::Message) -> Self {
        Self {
            id: row.id,
            conversation_id: row.conversation_id,
            parent_message_id: row.parent_message_id,
            visibility: row.visibility.clone(),
            direction: row.direction.clone(),
            body: row.body.clone(),
            author_person_id: row.author_person_id,
            provider_message_id: row.provider_message_id.clone(),
            provider_ref: row.provider_ref.clone(),
            delivery_state: row.delivery_state.clone(),
            purpose: row.purpose.clone(),
            attachment_asset_id: row.attachment_asset_id,
            communication_id: row.communication_id,
            occurred_at: row.occurred_at.clone(),
            alert_state: None,
            alert_channel_id: None,
            alert_timestamp: None,
            inserted_at: micros(row.inserted_at),
            updated_at: micros(row.updated_at),
        }
    }

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
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
            communication_id: row.try_get("communication_id")?,
            occurred_at: row.try_get("occurred_at")?,
            alert_state: row.try_get("alert_state")?,
            alert_channel_id: row.try_get("alert_channel_id")?,
            alert_timestamp: row.try_get("alert_timestamp")?,
            inserted_at: row.try_get("inserted_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

/// Compare one Conversation read with PostgreSQL's row for the same lookup.
pub(crate) async fn shadow_conversation(
    pg: &PostgresDb,
    id: Uuid,
    source: Option<&crate::inbox::Conversation>,
) -> Result<Comparison, MirrorError> {
    compare_row(
        pg,
        CONVERSATION_BY_ID,
        ShadowKey::Id(id),
        source.map(ConversationImage::from_source),
        ConversationImage::from_row,
        |image| image.id,
    )
    .await
}

/// Compare one Message read with PostgreSQL's row for the same lookup. The
/// model carries no alert fields, so the source side compares them as absent.
pub(crate) async fn shadow_message(
    pg: &PostgresDb,
    id: Uuid,
    source: Option<&crate::inbox::Message>,
) -> Result<Comparison, MirrorError> {
    compare_row(
        pg,
        MESSAGE_BY_ID,
        ShadowKey::Id(id),
        source.map(MessageImage::from_source),
        MessageImage::from_row,
        |image| image.id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::{normalize, source, source_one, table, upsert_sql};

    fn inbound(firm_id: Uuid) -> crate::inbox::IngestInbound<'static> {
        crate::inbox::IngestInbound {
            firm_id,
            brand_key: Some("neon"),
            interaction_type: None,
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: "email",
            provider_account: "synthetic-account",
            provider_message_id: "synthetic-message",
            provider_thread_ref: None,
            external_address: "sender@example.com",
            external_name: None,
            subject: None,
            body: "Synthetic message",
            source_page: None,
            source_channel: None,
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachment_asset_id: None,
            occurred_at: "2026-10-20T00:00:00Z",
        }
    }

    #[test]
    fn record_ids_and_timestamps_become_comparable_sql_values() {
        let id = Uuid::nil();
        let firm = Uuid::now_v7();
        let (_, row) = normalize(
            table("inbox_conversation").unwrap(),
            json!({
                "id": format!("inbox_conversation:{id}"), "firm_id": format!("firm:{firm}"),
                "inserted_at": "2026-10-20T01:02:03Z", "updated_at": "2026-10-20T01:02:03+00:00"
            }),
        )
        .unwrap();
        assert_eq!(row["id"], id.to_string());
        assert_eq!(row["firm_id"], firm.to_string());
        assert_eq!(row["inserted_at"], row["updated_at"]);
        assert!(row
            .get("project_id")
            .is_some_and(serde_json::Value::is_null));
    }

    #[test]
    fn upsert_is_idempotent_at_every_column() {
        let sql = upsert_sql(table("inbox_ingress").unwrap());
        assert!(sql.contains("ON CONFLICT (id) DO NOTHING"));
        let sql = upsert_sql(table("message").unwrap());
        assert!(sql.contains("ON CONFLICT (id) DO UPDATE"));
        assert!(sql.contains("ROW(message.conversation_id"));
        assert!(sql.contains("IS DISTINCT FROM"));
    }

    #[tokio::test]
    async fn source_images_cover_conversation_message_and_ingress() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let saved = crate::inbox::ingest_inbound(&db, &inbound(firm))
            .await
            .unwrap();
        let conversations = source(&db, table("inbox_conversation").unwrap())
            .await
            .unwrap();
        let messages = source(&db, table("message").unwrap()).await.unwrap();
        let ingress = source(&db, table("inbox_ingress").unwrap()).await.unwrap();
        assert!(
            conversations.contains_key(&crate::postgres_mirror::MirrorKey::InboxConversation(
                saved.conversation_id
            ))
        );
        assert!(
            messages.contains_key(&crate::postgres_mirror::MirrorKey::InboxMessage(
                saved.message_id
            ))
        );
        assert_eq!(ingress.len(), 1);
        assert!(source_one(&db, table("message").unwrap(), saved.message_id)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn five_inbox_tables_mirror_and_mutable_drift_reconciles() {
        use crate::postgres_mirror::{self, MirrorKey};
        use crate::surreal::record_id;

        let Some((options, database)) = postgres_mirror::tests::fresh_schema().await else {
            return;
        };
        let pg = postgres_mirror::tests::connect(&options, &database).await;
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let saved = crate::inbox::ingest_inbound(&db, &inbound(firm))
            .await
            .unwrap();
        let project = crate::projects::create(
            &db,
            &crate::projects::NewProject {
                code: crate::projects::code_from_name("Synthetic Inbox Project", Uuid::now_v7()),
                name: "Synthetic Inbox Project".into(),
                status: "open".into(),
                entity_id: crate::test_support::seed_entity(&db).await,
                firm_id: Some(firm),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let actor = crate::persons::create(
            &db,
            &crate::persons::NewPerson::with_role(
                "Synthetic Owner",
                "owner@example.com",
                crate::persons::Role::Owner,
            ),
        )
        .await
        .unwrap();
        let assignment = crate::inbox::assign(&db, actor.id, saved.conversation_id, project.id)
            .await
            .unwrap()
            .id;
        let attempt = Uuid::now_v7();
        db.query("CREATE $id SET message_id=$message, state='submitted', inserted_at=time::now()")
            .bind(("id", record_id("inbox_delivery_attempt", attempt)))
            .bind(("message", record_id("message", saved.message_id)))
            .await
            .unwrap()
            .check()
            .unwrap();

        let firm_entity = crate::firms::find_by_id(&db, firm)
            .await
            .unwrap()
            .unwrap()
            .entity_id
            .unwrap();
        postgres_mirror::tests::mirror_identity(&pg, &[firm_entity, project.entity_id], &[]).await;

        let ingress = source(&db, table("inbox_ingress").unwrap()).await.unwrap();
        let ingress = ingress.keys().next().unwrap().clone();
        for key in [
            MirrorKey::InboxConversation(saved.conversation_id),
            MirrorKey::InboxMessage(saved.message_id),
            ingress,
            MirrorKey::InboxAssignment(assignment),
            MirrorKey::InboxDeliveryAttempt(attempt),
        ] {
            postgres_mirror::mirror(&db, &pg, &key)
                .await
                .unwrap_or_else(|error| panic!("{key}: {error}"));
        }
        assert!(postgres_mirror::inbox::parity(&db, &pg)
            .await
            .unwrap()
            .is_clean());
        assert!(
            sqlx::query("UPDATE inbox_assignment SET inserted_at=now() WHERE id=$1")
                .bind(assignment)
                .execute(&pg.0)
                .await
                .is_err()
        );
        sqlx::query("UPDATE message SET body='drifted' WHERE id=$1")
            .bind(saved.message_id)
            .execute(&pg.0)
            .await
            .unwrap();
        assert!(!postgres_mirror::inbox::parity(&db, &pg)
            .await
            .unwrap()
            .is_clean());
        assert!(postgres_mirror::inbox::reconcile(&db, &pg)
            .await
            .unwrap()
            .is_clean());
        assert!(postgres_mirror::inbox::reconcile(&db, &pg)
            .await
            .unwrap()
            .is_clean());
        pg.0.close().await;
    }
}
