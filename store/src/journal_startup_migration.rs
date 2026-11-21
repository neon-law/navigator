//! Explicit operator import from the read-only legacy journal.
//! PostgreSQL is authoritative: conflicts refuse the import instead of overwriting it.
use crate::firm_journal::{entry_digest, JournalEntry, JournalError};
use crate::postgres::PostgresDb;
use serde::Serialize;
use std::collections::BTreeSet;
use uuid::Uuid;

mod handoff;
pub use handoff::{
    handoff_captured, CapturedJournalRow, JournalDifference, JournalDifferenceReason,
    JournalHandoff,
};

mod source;
pub use source::{capture, import, import_firm, triage};

/// Read-only input to the single PostgreSQL importer. Implementations expose no write capability.
#[async_trait::async_trait]
trait ImportInput: Send + Sync {
    async fn rows(&self, firm: Option<Uuid>) -> Result<Vec<CapturedJournalRow>, JournalError>;
    async fn firm(&self, id: Uuid) -> anyhow::Result<Option<crate::firms::Firm>>;
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
    input: &dyn ImportInput,
    firm: Option<Uuid>,
) -> Result<Vec<(JournalEntry, Option<String>)>, JournalError> {
    let rows = input.rows(firm).await?;
    require_review_bound(rows.len())?;
    handoff::parse_captured(&rows)
}
fn require_review_bound(rows: usize) -> Result<(), JournalError> {
    if rows > 1000 {
        return Err(JournalError::Invalid(
            "journal import exceeds 1000-row review bound".into(),
        ));
    }
    Ok(())
}
fn classify_timestamps(rows: &[(JournalEntry, Option<String>)]) -> Vec<TriageItem> {
    let mut triage = Vec::new();
    let mut blocked = BTreeSet::new();
    for (entry, timestamp) in rows {
        let reason = match timestamp {
            None => Some(TriageReason::MissingInsertionTimestamp),
            Some(value) if chrono::DateTime::parse_from_rfc3339(value).is_err() => {
                Some(TriageReason::InvalidInsertionTimestamp)
            }
            Some(_) => None,
        };
        if let Some(reason) = reason {
            blocked.insert(entry.id);
            triage.push(item(entry, None, reason));
        }
    }
    loop {
        let before = blocked.len();
        for (entry, timestamp) in rows {
            if !blocked.contains(&entry.id)
                && entry
                    .reversal_of
                    .is_some_and(|parent| blocked.contains(&parent))
            {
                blocked.insert(entry.id);
                triage.push(item(
                    entry,
                    timestamp.clone(),
                    TriageReason::UnresolvedReversalTarget,
                ));
            }
        }
        if before == blocked.len() {
            break;
        }
    }
    triage.sort_by_key(|row| row.entry_id);
    triage
}
fn classify(
    rows: Vec<(JournalEntry, Option<String>)>,
) -> (Vec<(JournalEntry, String)>, Vec<TriageItem>) {
    let triage = classify_timestamps(&rows);
    let blocked: BTreeSet<_> = triage.iter().map(|row| row.entry_id).collect();
    let valid = rows
        .into_iter()
        .filter_map(|(entry, timestamp)| {
            if blocked.contains(&entry.id) {
                None
            } else {
                timestamp.map(|timestamp| (entry, timestamp))
            }
        })
        .collect();
    (valid, triage)
}
/// Read-only per-Firm timestamp triage, bounded to 1000 legacy rows.
/// Missing source targets remain unresolved until the importer checks PostgreSQL.
async fn triage_input(
    input: &dyn ImportInput,
    firm: Uuid,
) -> Result<Vec<TriageItem>, JournalError> {
    let (mut pending, mut triage) = classify(read(input, Some(firm)).await?);
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
async fn import_selected(
    input: &dyn ImportInput,
    pg: &PostgresDb,
    firm: Option<Uuid>,
) -> Result<ImportReport, JournalError> {
    require_import_admission(pg).await?;
    let (pending, mut triage) = classify(read(input, firm).await?);
    let mut tx = pg.0.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *tx)
        .await?;
    // Even an empty source must not hide an unprovisioned target schema.
    sqlx::query("SELECT id,legacy_imported_at,legacy_inserted_at_known,legacy_inserted_at_source FROM journal_entry LIMIT 0")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(20261011200001)")
        .execute(&mut *tx)
        .await?;
    lock_import_firms(input, pg, &mut tx, &pending).await?;
    require_import_admission(pg).await?;
    let (mut pending, unresolved) = resolvable_reversals(&mut tx, pending).await?;
    triage.extend(unresolved);
    validate_import_firms(&mut tx, &pending).await?;
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
            count +=
                usize::from(append_entry_on(&mut tx, &entry, &inserted_at, Some(input)).await?);
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

async fn append_entry_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entry: &JournalEntry,
    inserted_at: &str,
    source: Option<&dyn ImportInput>,
) -> Result<bool, JournalError> {
    let hash = entry_digest(entry)?;
    let existing: Vec<(Uuid, String, sqlx::types::Json<JournalEntry>)> = sqlx::query_as(
    "SELECT id,digest,payload FROM journal_entry WHERE id=$1 OR (firm_id=$2 AND command_key=$3)")
    .bind(entry.id).bind(entry.firm_id).bind(&entry.key).fetch_all(&mut **tx).await?;
    if !existing.is_empty() {
        if existing.len() != 1
            || existing[0].0 != entry.id
            || existing[0].1 != hash
            || serde_json::to_value(&existing[0].2 .0)? != serde_json::to_value(entry)?
        {
            return Err(JournalError::Conflict);
        }
        return Ok(false);
    }
    if let Some(source) = source {
        let firm = source
            .firm(entry.firm_id)
            .await
            .map_err(|_| JournalError::Invalid("legacy journal Firm lookup failed".into()))?
            .ok_or_else(|| JournalError::Invalid("legacy journal Firm is missing".into()))?;
        crate::firm_journal::ensure_firm_row(tx, &firm).await?;
    }
    sqlx::query("INSERT INTO journal_entry(id,firm_id,command_key,actor_id,occurred_at,memo,payload,digest,reversal_of,inserted_at,legacy_imported_at,legacy_inserted_at_known,legacy_inserted_at_source) VALUES($1,$2,$3,$4,$5::timestamptz,$6,$7,$8,$9,$10::timestamptz,now(),true,$10::text)")
    .bind(entry.id).bind(entry.firm_id).bind(&entry.key).bind(entry.actor_id)
    .bind(&entry.occurred_at).bind(&entry.memo).bind(sqlx::types::Json(&entry))
    .bind(hash).bind(entry.reversal_of).bind(inserted_at).execute(&mut **tx).await?;
    for (position, posting) in entry.postings.iter().enumerate() {
        sqlx::query("INSERT INTO journal_posting(entry_id,firm_id,position,account,unit,quantity,project_id,category,ownership,restrictions) VALUES($1,$2,$3,$4,$5,$6::numeric,$7,$8,$9,$10)")
        .bind(entry.id).bind(entry.firm_id)
        .bind(i32::try_from(position).map_err(|_| JournalError::Invalid("legacy posting limit".into()))?)
        .bind(&posting.account).bind(sqlx::types::Json(&posting.unit)).bind(&posting.quantity)
        .bind(posting.project_id).bind(serde_json::to_value(posting.category)?.as_str()
            .ok_or_else(|| JournalError::Invalid("legacy category encoding".into()))?)
        .bind(serde_json::to_value(posting.ownership)?.as_str().ok_or_else(|| JournalError::Invalid("legacy ownership encoding".into()))?)
        .bind(sqlx::types::Json(&posting.restrictions))
        .execute(&mut **tx).await?;
    }
    for source in &entry.sources {
        let id = crate::firm_journal::persist_source(tx, entry.firm_id, source).await?;
        sqlx::query("INSERT INTO journal_entry_source(entry_id,source_id) VALUES($1,$2)")
            .bind(entry.id)
            .bind(id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(true)
}

/// Observed insert/retained visits for one requested logical key, not a durable total.
#[derive(Debug, Serialize)]
pub struct CapturedImportReport {
    pub requested_entry_id: Uuid,
    pub imported_requested: usize,
    pub imported_dependencies: usize,
    pub retained_dependencies: usize,
}

/// Refuse explicit operator imports after PostgreSQL has acquired this table.
pub async fn require_import_admission(pg: &PostgresDb) -> Result<(), JournalError> {
    let owned = crate::postgres_backfill::family::active_for_table(pg, "journal_entry")
        .await
        .map_err(|_| JournalError::Invalid("Journal ownership history is unavailable".into()))?;
    if owned {
        return Err(JournalError::Invalid(
            "Journal history is PostgreSQL owned".into(),
        ));
    }
    Ok(())
}

/// Copy one bounded captured dependency component under the native Firm lock.
/// The caller binds these immutable rows to the enrollment receipt before calling.
/// Requested-key acknowledgements remain distinct from companion insert counts.
pub async fn import_captured_event(
    rows: &[CapturedJournalRow],
    pg: &PostgresDb,
    requested: Uuid,
) -> Result<CapturedImportReport, JournalError> {
    if !crate::postgres_backfill::family::importing("journal_entry")
        || crate::postgres_backfill::family::Family::for_table("journal_entry").is_none()
        || !crate::postgres_mirror::source_import_allowed(pg, "journal_entry")
            .await
            .map_err(|_| JournalError::Invalid("Journal import history is unavailable".into()))?
    {
        return Err(JournalError::Invalid(
            "Journal import is not pending".into(),
        ));
    }
    let parsed = handoff::parse_captured(rows)?;
    let selected = captured_component(&parsed, requested)?;
    let triaged: BTreeSet<_> = classify_timestamps(&parsed)
        .into_iter()
        .map(|item| item.entry_id)
        .collect();
    if selected
        .iter()
        .any(|index| triaged.contains(&parsed[*index].0.id))
    {
        return Err(JournalError::Invalid(
            "captured Journal component requires triage".into(),
        ));
    }
    let component: Vec<_> = selected
        .iter()
        .map(|index| {
            let (entry, timestamp) = &parsed[*index];
            timestamp
                .as_deref()
                .map(|timestamp| (entry, timestamp))
                .ok_or_else(|| JournalError::Invalid("captured Journal timestamp is absent".into()))
        })
        .collect::<Result<_, _>>()?;
    let firm = component
        .first()
        .map(|(entry, _)| entry.firm_id)
        .ok_or_else(|| JournalError::Invalid("captured Journal component is empty".into()))?;
    let mut tx = pg.0.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock(20261011200001)")
        .execute(&mut *tx)
        .await?;
    let locked: Option<Uuid> = sqlx::query_scalar("SELECT id FROM firm WHERE id=$1 FOR UPDATE")
        .bind(firm)
        .fetch_optional(&mut *tx)
        .await?;
    if locked != Some(firm) {
        return Err(JournalError::Invalid(
            "captured Journal Firm is missing".into(),
        ));
    }
    handoff::validate_import_on(&mut tx, &component).await?;
    let mut report = CapturedImportReport {
        requested_entry_id: requested,
        imported_requested: 0,
        imported_dependencies: 0,
        retained_dependencies: 0,
    };
    let mut pending = component;
    while !pending.is_empty() {
        let before = pending.len();
        let mut deferred = Vec::new();
        for (entry, timestamp) in pending {
            if let Some(parent) = entry.reversal_of {
                let exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM journal_entry WHERE id=$1 AND firm_id=$2)",
                )
                .bind(parent)
                .bind(firm)
                .fetch_one(&mut *tx)
                .await?;
                if !exists {
                    deferred.push((entry, timestamp));
                    continue;
                }
            }
            let inserted = append_entry_on(&mut tx, entry, timestamp, None).await?;
            if entry.id == requested {
                report.imported_requested += usize::from(inserted);
            } else if inserted {
                report.imported_dependencies += 1;
            } else {
                report.retained_dependencies += 1;
            }
        }
        if deferred.len() == before {
            return Err(JournalError::Invalid(
                "captured Journal dependency is unresolved".into(),
            ));
        }
        pending = deferred;
    }
    tx.commit().await?;
    Ok(report)
}

fn captured_component(
    rows: &[(JournalEntry, Option<String>)],
    requested: Uuid,
) -> Result<BTreeSet<usize>, JournalError> {
    use crate::firm_journal::{is_movement_source, source_reference_id, Category, LinkKind};
    use std::collections::BTreeMap;
    let indices: BTreeMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(index, (entry, _))| (entry.id, index))
        .collect();
    let requested_index = indices
        .get(&requested)
        .ok_or_else(|| JournalError::Invalid("requested Journal event is not captured".into()))?;
    let firm = rows[*requested_index].0.firm_id;
    let mut children = BTreeMap::<Uuid, Vec<Uuid>>::new();
    let mut movements = BTreeMap::<String, Vec<Uuid>>::new();
    let mut movement_ids = BTreeMap::<Uuid, Vec<String>>::new();
    for (entry, _) in rows.iter().filter(|(entry, _)| entry.firm_id == firm) {
        if let Some(parent) = entry.reversal_of {
            children.entry(parent).or_default().push(entry.id);
        }
        if entry.reversal_of.is_none() {
            for source in entry
                .sources
                .iter()
                .filter(|source| is_movement_source(source))
            {
                let id = source_reference_id(firm, source)?;
                movements.entry(id.clone()).or_default().push(entry.id);
                movement_ids.entry(entry.id).or_default().push(id);
            }
            if entry
                .postings
                .iter()
                .any(|posting| posting.category == Category::Reimbursement)
            {
                for link in entry
                    .links
                    .iter()
                    .filter(|link| link.kind == LinkKind::Repayment)
                {
                    children.entry(link.target_id).or_default().push(entry.id);
                }
            }
        }
    }
    let mut selected = BTreeSet::new();
    let mut visited_movements = BTreeSet::new();
    let mut pending = vec![requested];
    while let Some(id) = pending.pop() {
        let index = *indices
            .get(&id)
            .ok_or_else(|| JournalError::Invalid("captured Journal dependency is absent".into()))?;
        if !selected.insert(index) {
            continue;
        }
        let entry = &rows[index].0;
        if entry.firm_id != firm {
            return Err(JournalError::Conflict);
        }
        let mut parents: Vec<_> = entry.reversal_of.into_iter().collect();
        if entry.reversal_of.is_none()
            && entry
                .postings
                .iter()
                .any(|posting| posting.category == Category::Reimbursement)
        {
            parents.extend(
                entry
                    .links
                    .iter()
                    .filter(|link| link.kind == LinkKind::Repayment)
                    .map(|link| link.target_id),
            );
        }
        for parent in parents {
            if let Some(index) = indices.get(&parent) {
                if rows[*index].0.firm_id != firm {
                    return Err(JournalError::Conflict);
                }
                pending.push(parent);
            }
        }
        if let Some(children) = children.get(&id) {
            pending.extend(children);
        }
        if let Some(keys) = movement_ids.get(&id) {
            for key in keys
                .iter()
                .filter(|key| visited_movements.insert((*key).clone()))
            {
                if let Some(neighbors) = movements.get(key) {
                    pending.extend(neighbors);
                }
            }
        }
    }
    Ok(selected)
}

async fn lock_import_firms(
    source: &dyn ImportInput,
    pg: &PostgresDb,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: &[(JournalEntry, String)],
) -> Result<(), JournalError> {
    let firms: BTreeSet<_> = rows.iter().map(|(entry, _)| entry.firm_id).collect();
    for id in firms {
        let locked: Option<Uuid> = sqlx::query_scalar("SELECT id FROM firm WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
        if locked.is_some() {
            continue;
        }
        // Without a Firm there can be no retained Journal parent. A reversal-only
        // source group remains triage without creating an unused Firm row.
        if rows
            .iter()
            .filter(|(entry, _)| entry.firm_id == id)
            .all(|(entry, _)| entry.reversal_of.is_some())
        {
            continue;
        }
        if crate::postgres_backfill::family::active_for_table(pg, "firm")
            .await
            .map_err(|_| JournalError::Invalid("Firm ownership history is unavailable".into()))?
        {
            return Err(JournalError::Invalid(
                "canonical Journal Firm is missing".into(),
            ));
        }
        let firm = source
            .firm(id)
            .await
            .map_err(|_| JournalError::Invalid("legacy Journal Firm lookup failed".into()))?
            .ok_or_else(|| JournalError::Invalid("legacy Journal Firm is missing".into()))?;
        crate::firm_journal::ensure_firm_row(tx, &firm).await?;
        let locked: Option<Uuid> = sqlx::query_scalar("SELECT id FROM firm WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
        if locked != Some(id) {
            return Err(JournalError::Conflict);
        }
    }
    Ok(())
}

async fn resolvable_reversals(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: Vec<(JournalEntry, String)>,
) -> Result<(Vec<(JournalEntry, String)>, Vec<TriageItem>), JournalError> {
    use std::collections::BTreeMap;
    let parents: Vec<_> = rows
        .iter()
        .filter_map(|(entry, _)| entry.reversal_of)
        .collect();
    let existing: Vec<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id,firm_id FROM journal_entry WHERE id=ANY($1::uuid[])")
            .bind(parents)
            .fetch_all(&mut **tx)
            .await?;
    let mut available: BTreeMap<_, _> = existing.into_iter().collect();
    let mut ready = BTreeSet::new();
    loop {
        let before = ready.len();
        for (entry, _) in &rows {
            if entry
                .reversal_of
                .is_none_or(|parent| available.get(&parent) == Some(&entry.firm_id))
            {
                available.insert(entry.id, entry.firm_id);
                ready.insert(entry.id);
            }
        }
        if before == ready.len() {
            break;
        }
    }
    let (valid, unresolved): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .partition(|(entry, _)| ready.contains(&entry.id));
    let triage = unresolved
        .into_iter()
        .map(|(entry, timestamp)| {
            item(
                &entry,
                Some(timestamp),
                TriageReason::UnresolvedReversalTarget,
            )
        })
        .collect();
    Ok((valid, triage))
}

async fn validate_import_firms(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: &[(JournalEntry, String)],
) -> Result<(), JournalError> {
    let firms: BTreeSet<_> = rows.iter().map(|(entry, _)| entry.firm_id).collect();
    for firm in firms {
        let component: Vec<_> = rows
            .iter()
            .filter(|(entry, _)| entry.firm_id == firm)
            .map(|(entry, timestamp)| (entry, timestamp.as_str()))
            .collect();
        handoff::validate_import_on(tx, &component).await?;
    }
    Ok(())
}

#[cfg(test)]
mod postgres_tests;
