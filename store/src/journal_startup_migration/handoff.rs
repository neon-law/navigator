//! Read-only verification of captured Journal Entries and their native projections.
use crate::firm_journal::{self as journal, JournalEntry, JournalError, Posting, SourceReference};
use crate::postgres::PostgresDb;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// Immutable input captured while the operator keeps Source writers quiesced.
/// The physical UUID is retained independently of the serialized event identity.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedJournalRow {
    pub id: Uuid,
    pub firm_id: String,
    pub command_key: String,
    pub payload: String,
    pub inserted_at: Option<String>,
}

/// Content-free reasons an immutable captured Journal history cannot be activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalDifferenceReason {
    SourceTimestamp,
    MissingTarget,
    PayloadMismatch,
    HeaderMismatch,
    PostingProjection,
    SourceProjection,
    LegacyProvenance,
    MissingFirm,
    ReversalParent,
    ReversalCycle,
    ReversalNegation,
    DuplicateReversal,
    ActiveMovement,
    MovementEvidence,
    RepaymentShape,
    RepaymentCost,
}

/// Only event/Firm UUIDs leave the proof; keys, payloads and database errors do not.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JournalDifference {
    pub entry_id: Option<Uuid>,
    pub firm_id: Option<Uuid>,
    pub reason: JournalDifferenceReason,
}

/// Conservation and integrity of the exact captured input and one PostgreSQL snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalHandoff {
    pub source_rows: usize,
    pub represented_source_rows: usize,
    pub compared_overlaps: usize,
    pub source_only_remaining: usize,
    pub target_rows: usize,
    pub native_target_only: usize,
    pub unresolved_source_rows: usize,
    pub target_integrity_failures: usize,
    pub differences: Vec<JournalDifference>,
}
impl JournalHandoff {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.compared_overlaps
            .checked_add(self.source_only_remaining)
            == Some(self.source_rows)
            && self.compared_overlaps.checked_add(self.native_target_only) == Some(self.target_rows)
            && self.represented_source_rows <= self.compared_overlaps
            && self
                .represented_source_rows
                .checked_add(self.unresolved_source_rows)
                == Some(self.source_rows)
            && self.source_rows == self.represented_source_rows
            && self.source_only_remaining == 0
            && self.unresolved_source_rows == 0
            && self.target_integrity_failures == 0
            && self.differences.is_empty()
    }
}

/// Parse captured input once using the same physical and logical identity contract as import.
pub(super) fn parse_captured(
    rows: &[CapturedJournalRow],
) -> Result<Vec<(JournalEntry, Option<String>)>, JournalError> {
    if rows.len() > 1000 {
        return Err(invalid_capture());
    }
    let mut ids = BTreeSet::new();
    let mut keys = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let entry: JournalEntry =
                serde_json::from_str(&row.payload).map_err(|_| invalid_capture())?;
            entry.validate().map_err(|_| invalid_capture())?;
            if row.id != entry.id
                || row.firm_id != entry.firm_id.to_string()
                || row.command_key != entry.key
                || !ids.insert(entry.id)
                || !keys.insert((entry.firm_id, entry.key.clone()))
            {
                return Err(invalid_capture());
            }
            Ok((entry, row.inserted_at.clone()))
        })
        .collect()
}
fn invalid_capture() -> JournalError {
    JournalError::Invalid("captured Journal identity or payload is invalid".into())
}

const PAGE_SIZE: i64 = 32;
struct Header {
    id: Uuid,
    firm_id: Uuid,
    command_key: String,
    actor_id: Uuid,
    occurred_at: DateTime<Utc>,
    memo: String,
    payload: String,
    digest: String,
    reversal_of: Option<Uuid>,
    inserted_at: DateTime<Utc>,
    legacy_imported_at: Option<DateTime<Utc>>,
    legacy_inserted_at_known: Option<bool>,
    legacy_inserted_at_source: Option<String>,
}
impl Header {
    fn read(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            firm_id: row.try_get("firm_id")?,
            command_key: row.try_get("command_key")?,
            actor_id: row.try_get("actor_id")?,
            occurred_at: row.try_get("occurred_at")?,
            memo: row.try_get("memo")?,
            payload: row.try_get("payload")?,
            digest: row.try_get("digest")?,
            reversal_of: row.try_get("reversal_of")?,
            inserted_at: row.try_get("inserted_at")?,
            legacy_imported_at: row.try_get("legacy_imported_at")?,
            legacy_inserted_at_known: row.try_get("legacy_inserted_at_known")?,
            legacy_inserted_at_source: row.try_get("legacy_inserted_at_source")?,
        })
    }
}
/// Retain only bounded identity/causal metadata after each event's body/projections are checked.
struct HistoryEvent {
    firm: Uuid,
    key: String,
    reversal: Option<Uuid>,
    repayment: Option<Uuid>,
    movements: Vec<String>,
    failures: BTreeSet<JournalDifferenceReason>,
}
type History = BTreeMap<Uuid, HistoryEvent>;
type Failures = BTreeMap<Uuid, BTreeSet<JournalDifferenceReason>>;
#[derive(Default)]
struct TargetAudit {
    history: History,
    matched: BTreeSet<Uuid>,
    source_failures: Failures,
    differences: BTreeSet<JournalDifference>,
}

async fn timestamp_matches(
    tx: &mut Transaction<'_, Postgres>,
    input: &str,
    actual: DateTime<Utc>,
) -> Result<bool, JournalError> {
    if DateTime::parse_from_rfc3339(input).is_err() {
        return Ok(false);
    }
    Ok(sqlx::query_scalar("SELECT CASE WHEN pg_input_is_valid($1,'timestamp with time zone') THEN $1::timestamptz=$2 ELSE false END")
        .bind(input).bind(actual).fetch_one(&mut **tx).await?)
}
async fn legacy_valid(
    tx: &mut Transaction<'_, Postgres>,
    row: &Header,
) -> Result<bool, JournalError> {
    match (
        &row.legacy_imported_at,
        row.legacy_inserted_at_known,
        &row.legacy_inserted_at_source,
    ) {
        (None, None, None) => Ok(true),
        (Some(_), Some(true), Some(value)) => timestamp_matches(tx, value, row.inserted_at).await,
        _ => Ok(false),
    }
}
fn metadata(entry: &JournalEntry) -> Result<HistoryEvent, JournalError> {
    let movements = entry
        .sources
        .iter()
        .filter(|source| journal::is_movement_source(source))
        .map(|source| journal::source_reference_id(entry.firm_id, source))
        .collect::<Result<_, _>>()?;
    let mut failures = BTreeSet::new();
    if journal::validate_movement_evidence(entry).is_err() {
        failures.insert(JournalDifferenceReason::MovementEvidence);
    }
    if journal::repayment_target(entry).is_err() {
        failures.insert(JournalDifferenceReason::RepaymentShape);
    }
    Ok(HistoryEvent {
        firm: entry.firm_id,
        key: crate::documents::sha256_hex(entry.key.as_bytes()),
        reversal: entry.reversal_of,
        repayment: journal::repayment_target(entry).ok().flatten(),
        movements,
        failures,
    })
}
async fn reversal_reason(
    tx: &mut Transaction<'_, Postgres>,
    entry: &JournalEntry,
    source: Option<&BTreeMap<Uuid, &JournalEntry>>,
) -> Result<Option<JournalDifferenceReason>, JournalError> {
    let Some(parent) = entry.reversal_of else {
        return Ok(None);
    };
    if let Some(original) = source.and_then(|source| source.get(&parent)) {
        return Ok(negation_reason(entry, original));
    }
    let payload: Option<String> =
        sqlx::query_scalar("SELECT payload::text FROM journal_entry WHERE id=$1")
            .bind(parent)
            .fetch_optional(&mut **tx)
            .await?;
    let original: Option<JournalEntry> =
        payload.and_then(|value| serde_json::from_str(&value).ok());
    Ok(match original {
        Some(original) if original.id == parent && original.validate().is_ok() => {
            negation_reason(entry, &original)
        }
        _ => Some(JournalDifferenceReason::ReversalParent),
    })
}
fn negation_reason(
    entry: &JournalEntry,
    original: &JournalEntry,
) -> Option<JournalDifferenceReason> {
    if entry.firm_id != original.firm_id {
        Some(JournalDifferenceReason::ReversalParent)
    } else if !journal::negates_postings(&original.postings, &entry.postings) {
        Some(JournalDifferenceReason::ReversalNegation)
    } else {
        None
    }
}

async fn validate_postings(
    tx: &mut Transaction<'_, Postgres>,
    entry: &JournalEntry,
) -> Result<bool, JournalError> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM journal_posting WHERE entry_id=$1")
        .bind(entry.id)
        .fetch_one(&mut **tx)
        .await?;
    if usize::try_from(count).ok() != Some(entry.postings.len()) {
        return Ok(false);
    }
    // The validated payload caps this read at 1000 postings; extra actual rows fail on count.
    let rows=sqlx::query("SELECT firm_id,position,account,unit,quantity::text AS quantity,project_id,category,ownership,restrictions FROM journal_posting WHERE entry_id=$1 ORDER BY position")
        .bind(entry.id).fetch_all(&mut **tx).await?;
    for (index, (row, expected)) in rows.into_iter().zip(&entry.postings).enumerate() {
        if row.try_get::<Uuid, _>("firm_id")? != entry.firm_id
            || usize::try_from(row.try_get::<i32, _>("position")?).ok() != Some(index)
            || posting(&row).as_ref() != Some(expected)
        {
            return Ok(false);
        }
    }
    Ok(true)
}
fn posting(row: &sqlx::postgres::PgRow) -> Option<Posting> {
    Some(Posting {
        account: row.try_get("account").ok()?,
        unit: serde_json::from_value(
            row.try_get::<sqlx::types::Json<serde_json::Value>, _>("unit")
                .ok()?
                .0,
        )
        .ok()?,
        quantity: row.try_get("quantity").ok()?,
        project_id: row.try_get("project_id").ok()?,
        category: serde_json::from_value(serde_json::Value::String(row.try_get("category").ok()?))
            .ok()?,
        ownership: serde_json::from_value(serde_json::Value::String(
            row.try_get("ownership").ok()?,
        ))
        .ok()?,
        restrictions: serde_json::from_value(
            row.try_get::<sqlx::types::Json<serde_json::Value>, _>("restrictions")
                .ok()?
                .0,
        )
        .ok()?,
    })
}
fn reference(row: &sqlx::postgres::PgRow) -> Result<Option<(Uuid, SourceReference)>, JournalError> {
    let id: String = row.try_get("id")?;
    let firm: Uuid = row.try_get("firm_id")?;
    let payload: String = row.try_get("payload")?;
    let value: SourceReference = match serde_json::from_str(&payload) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let valid = journal::source_reference_id(firm, &value)? == id
        && row.try_get::<String, _>("provider")? == value.provider
        && row.try_get::<String, _>("provider_scope")? == value.scope
        && row.try_get::<String, _>("account")? == value.account
        && row.try_get::<String, _>("object_type")? == value.object_type
        && row.try_get::<String, _>("external_id")? == value.external_id
        && row.try_get::<Option<String>, _>("locator")? == value.locator;
    Ok(valid.then_some((firm, value)))
}
async fn validate_links(
    tx: &mut Transaction<'_, Postgres>,
    entry: &JournalEntry,
) -> Result<bool, JournalError> {
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM journal_entry_source WHERE entry_id=$1")
            .bind(entry.id)
            .fetch_one(&mut **tx)
            .await?;
    if usize::try_from(count).ok() != Some(entry.sources.len()) {
        return Ok(false);
    }
    let expected = entry
        .sources
        .iter()
        .map(|source| Ok((journal::source_reference_id(entry.firm_id, source)?, source)))
        .collect::<Result<BTreeMap<_, _>, JournalError>>()?;
    let rows=sqlx::query("SELECT s.id,s.firm_id,s.provider,s.provider_scope,s.account,s.object_type,s.external_id,s.locator,s.payload::text AS payload FROM journal_entry_source l JOIN journal_source_reference s ON s.id=l.source_id WHERE l.entry_id=$1 ORDER BY s.id")
        .bind(entry.id).fetch_all(&mut **tx).await?;
    for row in rows {
        let id: String = row.try_get("id")?;
        let Some((firm, actual)) = reference(&row)? else {
            return Ok(false);
        };
        if firm != entry.firm_id || expected.get(&id).copied() != Some(&actual) {
            return Ok(false);
        }
    }
    Ok(true)
}
async fn audit_references(
    tx: &mut Transaction<'_, Postgres>,
    differences: &mut BTreeSet<JournalDifference>,
    firm: Option<Uuid>,
) -> Result<(), JournalError> {
    let mut after: Option<String> = None;
    loop {
        let rows=sqlx::query("SELECT id,firm_id,provider,provider_scope,account,object_type,external_id,locator,payload::text AS payload FROM journal_source_reference WHERE ($1::text IS NULL OR id>$1) AND ($3::uuid IS NULL OR firm_id=$3) ORDER BY id LIMIT $2")
            .bind(after.as_deref()).bind(PAGE_SIZE).bind(firm).fetch_all(&mut **tx).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            after = Some(row.try_get("id")?);
            if reference(&row)?.is_none() {
                differences.insert(JournalDifference {
                    entry_id: None,
                    firm_id: Some(row.try_get("firm_id")?),
                    reason: JournalDifferenceReason::SourceProjection,
                });
            }
        }
    }
    Ok(())
}
async fn header_valid(
    tx: &mut Transaction<'_, Postgres>,
    header: &Header,
    entry: &JournalEntry,
) -> Result<bool, JournalError> {
    Ok(entry.validate().is_ok()
        && entry.id == header.id
        && entry.firm_id == header.firm_id
        && entry.key == header.command_key
        && entry.actor_id == header.actor_id
        && entry.memo == header.memo
        && entry.reversal_of == header.reversal_of
        && journal::entry_digest(entry)? == header.digest
        && timestamp_matches(tx, &entry.occurred_at, header.occurred_at).await?)
}
async fn audit_event(
    tx: &mut Transaction<'_, Postgres>,
    header: &Header,
    pending: Option<&BTreeMap<Uuid, &JournalEntry>>,
) -> Result<(HistoryEvent, Option<JournalEntry>), JournalError> {
    use JournalDifferenceReason as Reason;
    let entry: JournalEntry = match serde_json::from_str::<JournalEntry>(&header.payload) {
        Ok(entry) if entry.validate().is_ok() => entry,
        _ => {
            return Ok((
                HistoryEvent {
                    firm: header.firm_id,
                    key: crate::documents::sha256_hex(header.command_key.as_bytes()),
                    reversal: header.reversal_of,
                    repayment: None,
                    movements: Vec::new(),
                    failures: BTreeSet::from([Reason::HeaderMismatch]),
                },
                None,
            ));
        }
    };
    let mut meta = metadata(&entry)?;
    meta.firm = header.firm_id;
    meta.key = crate::documents::sha256_hex(header.command_key.as_bytes());
    meta.reversal = header.reversal_of;
    if !header_valid(tx, header, &entry).await? {
        meta.failures.insert(Reason::HeaderMismatch);
    }
    if !legacy_valid(tx, header).await? {
        meta.failures.insert(Reason::LegacyProvenance);
    }
    if !validate_postings(tx, &entry).await? {
        meta.failures.insert(Reason::PostingProjection);
    }
    if !validate_links(tx, &entry).await? {
        meta.failures.insert(Reason::SourceProjection);
    }
    if let Some(reason) = reversal_reason(tx, &entry, pending).await? {
        meta.failures.insert(reason);
    }
    if let Some(reason) = repayment_reason(tx, &entry, pending).await? {
        meta.failures.insert(reason);
    }
    Ok((meta, Some(entry)))
}
async fn audit_target(
    tx: &mut Transaction<'_, Postgres>,
    source: &BTreeMap<Uuid, &JournalEntry>,
    timestamps: &BTreeMap<Uuid, Option<String>>,
    firm: Option<Uuid>,
) -> Result<TargetAudit, JournalError> {
    let mut out = TargetAudit::default();
    let mut after: Option<Uuid> = None;
    loop {
        let rows=sqlx::query("SELECT id,firm_id,command_key,actor_id,occurred_at,memo,payload::text AS payload,digest,reversal_of,inserted_at,legacy_imported_at,legacy_inserted_at_known,legacy_inserted_at_source FROM journal_entry WHERE ($1::uuid IS NULL OR id>$1) AND ($3::uuid IS NULL OR firm_id=$3) ORDER BY id LIMIT $2")
            .bind(after).bind(PAGE_SIZE).bind(firm).fetch_all(&mut **tx).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let header = Header::read(&row)?;
            after = Some(header.id);
            let (meta, entry) = audit_event(tx, &header, firm.map(|_| source)).await?;
            if let Some(captured) = source.get(&header.id) {
                out.matched.insert(header.id);
                let reasons = out.source_failures.entry(header.id).or_default();
                if entry.as_ref() != Some(*captured) {
                    reasons.insert(JournalDifferenceReason::PayloadMismatch);
                }
                if header.legacy_imported_at.is_some()
                    && header.legacy_inserted_at_source.as_ref()
                        != timestamps.get(&header.id).and_then(Option::as_ref)
                {
                    reasons.insert(JournalDifferenceReason::LegacyProvenance);
                }
            }
            out.history.insert(header.id, meta);
            // Raw payload and actual projections are dropped before the next event.
        }
    }
    audit_references(tx, &mut out.differences, firm).await?;
    Ok(out)
}

fn audit_history(entries: &History) -> Failures {
    use JournalDifferenceReason as Reason;
    let mut bad: Failures = entries
        .iter()
        .filter(|(_, entry)| !entry.failures.is_empty())
        .map(|(id, entry)| (*id, entry.failures.clone()))
        .collect();
    let mut reversals = BTreeMap::<Uuid, Vec<Uuid>>::new();
    for (&id, entry) in entries {
        if let Some(parent) = entry.reversal {
            reversals.entry(parent).or_default().push(id);
        }
        if let Some(parent) = entry.reversal.or(entry.repayment) {
            if entries
                .get(&parent)
                .is_none_or(|original| original.firm != entry.firm)
            {
                bad.entry(id).or_default().insert(parent_reason(entry));
            }
        }
        let mut seen = BTreeSet::new();
        let mut next = Some(id);
        while let Some(current) = next {
            if !seen.insert(current) {
                let repayment_cycle = seen.iter().any(|id| {
                    entries
                        .get(id)
                        .is_some_and(|entry| entry.repayment.is_some())
                });
                for cycle in seen {
                    bad.entry(cycle).or_default().insert(if repayment_cycle {
                        Reason::RepaymentCost
                    } else {
                        Reason::ReversalCycle
                    });
                }
                break;
            }
            next = entries
                .get(&current)
                .and_then(|entry| entry.reversal.or(entry.repayment));
        }
    }
    for children in reversals.values().filter(|children| children.len() > 1) {
        for child in children {
            bad.entry(*child)
                .or_default()
                .insert(Reason::DuplicateReversal);
        }
    }
    active_movements(entries, &reversals, &mut bad);
    loop {
        let before = bad.len();
        for (&id, entry) in entries {
            if entry
                .reversal
                .or(entry.repayment)
                .is_some_and(|parent| bad.contains_key(&parent))
            {
                bad.entry(id).or_default().insert(parent_reason(entry));
            }
        }
        if bad.len() == before {
            break;
        }
    }
    bad
}
fn active_movements(entries: &History, reversals: &BTreeMap<Uuid, Vec<Uuid>>, bad: &mut Failures) {
    let mut movements = BTreeMap::<&str, Vec<(Uuid, &HistoryEvent)>>::new();
    for (&id, entry) in entries.iter().filter(|(id, _)| !reversals.contains_key(id)) {
        for source in &entry.movements {
            movements.entry(source).or_default().push((id, entry));
        }
    }
    for rows in movements.values() {
        for &(id, entry) in rows.iter().filter(|(_, entry)| entry.reversal.is_none()) {
            for &(other, _) in rows
                .iter()
                .filter(|(other, old)| *other != id && old.key != entry.key)
            {
                bad.entry(id)
                    .or_default()
                    .insert(JournalDifferenceReason::ActiveMovement);
                bad.entry(other)
                    .or_default()
                    .insert(JournalDifferenceReason::ActiveMovement);
            }
        }
    }
}
async fn source_metadata(
    tx: &mut Transaction<'_, Postgres>,
    source: &BTreeMap<Uuid, &JournalEntry>,
    timestamps: &BTreeMap<Uuid, Option<String>>,
) -> Result<History, JournalError> {
    let mut out = History::new();
    for (&id, entry) in source {
        let mut meta = metadata(entry)?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM firm WHERE id=$1)")
            .bind(entry.firm_id)
            .fetch_one(&mut **tx)
            .await?;
        if !exists {
            meta.failures.insert(JournalDifferenceReason::MissingFirm);
        }
        let valid = match timestamps.get(&id).and_then(Option::as_ref) {
            Some(value) if DateTime::parse_from_rfc3339(value).is_ok() => {
                sqlx::query_scalar::<_, bool>(
                    "SELECT pg_input_is_valid($1,'timestamp with time zone')",
                )
                .bind(value)
                .fetch_one(&mut **tx)
                .await?
            }
            _ => false,
        };
        if !valid {
            meta.failures
                .insert(JournalDifferenceReason::SourceTimestamp);
        }
        if let Some(reason) = reversal_reason(tx, entry, Some(source)).await? {
            meta.failures.insert(reason);
        }
        if let Some(reason) = repayment_reason(tx, entry, Some(source)).await? {
            meta.failures.insert(reason);
        }
        out.insert(id, meta);
    }
    Ok(out)
}
fn add_differences(out: &mut BTreeSet<JournalDifference>, failures: &Failures, history: &History) {
    for (&id, reasons) in failures {
        for &reason in reasons {
            out.insert(JournalDifference {
                entry_id: Some(id),
                firm_id: history.get(&id).map(|entry| entry.firm),
                reason,
            });
        }
    }
}

/// Verify immutable captured rows against one read-only repeatable-read target snapshot.
/// Raw target bodies/projections are bounded to one keyset page and one event, respectively;
/// only identity, causal edges and canonical movement identities survive the page.
/// No Source query, import, reconciliation, DDL or metadata update occurs here.
///
/// # Errors
/// Captured identities/payloads are malformed or PostgreSQL cannot provide the snapshot.
pub async fn handoff_captured(
    rows: &[CapturedJournalRow],
    pg: &PostgresDb,
) -> Result<JournalHandoff, JournalError> {
    let parsed = parse_captured(rows)?;
    let source = parsed.iter().map(|(entry, _)| (entry.id, entry)).collect();
    let timestamps = parsed
        .iter()
        .map(|(entry, at)| (entry.id, at.clone()))
        .collect();
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let mut target = audit_target(&mut tx, &source, &timestamps, None).await?;
    let native_failures = audit_history(&target.history);
    add_differences(&mut target.differences, &native_failures, &target.history);
    let target_rows = target.history.len();
    let target_integrity_failures = native_failures.len();
    let source_history = source_metadata(&mut tx, &source, &timestamps).await?;
    target.history.extend(source_history);
    let combined_failures = audit_history(&target.history);
    let mut represented = 0;
    for &id in source.keys() {
        let reasons = target.source_failures.entry(id).or_default();
        if !target.matched.contains(&id) {
            reasons.insert(JournalDifferenceReason::MissingTarget);
        }
        if let Some(other) = combined_failures.get(&id) {
            reasons.extend(other);
        }
        if let Some(other) = native_failures.get(&id) {
            reasons.extend(other);
        }
        if reasons.is_empty() {
            represented += 1;
        }
    }
    add_differences(
        &mut target.differences,
        &target.source_failures,
        &target.history,
    );
    let overlaps = target.matched.len();
    let report = JournalHandoff {
        source_rows: rows.len(),
        represented_source_rows: represented,
        compared_overlaps: overlaps,
        source_only_remaining: rows.len() - overlaps,
        target_rows,
        native_target_only: target_rows - overlaps,
        unresolved_source_rows: rows.len() - represented,
        target_integrity_failures,
        differences: target.differences.into_iter().collect(),
    };
    tx.rollback().await?;
    Ok(report)
}

fn parent_reason(entry: &HistoryEvent) -> JournalDifferenceReason {
    if entry.reversal.is_some() {
        JournalDifferenceReason::ReversalParent
    } else {
        JournalDifferenceReason::RepaymentCost
    }
}
async fn repayment_reason(
    tx: &mut Transaction<'_, Postgres>,
    entry: &JournalEntry,
    source: Option<&BTreeMap<Uuid, &JournalEntry>>,
) -> Result<Option<JournalDifferenceReason>, JournalError> {
    let target = match journal::repayment_target(entry) {
        Ok(Some(target)) => target,
        Ok(None) => return Ok(None),
        Err(_) => return Ok(Some(JournalDifferenceReason::RepaymentShape)),
    };
    if target == entry.id {
        return Ok(Some(JournalDifferenceReason::RepaymentCost));
    }
    if let Some(cost) = source.and_then(|source| source.get(&target)) {
        return Ok(repayment_cost_reason(entry, cost));
    }
    let payload: Option<String> =
        sqlx::query_scalar("SELECT payload::text FROM journal_entry WHERE id=$1")
            .bind(target)
            .fetch_optional(&mut **tx)
            .await?;
    let cost: Option<JournalEntry> =
        payload.and_then(|payload| serde_json::from_str(&payload).ok());
    Ok(match cost {
        Some(cost) if cost.id == target && cost.validate().is_ok() => {
            repayment_cost_reason(entry, &cost)
        }
        _ => Some(JournalDifferenceReason::RepaymentCost),
    })
}
fn repayment_cost_reason(
    entry: &JournalEntry,
    cost: &JournalEntry,
) -> Option<JournalDifferenceReason> {
    if cost.firm_id != entry.firm_id {
        return Some(JournalDifferenceReason::RepaymentCost);
    }
    for posting in entry
        .postings
        .iter()
        .filter(|posting| posting.category == journal::Category::Reimbursement)
    {
        if !journal::quantity(&posting.quantity).is_ok_and(|value| value < 0) {
            return Some(JournalDifferenceReason::RepaymentShape);
        }
        let incurred = cost
            .postings
            .iter()
            .filter(|candidate| {
                candidate.category == journal::Category::AdvancedCost
                    && candidate.project_id == posting.project_id
                    && candidate.unit == posting.unit
            })
            .try_fold(0_i128, |sum, candidate| {
                journal::quantity(&candidate.quantity)
                    .ok()
                    .and_then(|quantity| sum.checked_add(quantity))
            });
        if incurred.is_none_or(|value| value <= 0) {
            return Some(JournalDifferenceReason::RepaymentCost);
        }
    }
    // Native capacity and cost-unreversed checks are insertion-time rules. Final-state
    // history cannot infer their old ordering or impose a new gross/net repayment cap.
    None
}

/// Validate one physically parsed, timestamped, same-Firm component in the caller's
/// transaction and Firm lock. Pending rows are an overlay, not a containment failure.
pub(super) async fn validate_import_on(
    tx: &mut Transaction<'_, Postgres>,
    component: &[(&JournalEntry, &str)],
) -> Result<(), JournalError> {
    let Some((first, _)) = component.first() else {
        return Ok(());
    };
    if component.len() > 1000
        || component
            .iter()
            .any(|(entry, _)| entry.firm_id != first.firm_id || entry.validate().is_err())
    {
        return Err(invalid_capture());
    }
    let source: BTreeMap<_, _> = component
        .iter()
        .map(|(entry, _)| (entry.id, *entry))
        .collect();
    let keys: BTreeSet<_> = component
        .iter()
        .map(|(entry, _)| (entry.firm_id, entry.key.as_str()))
        .collect();
    if source.len() != component.len() || keys.len() != component.len() {
        return Err(invalid_capture());
    }
    let timestamps = component
        .iter()
        .map(|(entry, at)| (entry.id, Some((*at).to_owned())))
        .collect();
    let mut target = audit_target(tx, &source, &timestamps, Some(first.firm_id)).await?;
    if !target.differences.is_empty()
        || target
            .history
            .values()
            .any(|entry| !entry.failures.is_empty())
        || target
            .source_failures
            .values()
            .any(|reasons| !reasons.is_empty())
    {
        return Err(JournalError::Conflict);
    }
    let pending = source_metadata(tx, &source, &timestamps).await?;
    // PostgreSQL's immutable command key must not be reassigned by an insert-only import.
    for (&id, entry) in &pending {
        if target
            .history
            .iter()
            .any(|(old, stored)| *old != id && stored.firm == entry.firm && stored.key == entry.key)
        {
            return Err(JournalError::Conflict);
        }
    }
    target.history.extend(pending);
    if !audit_history(&target.history).is_empty() {
        return Err(JournalError::Conflict);
    }
    Ok(())
}
