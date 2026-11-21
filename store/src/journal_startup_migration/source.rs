//! Explicit read-only Source adapter for retained operator imports.
use super::{CapturedJournalRow, ImportInput, ImportReport, JournalError, PostgresDb, TriageItem};
use crate::surreal::SurrealDb;
use serde::Deserialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

#[derive(Deserialize, SurrealValue)]
struct LegacyEntry {
    id: surrealdb::types::RecordId,
    firm_id: String,
    command_key: String,
    payload: String,
    inserted_at: Option<String>,
}

async fn capture_selected(
    db: &SurrealDb,
    firm: Option<Uuid>,
) -> Result<Vec<CapturedJournalRow>, JournalError> {
    let mut result = db.query("SELECT id,firm_id,command_key,payload,IF inserted_at != NONE THEN <string>inserted_at ELSE NONE END AS inserted_at FROM journal_entry WHERE $firm = NONE OR firm_id = $firm LIMIT 1001")
        .bind(("firm",firm.map(|id|id.to_string()))).await?.check()?;
    let rows: Vec<LegacyEntry> = result.take(0)?;
    super::require_review_bound(rows.len())?;
    let captured = rows
        .into_iter()
        .map(|row| {
            let id = crate::surreal::record_uuid(&row.id).ok_or_else(|| {
                JournalError::Invalid("legacy Journal physical identity is invalid".into())
            })?;
            Ok(CapturedJournalRow {
                id,
                firm_id: row.firm_id,
                command_key: row.command_key,
                payload: row.payload,
                inserted_at: row.inserted_at,
            })
        })
        .collect::<Result<Vec<_>, JournalError>>()?;
    super::handoff::parse_captured(&captured)?;
    Ok(captured)
}
struct Input<'a>(&'a SurrealDb);
#[async_trait::async_trait]
impl ImportInput for Input<'_> {
    async fn rows(&self, firm: Option<Uuid>) -> Result<Vec<CapturedJournalRow>, JournalError> {
        capture_selected(self.0, firm).await
    }
    async fn firm(&self, id: Uuid) -> anyhow::Result<Option<crate::firms::Firm>> {
        Ok(crate::postgres_mirror::from_source(crate::firms::find_by_id(self.0, id)).await?)
    }
}

/// Read bounded physical Source rows for the operator's export receipt.
pub async fn capture(db: &SurrealDb) -> Result<Vec<CapturedJournalRow>, JournalError> {
    capture_selected(db, None).await
}
/// Read-only per-Firm timestamp triage of retained operator input.
pub async fn triage(db: &SurrealDb, firm: Uuid) -> Result<Vec<TriageItem>, JournalError> {
    super::triage_input(&Input(db), firm).await
}
/// Import retained operator input atomically under native admission and Firm locks.
pub async fn import(db: &SurrealDb, pg: &PostgresDb) -> Result<ImportReport, JournalError> {
    super::import_selected(&Input(db), pg, None).await
}
/// Import retained operator input restricted to one Firm.
pub async fn import_firm(
    db: &SurrealDb,
    pg: &PostgresDb,
    firm: Uuid,
) -> Result<ImportReport, JournalError> {
    super::import_selected(&Input(db), pg, Some(firm)).await
}

#[cfg(test)]
mod tests {
    use super::LegacyEntry;
    use surrealdb::types::{Object, RecordId, SurrealValue, Value};

    #[test]
    fn legacy_wire_decode_preserves_physical_uuid_and_optional_insertion_timestamp() {
        let physical = uuid::Uuid::new_v4();
        let firm = uuid::Uuid::new_v4().to_string();
        let payload = r#"{"id":"payload identity is not the physical record key"}"#;
        for timestamp in [Some("2026-10-12T01:02:03.123456789Z"), None] {
            let mut wire = Object::new();
            wire.insert("id", crate::surreal::record_id("journal_entry", physical));
            wire.insert("firm_id", firm.clone());
            wire.insert("command_key", "synthetic:legacy-wire");
            wire.insert("payload", payload);
            wire.insert("inserted_at", timestamp.map(str::to_owned));
            let decoded = LegacyEntry::from_value(Value::Object(wire.clone())).unwrap();
            assert_eq!(crate::surreal::record_uuid(&decoded.id), Some(physical));
            assert_eq!(decoded.id.table.as_str(), "journal_entry");
            assert_eq!(decoded.firm_id, firm);
            assert_eq!(decoded.command_key, "synthetic:legacy-wire");
            assert_eq!(decoded.payload, payload);
            assert_eq!(decoded.inserted_at.as_deref(), timestamp);
            // A native RecordId with a string key is not a UUID physical identity.
            wire.insert("id", RecordId::new("journal_entry", physical.to_string()));
            let string_key = LegacyEntry::from_value(Value::Object(wire)).unwrap();
            assert!(crate::surreal::record_uuid(&string_key.id).is_none());
        }
    }
}
