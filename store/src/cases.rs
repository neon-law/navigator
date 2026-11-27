//! `store::cases` — the litigation module's commands (#686).
//!
//! Two layers, and the split is the design:
//!
//! 1. A **generic spine**, [`DocketEntry`], modelled on how a court's own
//!    docket works — a numbered list of typed entries. A new niche
//!    instrument type is one [`EntryKind`] value, never a schema change.
//! 2. **Structured tables where structure is earned.** A device lives on
//!    the spine alone until it needs internal structure, and then
//!    graduates. Written discovery is the first to graduate.
//!
//! # These tables live in SurrealDB
//!
//! `cases`, `case_docket_entries`, `discovery_requests`, and
//! `discovery_items` moved with wave six of #1093 (ENG-160), in the
//! litigation slice.

pub use model::cases::{escalated_filing_name, filing_name, FilingNameError};

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashSet;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::cases as authority;

/// What a docket entry is.
///
/// Closed but **code-extended**: adding a niche instrument is a variant
/// here plus one entry in the schema's `kind` ASSERT, not a schema change
/// to the spine. Deliberately spans the stages of litigation rather than
/// enumerating every device up front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Pleading,
    Motion,
    Opposition,
    Reply,
    Order,
    Notice,
    Stipulation,
    DiscoveryRequest,
    DiscoveryResponse,
    Subpoena,
    ExpertDisclosure,
    Hearing,
    Trial,
    Appeal,
    Settlement,
    /// An entry that is genuinely none of the above. Present so the spine
    /// never blocks recording something a court did.
    Other,
}

impl EntryKind {
    pub const ALL: &'static [EntryKind] = &[
        EntryKind::Pleading,
        EntryKind::Motion,
        EntryKind::Opposition,
        EntryKind::Reply,
        EntryKind::Order,
        EntryKind::Notice,
        EntryKind::Stipulation,
        EntryKind::DiscoveryRequest,
        EntryKind::DiscoveryResponse,
        EntryKind::Subpoena,
        EntryKind::ExpertDisclosure,
        EntryKind::Hearing,
        EntryKind::Trial,
        EntryKind::Appeal,
        EntryKind::Settlement,
        EntryKind::Other,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Pleading => "pleading",
            EntryKind::Motion => "motion",
            EntryKind::Opposition => "opposition",
            EntryKind::Reply => "reply",
            EntryKind::Order => "order",
            EntryKind::Notice => "notice",
            EntryKind::Stipulation => "stipulation",
            EntryKind::DiscoveryRequest => "discovery_request",
            EntryKind::DiscoveryResponse => "discovery_response",
            EntryKind::Subpoena => "subpoena",
            EntryKind::ExpertDisclosure => "expert_disclosure",
            EntryKind::Hearing => "hearing",
            EntryKind::Trial => "trial",
            EntryKind::Appeal => "appeal",
            EntryKind::Settlement => "settlement",
            EntryKind::Other => "other",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<EntryKind> {
        Self::ALL.iter().copied().find(|k| k.as_str() == value)
    }

    /// Hearings and trials are appearances: they carry `scheduled_on`.
    #[must_use]
    pub fn is_appearance(self) -> bool {
        matches!(self, Self::Hearing | Self::Trial)
    }
}

/// A written-discovery device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    /// Numbered written questions. The formal device the glossary
    /// reserves *Inquiry* as the generic noun for.
    Interrogatories,
    RequestsForProduction,
    RequestsForAdmission,
}

impl Device {
    pub const ALL: &'static [Device] = &[
        Device::Interrogatories,
        Device::RequestsForProduction,
        Device::RequestsForAdmission,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Device::Interrogatories => "interrogatories",
            Device::RequestsForProduction => "requests_for_production",
            Device::RequestsForAdmission => "requests_for_admission",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Device> {
        Self::ALL.iter().copied().find(|d| d.as_str() == value)
    }
}

/// Errors from the litigation commands.
#[derive(Debug, thiserror::Error)]
pub enum CaseError {
    #[error("`{0}` is not a recognized docket entry kind")]
    UnknownEntryKind(String),
    #[error("`{0}` is not a recognized discovery device")]
    UnknownDevice(String),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a {0} returned no usable row")]
    WriteReturnedNothing(&'static str),
    /// The item named by [`answer_item`] does not exist.
    #[error("no discovery item with id {0}")]
    ItemNotFound(Uuid),
    /// The docket entry named by [`link_document`] or [`supersede`] does
    /// not exist.
    #[error("no docket entry with id {0}")]
    EntryNotFound(Uuid),
    /// [`supersede`] was asked to continue an entry that is not a hearing
    /// or a trial; only an appearance carries a date a continuance moves.
    #[error("a `{0}` entry is not an appearance and cannot be superseded")]
    NotAnAppearance(String),
    /// A `kind`/`scheduled_on` pair the schema's ASSERT refuses on the
    /// SurrealDB path; the PostgreSQL path enforces the same shape.
    #[error("a hearing or trial entry requires a scheduled date and no other kind may carry one")]
    ScheduledOnRule,
    /// PostgreSQL, under the litigation authority, failed.
    #[error(transparent)]
    Postgres(#[from] authority::CasesAuthorityError),
}

// Raw sqlx calls inside the transaction helpers surface through the
// litigation authority's error, which `CaseError::Postgres` already wraps.
impl From<sqlx::Error> for CaseError {
    fn from(err: sqlx::Error) -> Self {
        CaseError::Postgres(err.into())
    }
}

/// One litigation matter before one forum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Case {
    pub id: Uuid,
    /// The matter this case belongs to.
    pub project_id: Uuid,
    /// The case caption as filed.
    pub caption: String,
    /// The court or tribunal.
    pub forum: Option<String>,
    pub jurisdiction: Option<String>,
    /// The forum's own docket number.
    pub docket_number: Option<String>,
    /// The presiding judge — part of the case masthead alongside caption,
    /// court, and docket number.
    pub judge: Option<String>,
    /// Which side the client is on.
    pub posture: String,
    /// `open`, `stayed`, or `closed`.
    pub status: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One entry on a case's docket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocketEntry {
    pub id: Uuid,
    pub case_id: Uuid,
    /// Per-case ordinal as text, so `29-1` round-trips exactly.
    pub entry_number: String,
    /// The closed, code-extended entry type.
    pub kind: String,
    pub title: String,
    /// Who filed or served it.
    pub party: Option<String>,
    /// RFC 3339 date filed or served.
    pub filed_or_served_on: Option<String>,
    /// When the court set a hearing or trial. Required for those kinds,
    /// absent for every other. Historical rows that predate the field stay
    /// `None` until an operator records the date; they are never backfilled.
    pub scheduled_on: Option<DateTime<Utc>>,
    /// The prior appearance this continuance replaces. The superseded
    /// entry keeps its date; the calendar follows the chain to the entry
    /// no later entry points at.
    pub supersedes: Option<Uuid>,
    /// `None` means *source pending*.
    pub document_asset_id: Option<Uuid>,
    /// Set when the firm drafted it.
    pub notation_id: Option<Uuid>,
    /// The case number as stamped by the court's own CM/ECF header, not as
    /// typed by whoever recorded the entry (LAW-24). `None` until
    /// [`record_stamp`] extracts one from the filed document.
    pub stamp_case_number: Option<String>,
    /// The document (entry) number as stamped by CM/ECF. Compare against
    /// `entry_number` to catch a hand-typed docket number that disagrees
    /// with the document itself.
    pub stamp_entry_number: Option<String>,
    /// The filing date as stamped by CM/ECF, verbatim (`MM/DD/YY` or
    /// `MM/DD/YYYY`) rather than parsed, so an unrecognized format is still
    /// recorded.
    pub stamp_filed_on: Option<String>,
    /// When this entry's document was last hash-checked against an
    /// independently fetched copy (LAW-24, option 1). `None` until
    /// [`record_integrity_check`] runs.
    pub integrity_checked_at: Option<DateTime<Utc>>,
    /// The SHA-256 of the independently fetched copy the check compared
    /// against. Kept alongside the result so a later reviewer can see what
    /// was actually fetched, not just whether it matched.
    pub integrity_source_sha256_hex: Option<String>,
    /// Whether the independently fetched copy's hash matched the stored
    /// document's. `None` before any check has run — distinct from
    /// `Some(false)`, a check that ran and found a mismatch.
    pub integrity_matched: Option<bool>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A hearing or trial the calendar should show: the current appearance on
/// a continuance chain, with a date still in the future.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Appearance {
    pub id: Uuid,
    pub case_id: Uuid,
    pub project_id: Uuid,
    pub kind: String,
    pub title: String,
    pub scheduled_on: DateTime<Utc>,
}

impl Appearance {
    /// The calendar's date cell — UTC, minute precision.
    #[must_use]
    pub fn calendar_date(&self) -> String {
        self.scheduled_on.format("%Y-%m-%d %H:%M UTC").to_string()
    }

    /// The calendar's status cell.
    #[must_use]
    pub fn calendar_status(&self) -> &'static str {
        match self.kind.as_str() {
            "hearing" => "Hearing",
            "trial" => "Trial",
            _ => "Appearance",
        }
    }
}

/// A hearing or trial row that has no `scheduled_on`. Existing databases
/// can hold these from before the field existed; they are reported, never
/// backfilled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UndatedAppearance {
    pub id: Uuid,
    pub case_id: Uuid,
    pub kind: String,
    pub title: String,
}

/// One served set of written discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveryRequest {
    pub id: Uuid,
    pub case_id: Uuid,
    /// Optional, because a device is keyed to the case and only optionally
    /// to the entry recording its service.
    pub docket_entry_id: Option<Uuid>,
    /// `interrogatories`, `requests_for_production`, or
    /// `requests_for_admission`.
    pub device: String,
    /// Which set — "Interrogatories, Set Two".
    pub set_number: i32,
    pub propounding_party: String,
    pub responding_party: String,
    pub served_on: Option<String>,
    pub responses_due_on: Option<String>,
    /// `served`, `responded`, or `closed`.
    pub status: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One numbered item within a served set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveryItem {
    pub id: Uuid,
    pub discovery_request_id: Uuid,
    /// The item's number within its set.
    pub item_number: i32,
    /// The propounded request, verbatim.
    pub request_text: String,
    /// The response, once drafted. `None` until answered.
    pub response_text: Option<String>,
    /// Objections asserted to this item.
    pub objections: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// The litigation rows as PostgreSQL's `to_jsonb` serves them, under the
// `postgres::cases` authority. Record links are plain UUID foreign keys, so
// each type adds nothing to decode beyond the column list.

#[derive(serde::Deserialize)]
struct PgCaseRow {
    id: Uuid,
    project_id: Uuid,
    caption: String,
    forum: Option<String>,
    jurisdiction: Option<String>,
    docket_number: Option<String>,
    judge: Option<String>,
    posture: String,
    status: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgCaseRow> for Case {
    fn from(row: PgCaseRow) -> Self {
        Case {
            id: row.id,
            project_id: row.project_id,
            caption: row.caption,
            forum: row.forum,
            jurisdiction: row.jurisdiction,
            docket_number: row.docket_number,
            judge: row.judge,
            posture: row.posture,
            status: row.status,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(serde::Deserialize)]
struct PgDocketEntryRow {
    id: Uuid,
    case_id: Uuid,
    entry_number: String,
    kind: String,
    title: String,
    party: Option<String>,
    filed_or_served_on: Option<String>,
    scheduled_on: Option<DateTime<Utc>>,
    supersedes: Option<Uuid>,
    document_asset_id: Option<Uuid>,
    notation_id: Option<Uuid>,
    stamp_case_number: Option<String>,
    stamp_entry_number: Option<String>,
    stamp_filed_on: Option<String>,
    integrity_checked_at: Option<DateTime<Utc>>,
    integrity_source_sha256_hex: Option<String>,
    integrity_matched: Option<bool>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgDocketEntryRow> for DocketEntry {
    fn from(row: PgDocketEntryRow) -> Self {
        DocketEntry {
            id: row.id,
            case_id: row.case_id,
            entry_number: row.entry_number,
            kind: row.kind,
            title: row.title,
            party: row.party,
            filed_or_served_on: row.filed_or_served_on,
            scheduled_on: row.scheduled_on,
            supersedes: row.supersedes,
            document_asset_id: row.document_asset_id,
            notation_id: row.notation_id,
            stamp_case_number: row.stamp_case_number,
            stamp_entry_number: row.stamp_entry_number,
            stamp_filed_on: row.stamp_filed_on,
            integrity_checked_at: row.integrity_checked_at,
            integrity_source_sha256_hex: row.integrity_source_sha256_hex,
            integrity_matched: row.integrity_matched,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(serde::Deserialize)]
struct PgDiscoveryRequestRow {
    id: Uuid,
    case_id: Uuid,
    docket_entry_id: Option<Uuid>,
    device: String,
    set_number: i32,
    propounding_party: String,
    responding_party: String,
    served_on: Option<String>,
    responses_due_on: Option<String>,
    status: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgDiscoveryRequestRow> for DiscoveryRequest {
    fn from(row: PgDiscoveryRequestRow) -> Self {
        DiscoveryRequest {
            id: row.id,
            case_id: row.case_id,
            docket_entry_id: row.docket_entry_id,
            device: row.device,
            set_number: row.set_number,
            propounding_party: row.propounding_party,
            responding_party: row.responding_party,
            served_on: row.served_on,
            responses_due_on: row.responses_due_on,
            status: row.status,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(serde::Deserialize)]
struct PgDiscoveryItemRow {
    id: Uuid,
    discovery_request_id: Uuid,
    item_number: i32,
    request_text: String,
    response_text: Option<String>,
    objections: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgDiscoveryItemRow> for DiscoveryItem {
    fn from(row: PgDiscoveryItemRow) -> Self {
        DiscoveryItem {
            id: row.id,
            discovery_request_id: row.discovery_request_id,
            item_number: row.item_number,
            request_text: row.request_text,
            response_text: row.response_text,
            objections: row.objections,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// What a new [`Case`] needs.
#[derive(Debug, Clone)]
pub struct NewCase<'a> {
    pub project_id: Uuid,
    pub caption: &'a str,
    pub forum: Option<&'a str>,
    pub jurisdiction: Option<&'a str>,
    pub docket_number: Option<&'a str>,
    /// The presiding judge — part of the case masthead.
    pub judge: Option<&'a str>,
    pub posture: &'a str,
}

/// Open a case on a matter. Many cases per matter is the point, so this
/// never checks for an existing one.
///
/// # Errors
/// Propagates any database error.
pub async fn open_case(_db: &ApplicationStore, new: &NewCase<'_>) -> Result<Case, CaseError> {
    let pg = authority::authority().await?;
    pg_open_case(&pg, new).await
}

/// Resolve or open a numbered case without duplicating overlapping intake.
/// The owning Project row serializes lookup/create in the database; every
/// optimistic conflict reruns the lookup under the shared write policy.
///
/// # Errors
/// Propagates database failures or a write that returns no usable case.
pub async fn ensure_case(_db: &ApplicationStore, new: &NewCase<'_>) -> Result<Case, CaseError> {
    let pg = authority::authority().await?;
    pg_ensure_case(&pg, new).await
}

/// Every case on `project_id`, oldest first.
///
/// # Errors
/// Propagates any database error.
pub async fn for_project(_db: &ApplicationStore, project_id: Uuid) -> Result<Vec<Case>, CaseError> {
    let pg = authority::authority().await?;
    pg_cases_for_project(&pg, project_id).await
}

/// The case on `project_id` with the same docket number, ignoring case
/// and trailing judge initials on either number.
///
/// # Errors
/// Propagates any database error.
pub async fn find_case_by_docket_number(
    db: &ApplicationStore,
    project_id: Uuid,
    docket_number: &str,
) -> Result<Option<Case>, CaseError> {
    let wanted = canonical_docket_number(docket_number);
    Ok(canonical_match(for_project(db, project_id).await?, &wanted))
}

/// A docket's identity excludes trailing judge initials. Single-letter
/// case-type codes (such as Nevada's `-C`) remain part of the number.
fn canonical_docket_number(number: &str) -> String {
    let mut number = number.trim().to_ascii_lowercase();
    while let Some((base, suffix)) = number.rsplit_once('-') {
        let has_serial = base
            .split('-')
            .any(|part| part.len() >= 4 && part.bytes().all(|byte| byte.is_ascii_digit()));
        if !has_serial || suffix.len() < 2 || !suffix.bytes().all(|byte| byte.is_ascii_alphabetic())
        {
            break;
        }
        number.truncate(base.len());
    }
    number
}

/// Every docket entry on every case of `project_id`, case by case in the
/// order the cases were opened and, within a case, in insertion order (see
/// [`docket`]).
///
/// # Errors
/// Propagates any database error.
pub async fn docket_for_project(
    db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<DocketEntry>, CaseError> {
    let mut entries = Vec::new();
    for case in for_project(db, project_id).await? {
        entries.extend(docket(db, case.id).await?);
    }
    Ok(entries)
}

/// One docket entry by id, or `None`.
///
/// # Errors
/// Propagates any database error.
pub async fn find_entry(
    _db: &ApplicationStore,
    entry_id: Uuid,
) -> Result<Option<DocketEntry>, CaseError> {
    let pg = authority::authority().await?;
    pg_find_entry(&pg, entry_id).await
}

/// Link the filed document to an entry, ending its *source pending* state.
/// A later link replaces the earlier one: the entry names the document that
/// is on file for it, and a corrected filing is a new revision of that
/// document rather than a second link.
///
/// # Errors
/// [`CaseError::EntryNotFound`] when no entry carries `entry_id`, or any
/// database error.
pub async fn link_document(
    _db: &ApplicationStore,
    entry_id: Uuid,
    document_asset_id: Uuid,
) -> Result<DocketEntry, CaseError> {
    let pg = authority::authority().await?;
    pg_link_document(&pg, entry_id, document_asset_id).await
}

/// What a continuance needs: the new entry number the court gave it and
/// the date the appearance moved to. The kind, the case, and the title are
/// the prior entry's unless a new title is given.
#[derive(Debug, Clone)]
pub struct Continuance<'a> {
    pub entry_number: &'a str,
    pub scheduled_on: DateTime<Utc>,
    pub title: Option<&'a str>,
    pub party: Option<&'a str>,
    pub filed_or_served_on: Option<&'a str>,
    pub document_asset_id: Option<Uuid>,
}

/// Record a continuance: a new entry of the prior entry's kind carrying the
/// new date and a `supersedes` link back, so the earlier date stays on the
/// record and the calendar follows the chain.
///
/// # Errors
/// [`CaseError::EntryNotFound`] when no entry carries `prior_id`,
/// [`CaseError::NotAnAppearance`] when it is not a hearing or trial, or any
/// database error.
pub async fn supersede(
    db: &ApplicationStore,
    prior_id: Uuid,
    continuance: &Continuance<'_>,
) -> Result<DocketEntry, CaseError> {
    let prior = find_entry(db, prior_id)
        .await?
        .ok_or(CaseError::EntryNotFound(prior_id))?;
    let kind = EntryKind::parse(&prior.kind)
        .ok_or_else(|| CaseError::UnknownEntryKind(prior.kind.clone()))?;
    if !kind.is_appearance() {
        return Err(CaseError::NotAnAppearance(prior.kind));
    }
    record_entry(
        db,
        &NewDocketEntry {
            case_id: prior.case_id,
            entry_number: continuance.entry_number,
            kind,
            title: continuance.title.unwrap_or(&prior.title),
            party: continuance.party,
            filed_or_served_on: continuance.filed_or_served_on,
            scheduled_on: Some(continuance.scheduled_on),
            supersedes: Some(prior.id),
            document_asset_id: continuance.document_asset_id,
            notation_id: None,
        },
    )
    .await
}

/// Remove the litigation rows that cascade from a matter.
///
/// SurrealDB has no cascade, so the chain is walked explicitly
/// in dependency order: discovery items, requests, docket entries, and
/// finally the cases themselves.
///
/// # Errors
/// Propagates any database error.
pub async fn delete_for_project(_db: &ApplicationStore, project_id: Uuid) -> Result<(), CaseError> {
    let pg = authority::authority().await?;
    pg_delete_for_project(&pg, project_id).await
}

/// What a new docket entry needs.
#[derive(Debug, Clone)]
pub struct NewDocketEntry<'a> {
    pub case_id: Uuid,
    /// Text, so a court's attachment sub-number (`29-1`) round-trips.
    pub entry_number: &'a str,
    pub kind: EntryKind,
    pub title: &'a str,
    pub party: Option<&'a str>,
    pub filed_or_served_on: Option<&'a str>,
    /// Required when `kind` is [`EntryKind::Hearing`] or [`EntryKind::Trial`];
    /// must be `None` otherwise. The schema ASSERT is the write-time gate.
    pub scheduled_on: Option<DateTime<Utc>>,
    /// The prior appearance a continuance replaces.
    pub supersedes: Option<Uuid>,
    /// `None` is a meaningful state — the entry renders *source pending*.
    pub document_asset_id: Option<Uuid>,
    pub notation_id: Option<Uuid>,
}

/// Record an entry on a case's docket.
///
/// # Errors
/// Propagates any database error.
pub async fn record_entry(
    db: &ApplicationStore,
    new: &NewDocketEntry<'_>,
) -> Result<DocketEntry, CaseError> {
    persist_entry(db, new, false).await
}

/// Reuse a numbered entry or create it atomically across concurrent callers.
/// The Case row serializes the decision, including callers on other servers.
///
/// # Errors
/// Propagates database failures or a write that returns no usable entry.
pub async fn ensure_entry(
    db: &ApplicationStore,
    new: &NewDocketEntry<'_>,
) -> Result<DocketEntry, CaseError> {
    persist_entry(db, new, true).await
}

async fn persist_entry(
    _db: &ApplicationStore,
    new: &NewDocketEntry<'_>,
    reuse: bool,
) -> Result<DocketEntry, CaseError> {
    let pg = authority::authority().await?;
    pg_persist_entry(&pg, new, reuse).await
}

/// Record the CM/ECF header stamp [`crate::cm_ecf_stamp::extract`] found in
/// the filed document, as **structured provenance separate from** the
/// hand-typed `entry_number` and `filed_or_served_on` (LAW-24, option 2).
/// Neither existing field is overwritten — a reviewer can see what was
/// typed and what the document itself says, and notice when they disagree.
///
/// # Errors
/// Propagates any database error, and [`CaseError::WriteReturnedNothing`]
/// if `entry_id` names no docket entry.
pub async fn record_stamp(
    _db: &ApplicationStore,
    entry_id: Uuid,
    stamp: &crate::cm_ecf_stamp::CmEcfStamp,
) -> Result<DocketEntry, CaseError> {
    let pg = authority::authority().await?;
    pg_record_stamp(&pg, entry_id, stamp).await
}

/// Record the result of hashing an independently fetched copy of this
/// entry's filed document against the one Navigator stored — LAW-24's
/// docket-record attestation, the real authenticity check a "signature
/// verified" badge cannot be, because these PDFs carry no signature to
/// verify. `matched` is the caller's own comparison (typically
/// [`crate::documents::verify_independent_copy`]); this function only
/// records the result, so the crypto-adjacent comparison stays in one place
/// and this stays a plain write.
///
/// # Errors
/// Propagates any database error, and [`CaseError::WriteReturnedNothing`]
/// if `entry_id` names no docket entry.
pub async fn record_integrity_check(
    _db: &ApplicationStore,
    entry_id: Uuid,
    source_sha256_hex: &str,
    matched: bool,
) -> Result<DocketEntry, CaseError> {
    let pg = authority::authority().await?;
    pg_record_integrity_check(&pg, entry_id, source_sha256_hex, matched).await
}

/// The case's docket, in insertion order.
///
/// Not ordered by `entry_number`: it is text carrying sub-numbers like
/// `29-1`, so sorting it as a string would put `10` before `9`. Insertion
/// order is the order the firm recorded the docket in, which is the
/// order a court's own docket is read in.
///
/// # Errors
/// Propagates any database error.
pub async fn docket(_db: &ApplicationStore, case_id: Uuid) -> Result<Vec<DocketEntry>, CaseError> {
    let pg = authority::authority().await?;
    pg_docket(&pg, case_id).await
}

/// True when the entry has no document filed against it yet — the
/// *source pending* state.
///
/// A meaningful state, not an error. Derived from the link rather than
/// from a hand-maintained flag, so it cannot disagree with reality.
#[must_use]
pub fn is_source_pending(entry: &DocketEntry) -> bool {
    entry.document_asset_id.is_none()
}

fn is_appearance_kind(kind: &str) -> bool {
    matches!(kind, "hearing" | "trial")
}

impl DocketEntry {
    /// Whether this entry records a hearing or trial.
    #[must_use]
    pub fn is_appearance(&self) -> bool {
        is_appearance_kind(&self.kind)
    }
}

/// Docket entries for a calendar view, keeping only the latest entry on
/// each continuance chain. Pass the complete docket so each replacement is visible.
pub fn calendar_docket_entries(entries: &[DocketEntry]) -> impl Iterator<Item = &DocketEntry> {
    let superseded: HashSet<Uuid> = entries
        .iter()
        .filter_map(|entry| entry.supersedes)
        .collect();
    entries
        .iter()
        .filter(move |entry| !entry.is_appearance() || !superseded.contains(&entry.id))
}

fn appearance_from_entry(entry: &DocketEntry, project_id: Uuid) -> Option<Appearance> {
    Some(Appearance {
        id: entry.id,
        case_id: entry.case_id,
        project_id,
        kind: entry.kind.clone(),
        title: entry.title.clone(),
        scheduled_on: entry.scheduled_on?,
    })
}

/// Current upcoming appearances on `case_id`: hearing and trial entries
/// that carry a date, that no later entry supersedes, and whose date is
/// still in the future.
///
/// # Errors
/// Propagates any database error.
pub async fn current_appearances_for_case(
    db: &ApplicationStore,
    case_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<Appearance>, CaseError> {
    let entries = docket(db, case_id).await?;
    let now = Utc::now();
    let mut appearances: Vec<Appearance> = calendar_docket_entries(&entries)
        .filter(|entry| entry.is_appearance())
        .filter(|entry| entry.scheduled_on.is_some_and(|when| when >= now))
        .filter_map(|entry| appearance_from_entry(entry, project_id))
        .collect();
    appearances.sort_by_key(|appearance| appearance.scheduled_on);
    Ok(appearances)
}

/// Current upcoming appearances on every case belonging to `project_id`.
///
/// # Errors
/// Propagates any database error.
pub async fn current_appearances_for_project(
    db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<Appearance>, CaseError> {
    let mut appearances = Vec::new();
    for case in for_project(db, project_id).await? {
        appearances.extend(current_appearances_for_case(db, case.id, case.project_id).await?);
    }
    appearances.sort_by_key(|appearance| appearance.scheduled_on);
    Ok(appearances)
}

/// Current upcoming appearances across the given matters.
///
/// # Errors
/// Propagates any database error.
pub async fn current_appearances_for_projects(
    db: &ApplicationStore,
    project_ids: &[Uuid],
) -> Result<Vec<Appearance>, CaseError> {
    let mut appearances = Vec::new();
    for project_id in project_ids {
        appearances.extend(current_appearances_for_project(db, *project_id).await?);
    }
    appearances.sort_by_key(|appearance| appearance.scheduled_on);
    Ok(appearances)
}

/// Hearing and trial rows that have no `scheduled_on`. The schema refuses
/// new writes in that state; this read is how an operator finds rows that
/// predate the field. It never writes.
///
/// # Errors
/// Propagates any database error.
pub async fn undated_hearings_and_trials(
    _db: &ApplicationStore,
) -> Result<Vec<UndatedAppearance>, CaseError> {
    let pg = authority::authority().await?;
    pg_undated_hearings_and_trials(&pg).await
}

/// What a new served discovery set needs.
#[derive(Debug, Clone)]
pub struct NewDiscoveryRequest<'a> {
    pub case_id: Uuid,
    pub docket_entry_id: Option<Uuid>,
    pub device: Device,
    pub set_number: i32,
    pub propounding_party: &'a str,
    pub responding_party: &'a str,
    pub served_on: Option<&'a str>,
    /// What the served papers say. The Deadlines module answers what is
    /// actually due.
    pub responses_due_on: Option<&'a str>,
}

/// Record a served set of written discovery.
///
/// # Errors
/// Propagates any database error.
pub async fn serve_discovery(
    _db: &ApplicationStore,
    new: &NewDiscoveryRequest<'_>,
) -> Result<DiscoveryRequest, CaseError> {
    let pg = authority::authority().await?;
    pg_serve_discovery(&pg, new).await
}

/// Every served set on a case, oldest first.
///
/// # Errors
/// Propagates any database error.
pub async fn discovery_requests(
    _db: &ApplicationStore,
    case_id: Uuid,
) -> Result<Vec<DiscoveryRequest>, CaseError> {
    let pg = authority::authority().await?;
    pg_discovery_requests(&pg, case_id).await
}

/// Add a numbered item to a served set.
///
/// # Errors
/// Propagates any database error.
pub async fn add_item(
    _db: &ApplicationStore,
    discovery_request_id: Uuid,
    item_number: i32,
    request_text: &str,
) -> Result<DiscoveryItem, CaseError> {
    let pg = authority::authority().await?;
    pg_add_item(&pg, discovery_request_id, item_number, request_text).await
}

/// Answer one item, optionally asserting objections.
///
/// # Errors
/// [`CaseError::ItemNotFound`] when no item carries `item_id`, or any
/// database error.
pub async fn answer_item(
    _db: &ApplicationStore,
    item_id: Uuid,
    response_text: &str,
    objections: Option<&str>,
) -> Result<DiscoveryItem, CaseError> {
    let pg = authority::authority().await?;
    pg_answer_item(&pg, item_id, response_text, objections).await
}

/// A served set's items, in item order.
///
/// # Errors
/// Propagates any database error.
pub async fn items(
    _db: &ApplicationStore,
    discovery_request_id: Uuid,
) -> Result<Vec<DiscoveryItem>, CaseError> {
    let pg = authority::authority().await?;
    pg_items(&pg, discovery_request_id).await
}

// The PostgreSQL path, under the `postgres::cases` authority. Each helper
// below answers exactly one command, in the shapes the SurrealQL statements
// above them produce; the public functions dispatch before touching
// SurrealDB.

async fn pg_cases_for_project(
    pg: &crate::postgres::PostgresDb,
    project_id: Uuid,
) -> Result<Vec<Case>, CaseError> {
    let query = sqlx::query(
        "SELECT to_jsonb(\"case\") AS row FROM \"case\" \
         WHERE project_id = $1 ORDER BY id ASC",
    )
    .bind(project_id);
    Ok(authority::rows::<PgCaseRow>(pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// The case whose stored docket number canonicalizes to `wanted`, if any.
fn canonical_match(cases: Vec<Case>, wanted: &str) -> Option<Case> {
    cases.into_iter().find(|case: &Case| {
        case.docket_number
            .as_deref()
            .is_some_and(|stored| canonical_docket_number(stored) == wanted)
    })
}

async fn pg_open_case(
    pg: &crate::postgres::PostgresDb,
    new: &NewCase<'_>,
) -> Result<Case, CaseError> {
    let row = serde_json::json!({
        "id": Uuid::now_v7(), "project_id": new.project_id, "caption": new.caption,
        "forum": new.forum, "jurisdiction": new.jurisdiction,
        "docket_number": new.docket_number, "judge": new.judge,
        "posture": new.posture, "status": "open",
    });
    let query = sqlx::query(
        "INSERT INTO \"case\" SELECT * FROM \
         jsonb_populate_record(NULL::\"case\", \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         RETURNING to_jsonb(\"case\") AS row",
    )
    .bind(row);
    authority::maybe::<PgCaseRow>(pg, query)
        .await?
        .map(Into::into)
        .ok_or(CaseError::WriteReturnedNothing("case"))
}

async fn pg_ensure_case(
    pg: &crate::postgres::PostgresDb,
    new: &NewCase<'_>,
) -> Result<Case, CaseError> {
    let Some(number) = new.docket_number else {
        return pg_open_case(pg, new).await;
    };
    if let Some(existing) = canonical_match(
        pg_cases_for_project(pg, new.project_id).await?,
        &canonical_docket_number(number),
    ) {
        return Ok(existing);
    }
    let number = canonical_docket_number(number);
    let mut tx = pg.pool().begin().await?;
    // The SurrealDB path serializes the lookup/create on the owning Project
    // row; here an advisory lock over the (project, canonical number) pair
    // does, so concurrent intake cannot open a case twice.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("case:{}:{}", new.project_id, number))
        .execute(&mut *tx)
        .await?;
    let cases: Vec<PgCaseRow> = sqlx::query(
        "SELECT to_jsonb(r) AS row FROM \"case\" AS r \
         WHERE r.project_id = $1 ORDER BY r.id ASC",
    )
    .bind(new.project_id)
    .fetch_all(&mut *tx)
    .await?
    .iter()
    .map(authority::decode::<PgCaseRow>)
    .collect::<Result<_, _>>()?;
    if let Some(existing) = canonical_match(cases.into_iter().map(Into::into).collect(), &number) {
        return Ok(existing);
    }
    let row = serde_json::json!({
        "id": Uuid::now_v7(), "project_id": new.project_id, "caption": new.caption,
        "forum": new.forum, "jurisdiction": new.jurisdiction,
        "docket_number": number.clone(), "judge": new.judge,
        "posture": new.posture, "status": "open",
    });
    let written = sqlx::query(
        "INSERT INTO \"case\" SELECT * FROM \
         jsonb_populate_record(NULL::\"case\", \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         RETURNING to_jsonb(\"case\") AS row",
    )
    .bind(row)
    .fetch_one(&mut *tx)
    .await?;
    let case: Case = authority::decode::<PgCaseRow>(&written)?.into();
    tx.commit().await?;
    Ok(case)
}

async fn pg_find_entry(
    pg: &crate::postgres::PostgresDb,
    entry_id: Uuid,
) -> Result<Option<DocketEntry>, CaseError> {
    let query = sqlx::query(
        "SELECT to_jsonb(r) AS row FROM case_docket_entry AS r WHERE r.id = $1 LIMIT 1",
    )
    .bind(entry_id);
    Ok(authority::maybe::<PgDocketEntryRow>(pg, query)
        .await?
        .map(Into::into))
}

async fn pg_link_document(
    pg: &crate::postgres::PostgresDb,
    entry_id: Uuid,
    document_asset_id: Uuid,
) -> Result<DocketEntry, CaseError> {
    let query = sqlx::query(
        "UPDATE case_docket_entry SET document_asset_id = $2, updated_at = now() \
         WHERE id = $1 RETURNING to_jsonb(case_docket_entry) AS row",
    )
    .bind(entry_id)
    .bind(document_asset_id);
    authority::maybe::<PgDocketEntryRow>(pg, query)
        .await?
        .map(Into::into)
        .ok_or(CaseError::EntryNotFound(entry_id))
}

async fn pg_delete_for_project(
    pg: &crate::postgres::PostgresDb,
    project_id: Uuid,
) -> Result<(), CaseError> {
    // Children before parents, in one explicit transaction.
    let mut tx = pg.pool().begin().await?;
    for statement in [
        "DELETE FROM discovery_item WHERE discovery_request_id IN \
         (SELECT id FROM discovery_request WHERE case_id IN \
         (SELECT id FROM \"case\" WHERE project_id = $1))",
        "DELETE FROM discovery_request WHERE case_id IN \
         (SELECT id FROM \"case\" WHERE project_id = $1)",
        "DELETE FROM case_docket_entry WHERE case_id IN \
         (SELECT id FROM \"case\" WHERE project_id = $1)",
        "DELETE FROM \"case\" WHERE project_id = $1",
    ] {
        sqlx::query(statement)
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn pg_persist_entry(
    pg: &crate::postgres::PostgresDb,
    new: &NewDocketEntry<'_>,
    reuse: bool,
) -> Result<DocketEntry, CaseError> {
    // The schema ASSERT is the write-time gate on the SurrealDB path; the
    // PostgreSQL path refuses the same kind/date shapes before it writes.
    if new.kind.is_appearance() != new.scheduled_on.is_some() {
        return Err(CaseError::ScheduledOnRule);
    }
    let row = serde_json::json!({
        "id": Uuid::now_v7(), "case_id": new.case_id,
        "entry_number": new.entry_number, "kind": new.kind.as_str(),
        "title": new.title, "party": new.party,
        "filed_or_served_on": new.filed_or_served_on,
        "scheduled_on": new.scheduled_on, "supersedes": new.supersedes,
        "document_asset_id": new.document_asset_id, "notation_id": new.notation_id,
    });
    if !reuse {
        let query = sqlx::query(
            "INSERT INTO case_docket_entry SELECT * FROM \
             jsonb_populate_record(NULL::case_docket_entry, \
             $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
             RETURNING to_jsonb(case_docket_entry) AS row",
        )
        .bind(row);
        return authority::maybe::<PgDocketEntryRow>(pg, query)
            .await?
            .map(Into::into)
            .ok_or(CaseError::WriteReturnedNothing("docket entry"));
    }
    // Reuse: the Case row serializes the decision on the SurrealDB path;
    // the unique (case, entry number) index is the final word here. An
    // existing entry's document link fills only when it was missing — a
    // later call never replaces an earlier one.
    let mut tx = pg.pool().begin().await?;
    sqlx::query("UPDATE \"case\" SET updated_at = now() WHERE id = $1")
        .bind(new.case_id)
        .execute(&mut *tx)
        .await?;
    let written = sqlx::query(
        "INSERT INTO case_docket_entry AS d SELECT * FROM \
         jsonb_populate_record(NULL::case_docket_entry, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         ON CONFLICT (case_id, entry_number) DO UPDATE SET \
         document_asset_id = COALESCE(d.document_asset_id, EXCLUDED.document_asset_id), \
         updated_at = now() \
         RETURNING to_jsonb(d) AS row",
    )
    .bind(row)
    .fetch_one(&mut *tx)
    .await?;
    let entry: DocketEntry = authority::decode::<PgDocketEntryRow>(&written)?.into();
    tx.commit().await?;
    Ok(entry)
}

async fn pg_record_stamp(
    pg: &crate::postgres::PostgresDb,
    entry_id: Uuid,
    stamp: &crate::cm_ecf_stamp::CmEcfStamp,
) -> Result<DocketEntry, CaseError> {
    let query = sqlx::query(
        "UPDATE case_docket_entry SET \
         stamp_case_number = $2, stamp_entry_number = $3, stamp_filed_on = $4, \
         updated_at = now() \
         WHERE id = $1 RETURNING to_jsonb(case_docket_entry) AS row",
    )
    .bind(entry_id)
    .bind(stamp.case_number.clone())
    .bind(stamp.entry_number.clone())
    .bind(stamp.filed_on.clone());
    authority::maybe::<PgDocketEntryRow>(pg, query)
        .await?
        .map(Into::into)
        .ok_or(CaseError::WriteReturnedNothing("docket entry"))
}

async fn pg_record_integrity_check(
    pg: &crate::postgres::PostgresDb,
    entry_id: Uuid,
    source_sha256_hex: &str,
    matched: bool,
) -> Result<DocketEntry, CaseError> {
    let query = sqlx::query(
        "UPDATE case_docket_entry SET \
         integrity_checked_at = now(), integrity_source_sha256_hex = $2, \
         integrity_matched = $3, updated_at = now() \
         WHERE id = $1 RETURNING to_jsonb(case_docket_entry) AS row",
    )
    .bind(entry_id)
    .bind(source_sha256_hex.to_string())
    .bind(matched);
    authority::maybe::<PgDocketEntryRow>(pg, query)
        .await?
        .map(Into::into)
        .ok_or(CaseError::WriteReturnedNothing("docket entry"))
}

async fn pg_docket(
    pg: &crate::postgres::PostgresDb,
    case_id: Uuid,
) -> Result<Vec<DocketEntry>, CaseError> {
    let query = sqlx::query(
        "SELECT to_jsonb(r) AS row FROM case_docket_entry AS r \
         WHERE r.case_id = $1 ORDER BY r.id ASC",
    )
    .bind(case_id);
    Ok(authority::rows::<PgDocketEntryRow>(pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

async fn pg_undated_hearings_and_trials(
    pg: &crate::postgres::PostgresDb,
) -> Result<Vec<UndatedAppearance>, CaseError> {
    let query = sqlx::query(
        "SELECT to_jsonb(r) AS row FROM case_docket_entry AS r \
         WHERE r.kind IN ('hearing', 'trial') AND r.scheduled_on IS NULL \
         ORDER BY r.id ASC",
    );
    Ok(authority::rows::<PgDocketEntryRow>(pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .map(|entry: DocketEntry| UndatedAppearance {
            id: entry.id,
            case_id: entry.case_id,
            kind: entry.kind,
            title: entry.title,
        })
        .collect())
}

async fn pg_serve_discovery(
    pg: &crate::postgres::PostgresDb,
    new: &NewDiscoveryRequest<'_>,
) -> Result<DiscoveryRequest, CaseError> {
    let row = serde_json::json!({
        "id": Uuid::now_v7(), "case_id": new.case_id,
        "docket_entry_id": new.docket_entry_id, "device": new.device.as_str(),
        "set_number": new.set_number,
        "propounding_party": new.propounding_party,
        "responding_party": new.responding_party,
        "served_on": new.served_on, "responses_due_on": new.responses_due_on,
        "status": "served",
    });
    let query = sqlx::query(
        "INSERT INTO discovery_request SELECT * FROM \
         jsonb_populate_record(NULL::discovery_request, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         RETURNING to_jsonb(discovery_request) AS row",
    )
    .bind(row);
    authority::maybe::<PgDiscoveryRequestRow>(pg, query)
        .await?
        .map(Into::into)
        .ok_or(CaseError::WriteReturnedNothing("discovery request"))
}

async fn pg_discovery_requests(
    pg: &crate::postgres::PostgresDb,
    case_id: Uuid,
) -> Result<Vec<DiscoveryRequest>, CaseError> {
    let query = sqlx::query(
        "SELECT to_jsonb(r) AS row FROM discovery_request AS r \
         WHERE r.case_id = $1 ORDER BY r.id ASC",
    )
    .bind(case_id);
    Ok(authority::rows::<PgDiscoveryRequestRow>(pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

async fn pg_add_item(
    pg: &crate::postgres::PostgresDb,
    discovery_request_id: Uuid,
    item_number: i32,
    request_text: &str,
) -> Result<DiscoveryItem, CaseError> {
    let row = serde_json::json!({
        "id": Uuid::now_v7(), "discovery_request_id": discovery_request_id,
        "item_number": item_number, "request_text": request_text,
        "response_text": null, "objections": null,
    });
    let query = sqlx::query(
        "INSERT INTO discovery_item SELECT * FROM \
         jsonb_populate_record(NULL::discovery_item, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         RETURNING to_jsonb(discovery_item) AS row",
    )
    .bind(row);
    authority::maybe::<PgDiscoveryItemRow>(pg, query)
        .await?
        .map(Into::into)
        .ok_or(CaseError::WriteReturnedNothing("discovery item"))
}

async fn pg_answer_item(
    pg: &crate::postgres::PostgresDb,
    item_id: Uuid,
    response_text: &str,
    objections: Option<&str>,
) -> Result<DiscoveryItem, CaseError> {
    let query = sqlx::query(
        "UPDATE discovery_item SET response_text = $2, objections = $3, \
         updated_at = now() \
         WHERE id = $1 RETURNING to_jsonb(discovery_item) AS row",
    )
    .bind(item_id)
    .bind(response_text.to_string())
    .bind(objections.map(str::to_string));
    authority::maybe::<PgDiscoveryItemRow>(pg, query)
        .await?
        .map(Into::into)
        .ok_or(CaseError::ItemNotFound(item_id))
}

async fn pg_items(
    pg: &crate::postgres::PostgresDb,
    discovery_request_id: Uuid,
) -> Result<Vec<DiscoveryItem>, CaseError> {
    let query = sqlx::query(
        "SELECT to_jsonb(r) AS row FROM discovery_item AS r \
         WHERE r.discovery_request_id = $1 ORDER BY r.item_number ASC",
    )
    .bind(discovery_request_id);
    Ok(authority::rows::<PgDiscoveryItemRow>(pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{Device, EntryKind};

    #[test]
    fn every_entry_kind_round_trips() {
        for kind in EntryKind::ALL {
            assert_eq!(EntryKind::parse(kind.as_str()), Some(*kind));
        }
        assert_eq!(EntryKind::parse("nonsense"), None);
    }

    #[test]
    fn the_spine_spans_the_stages_of_litigation() {
        // Not just pleadings: the spine has to carry a case from the
        // complaint through appeal without a migration per stage.
        for kind in [
            EntryKind::Pleading,
            EntryKind::Motion,
            EntryKind::Subpoena,
            EntryKind::ExpertDisclosure,
            EntryKind::Hearing,
            EntryKind::Trial,
            EntryKind::Appeal,
            EntryKind::Settlement,
        ] {
            assert!(EntryKind::ALL.contains(&kind));
        }
    }

    #[test]
    fn every_device_round_trips() {
        for device in Device::ALL {
            assert_eq!(Device::parse(device.as_str()), Some(*device));
        }
        assert_eq!(Device::ALL.len(), 3, "the three written-discovery devices");
    }
}
