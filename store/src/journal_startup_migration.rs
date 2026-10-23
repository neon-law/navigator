//! Temporary, read-only SurrealDB journal import. Run before serving requests.
//! PostgreSQL is authoritative: conflicts abort startup instead of overwriting it.
use crate::firm_journal::{entry_digest, JournalEntry, JournalError};
use crate::postgres::PostgresDb;
use crate::surreal::SurrealDb;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

#[derive(Deserialize, SurrealValue)]
struct LegacyEntry {
    firm_id: String,
    command_key: String,
    payload: String,
    inserted_at: Option<String>,
}

/// Operator-visible triage contains identifiers and timestamps, never journal content.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriageReason {
    MissingInsertionTimestamp,
    InvalidInsertionTimestamp,
    UnresolvedReversalTarget,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TriageItem {
    pub entry_id: Uuid,
    pub firm_id: Uuid,
    pub reason: TriageReason,
    pub occurred_at: String,
    pub inserted_at: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct ImportReport {
    pub imported: usize,
    pub triage: Vec<TriageItem>,
}
fn item(entry: &JournalEntry, inserted_at: Option<String>, reason: TriageReason) -> TriageItem {
    TriageItem {
        entry_id: entry.id,
        firm_id: entry.firm_id,
        reason,
        occurred_at: entry.occurred_at.clone(),
        inserted_at,
    }
}
async fn read(
    db: &SurrealDb,
    firm: Option<Uuid>,
) -> Result<Vec<(JournalEntry, Option<String>)>, JournalError> {
    // A bounded source read prevents startup or an operator request from loading an unlimited ledger.
    let mut result = db.query("SELECT firm_id, command_key, payload, IF inserted_at != NONE THEN <string>inserted_at ELSE NONE END AS inserted_at FROM journal_entry WHERE $firm = NONE OR firm_id = $firm LIMIT 1001")
        .bind(("firm", firm.map(|id| id.to_string()))).await?.check()?;
    let rows: Vec<LegacyEntry> = result.take(0)?;
    if rows.len() > 1000 {
        return Err(JournalError::Invalid(
            "legacy journal import exceeds 1000-row review bound".into(),
        ));
    }
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let entry: JournalEntry = serde_json::from_str(&row.payload)?;
        entry.validate()?;
        if row.firm_id != entry.firm_id.to_string() || row.command_key != entry.key {
            return Err(JournalError::Conflict);
        }
        entries.push((entry, row.inserted_at));
    }
    Ok(entries)
}
fn classify(
    rows: Vec<(JournalEntry, Option<String>)>,
) -> (Vec<(JournalEntry, String)>, Vec<TriageItem>) {
    let mut valid = Vec::new();
    let mut triage = Vec::new();
    for (entry, timestamp) in rows {
        match timestamp {
            None => triage.push(item(&entry, None, TriageReason::MissingInsertionTimestamp)),
            Some(value) if chrono::DateTime::parse_from_rfc3339(&value).is_err() => {
                triage.push(item(&entry, None, TriageReason::InvalidInsertionTimestamp));
            }
            Some(value) => valid.push((entry, value)),
        }
    }
    // Propagate quarantine through reversal dependencies, preserving independent valid entries.
    loop {
        let blocked: BTreeSet<_> = triage.iter().map(|row| row.entry_id).collect();
        let before = valid.len();
        valid.retain(|(entry, timestamp)| {
            if entry
                .reversal_of
                .is_some_and(|target| blocked.contains(&target))
            {
                triage.push(item(
                    entry,
                    Some(timestamp.clone()),
                    TriageReason::UnresolvedReversalTarget,
                ));
                false
            } else {
                true
            }
        });
        if before == valid.len() {
            break;
        }
    }
    triage.sort_by_key(|row| row.entry_id);
    (valid, triage)
}
/// Read-only per-Firm timestamp triage, bounded to 1000 legacy rows.
/// Missing source targets remain unresolved until the importer checks PostgreSQL.
pub async fn triage(db: &SurrealDb, firm: Uuid) -> Result<Vec<TriageItem>, JournalError> {
    let (mut pending, mut triage) = classify(read(db, Some(firm)).await?);
    let mut resolved = BTreeSet::new();
    loop {
        let before = pending.len();
        pending.retain(|(entry, _)| {
            if entry
                .reversal_of
                .is_none_or(|target| resolved.contains(&target))
            {
                resolved.insert(entry.id);
                false
            } else {
                true
            }
        });
        if before == pending.len() {
            break;
        }
    }
    triage.extend(pending.into_iter().map(|(entry, timestamp)| {
        item(
            &entry,
            Some(timestamp),
            TriageReason::UnresolvedReversalTarget,
        )
    }));
    triage.sort_by_key(|row| row.entry_id);
    Ok(triage)
}
/// Import timestamped legacy entries atomically, preserving IDs and timestamps.
/// Rows without insertion timestamps and dependent reversals remain in SurrealDB
/// for triage. No timestamps are inferred, fabricated, or repaired automatically.
pub async fn import(db: &SurrealDb, pg: &PostgresDb) -> Result<ImportReport, JournalError> {
    let (mut pending, mut triage) = classify(read(db, None).await?);
    let mut tx = pg.0.begin().await?;
    // Even an empty source must not hide an unprovisioned target schema.
    sqlx::query("SELECT id,legacy_imported_at,legacy_inserted_at_known,legacy_inserted_at_source FROM journal_entry LIMIT 0")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(20261011200001)")
        .execute(&mut *tx)
        .await?;
    let mut count = 0;
    while !pending.is_empty() {
        let before = pending.len();
        let mut deferred = Vec::new();
        for (entry, inserted_at) in pending {
            if let Some(original) = entry.reversal_of {
                let exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM journal_entry WHERE id=$1 AND firm_id=$2)",
                )
                .bind(original)
                .bind(entry.firm_id)
                .fetch_one(&mut *tx)
                .await?;
                if !exists {
                    deferred.push((entry, inserted_at));
                    continue;
                }
            }
            let hash = entry_digest(&entry)?;
            let existing: Vec<(Uuid, String, sqlx::types::Json<JournalEntry>)> = sqlx::query_as(
                "SELECT id,digest,payload FROM journal_entry WHERE id=$1 OR (firm_id=$2 AND command_key=$3)")
                .bind(entry.id).bind(entry.firm_id).bind(&entry.key).fetch_all(&mut *tx).await?;
            if !existing.is_empty() {
                if existing.len() != 1
                    || existing[0].0 != entry.id
                    || existing[0].1 != hash
                    || serde_json::to_value(&existing[0].2 .0)? != serde_json::to_value(&entry)?
                {
                    return Err(JournalError::Conflict);
                }
                continue;
            }
            let firm = crate::firms::find_by_id(db, entry.firm_id)
                .await
                .map_err(|_| JournalError::Invalid("legacy journal Firm lookup failed".into()))?
                .ok_or_else(|| JournalError::Invalid("legacy journal Firm is missing".into()))?;
            crate::firm_journal::ensure_firm_row(&mut tx, &firm).await?;
            sqlx::query("INSERT INTO journal_entry(id,firm_id,command_key,actor_id,occurred_at,memo,payload,digest,reversal_of,inserted_at,legacy_imported_at,legacy_inserted_at_known,legacy_inserted_at_source) VALUES($1,$2,$3,$4,$5::timestamptz,$6,$7,$8,$9,$10::timestamptz,now(),true,$10::text)")
                .bind(entry.id).bind(entry.firm_id).bind(&entry.key).bind(entry.actor_id)
                .bind(&entry.occurred_at).bind(&entry.memo).bind(sqlx::types::Json(&entry))
                .bind(hash).bind(entry.reversal_of).bind(inserted_at).execute(&mut *tx).await?;
            for (position, posting) in entry.postings.iter().enumerate() {
                sqlx::query("INSERT INTO journal_posting(entry_id,firm_id,position,account,unit,quantity,project_id,category,ownership,restrictions) VALUES($1,$2,$3,$4,$5,$6::numeric,$7,$8,$9,$10)")
                    .bind(entry.id).bind(entry.firm_id)
                    .bind(i32::try_from(position).map_err(|_| JournalError::Invalid("legacy posting limit".into()))?)
                    .bind(&posting.account).bind(sqlx::types::Json(&posting.unit)).bind(&posting.quantity)
                    .bind(posting.project_id).bind(serde_json::to_value(posting.category)?.as_str()
                        .ok_or_else(|| JournalError::Invalid("legacy category encoding".into()))?)
                    .bind(serde_json::to_value(posting.ownership)?.as_str().ok_or_else(|| JournalError::Invalid("legacy ownership encoding".into()))?)
                    .bind(sqlx::types::Json(&posting.restrictions))
                    .execute(&mut *tx).await?;
            }
            for source in &entry.sources {
                let id =
                    crate::firm_journal::persist_source(&mut tx, entry.firm_id, source).await?;
                sqlx::query("INSERT INTO journal_entry_source(entry_id,source_id) VALUES($1,$2)")
                    .bind(entry.id)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            count += 1;
        }
        if deferred.len() == before {
            triage.extend(deferred.into_iter().map(|(entry, timestamp)| {
                item(
                    &entry,
                    Some(timestamp),
                    TriageReason::UnresolvedReversalTarget,
                )
            }));
            break;
        }
        pending = deferred;
    }
    tx.commit().await?;
    triage.sort_by_key(|row| row.entry_id);
    Ok(ImportReport {
        imported: count,
        triage,
    })
}
