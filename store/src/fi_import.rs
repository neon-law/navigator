//! Explicitly mapped Google Fi metadata imports; no message content or billing.
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::inbox::{self, IngestInbound};

pub const MAX_CSV_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_ROWS: usize = 10_000;
pub const ACCOUNT_PREFIX: &str = "google_fi:";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FiImportError {
    #[error("Fi mapping is invalid: {0}")]
    Mapping(&'static str),
    #[error("Fi CSV exceeds its byte or row limit")]
    Limit,
    #[error("Fi CSV has invalid or duplicated headers")]
    Headers,
    #[error("Fi CSV row {row}: invalid {field}")]
    Row { row: usize, field: &'static str },
    #[error("Fi CSV rows {first} and {second} repeat an identity; distinct identical events need stable source IDs")]
    Duplicate { first: usize, second: usize },
    #[error("Fi source identity already belongs to different metadata")]
    Conflict,
    #[error("another Fi import is committing; retry the unchanged batch")]
    Busy,
    #[error("Fi storage is unavailable")]
    Storage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Call,
    Sms,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => inbox::direction::INBOUND,
            Self::Outbound => inbox::direction::OUTBOUND,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Completed,
    Missed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationUnit {
    Seconds,
    Minutes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Columns {
    pub kind: String,
    pub direction: String,
    pub counterparty: String,
    pub timestamp: Vec<String>,
    pub duration: Option<String>,
    pub identity: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Zone {
    EmbeddedOffset,
    Iana(String),
    FixedOffset(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    pub columns: Columns,
    pub kinds: BTreeMap<String, Kind>,
    pub directions: BTreeMap<String, Direction>,
    #[serde(default)]
    pub statuses: BTreeMap<String, Status>,
    pub timestamp_format: String,
    /// Separator used only when the mapping names two timestamp columns.
    pub timestamp_separator: String,
    pub timezone: Zone,
    pub duration_unit: DurationUnit,
}

/// Canonical persisted facts. Every field is metadata; source content is absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub source: String,
    pub kind: Kind,
    pub direction: Direction,
    pub counterparty: String,
    pub occurred_at: String,
    pub duration_seconds: Option<i64>,
    pub status: Option<Status>,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub number: usize,
    pub metadata: Metadata,
    pub provider_message_id: String,
    pub body: String,
}

impl Row {
    #[must_use]
    pub fn ingest<'a>(&'a self, firm: Uuid, brand: &'a str, account: &'a str) -> IngestInbound<'a> {
        IngestInbound {
            project_id: None,
            firm_id: firm,
            brand_key: Some(brand),
            interaction_type: None,
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: Some("private"),
            channel: match self.metadata.kind {
                Kind::Call => inbox::channel::VOICE,
                Kind::Sms => inbox::channel::SMS,
            },
            provider_account: account,
            provider_message_id: &self.provider_message_id,
            provider_thread_ref: None,
            external_address: &self.metadata.counterparty,
            external_name: None,
            subject: Some("Google Fi metadata"),
            body: &self.body,
            source_page: Some(account),
            source_channel: Some("google_fi"),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachments: Vec::new(),
            occurred_at: &self.metadata.occurred_at,
            raw_storage_key: None,
        }
    }
}

#[must_use]
pub fn is_account(account: &str) -> bool {
    account.starts_with(ACCOUNT_PREFIX)
}

pub fn account(number: &str) -> Result<String, FiImportError> {
    let number = number.trim();
    if !inbox::is_e164(number) {
        return Err(FiImportError::Mapping("account must be E.164"));
    }
    Ok(format!("{ACCOUNT_PREFIX}{number}"))
}

fn body(metadata: &Metadata) -> String {
    let kind = match metadata.kind {
        Kind::Call => "Call",
        Kind::Sms => "SMS",
    };
    let direction = match metadata.direction {
        Direction::Inbound => "Inbound",
        Direction::Outbound => "Outbound",
    };
    let duration = metadata
        .duration_seconds
        .map_or_else(|| "Not recorded".into(), |value| value.to_string());
    let status = match metadata.status {
        None => "Not recorded",
        Some(Status::Completed) => "Completed",
        Some(Status::Missed) => "Missed",
        Some(Status::Unknown) => "Unknown",
    };
    format!("Google Fi history\nKind: {kind}\nDirection: {direction}\nCounterparty: {}\nOccurred at: {}\nDuration seconds: {duration}\nStatus: {status}",metadata.counterparty,metadata.occurred_at)
}

/// Decode only this adapter's generated metadata, never arbitrary source text.
#[must_use]
pub fn metadata(body: &str) -> Option<Metadata> {
    let lines = body.lines().collect::<Vec<_>>();
    if lines.len() != 7 || lines[0] != "Google Fi history" {
        return None;
    }
    let field = |at: usize, label: &str| lines[at].strip_prefix(label);
    let kind = match field(1, "Kind: ")? {
        "Call" => Kind::Call,
        "SMS" => Kind::Sms,
        _ => return None,
    };
    let direction = match field(2, "Direction: ")? {
        "Inbound" => Direction::Inbound,
        "Outbound" => Direction::Outbound,
        _ => return None,
    };
    let duration = field(5, "Duration seconds: ")?;
    let duration_seconds = if duration == "Not recorded" {
        None
    } else {
        Some(duration.parse::<i64>().ok()?)
    };
    let status = match field(6, "Status: ")? {
        "Not recorded" => None,
        "Completed" => Some(Status::Completed),
        "Missed" => Some(Status::Missed),
        "Unknown" => Some(Status::Unknown),
        _ => return None,
    };
    Some(Metadata {
        source: "google_fi:v1".into(),
        kind,
        direction,
        counterparty: field(3, "Counterparty: ")?.into(),
        occurred_at: field(4, "Occurred at: ")?.into(),
        duration_seconds,
        status,
    })
}

enum ParsedZone {
    Embedded,
    Iana(chrono_tz::Tz),
    Fixed(FixedOffset),
}

impl ParsedZone {
    fn new(zone: &Zone) -> Result<Self, FiImportError> {
        match zone {
            Zone::EmbeddedOffset => Ok(Self::Embedded),
            Zone::Iana(value) => value
                .parse()
                .map(Self::Iana)
                .map_err(|_| FiImportError::Mapping("unknown IANA timezone")),
            Zone::FixedOffset(value) => value
                .parse()
                .map(Self::Fixed)
                .map_err(|_| FiImportError::Mapping("invalid fixed offset")),
        }
    }
    fn parse(&self, value: &str, format: &str) -> Option<DateTime<Utc>> {
        match self {
            Self::Embedded => DateTime::parse_from_str(value, format)
                .ok()
                .map(|v| v.with_timezone(&Utc)),
            Self::Iana(zone) => zone
                .from_local_datetime(&NaiveDateTime::parse_from_str(value, format).ok()?)
                .single()
                .map(|v| v.with_timezone(&Utc)),
            Self::Fixed(zone) => zone
                .from_local_datetime(&NaiveDateTime::parse_from_str(value, format).ok()?)
                .single()
                .map(|v| v.with_timezone(&Utc)),
        }
    }
}

fn seconds(value: &str, unit: DurationUnit) -> Result<Option<i64>, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    duration_value(value, unit)
        .map(Some)
        .ok_or("invalid duration")
}

fn duration_value(value: &str, unit: DurationUnit) -> Option<i64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let fraction = fraction.trim_end_matches('0');
    if whole.is_empty()
        || whole.len() > 38
        || fraction.len() > 9
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let scale = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    let numerator = whole
        .parse::<u128>()
        .ok()?
        .checked_mul(scale)?
        .checked_add(if fraction.is_empty() {
            0
        } else {
            fraction.parse::<u128>().ok()?
        })?
        .checked_mul(match unit {
            DurationUnit::Seconds => 1,
            DurationUnit::Minutes => 60,
        })?;
    if numerator % scale != 0 {
        return None;
    }
    i64::try_from(numerator / scale).ok()
}

fn counterparty(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')' | '.'))
    {
        return None;
    }
    let normalized = value
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect::<String>();
    let digits = normalized.strip_prefix('+').unwrap_or(&normalized);
    (!digits.is_empty() && digits.len() <= 20 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then_some(normalized)
}

fn digest(parts: &[&str]) -> String {
    let mut encoded = Vec::new();
    for part in parts {
        encoded.extend_from_slice(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        encoded.extend_from_slice(part.as_bytes());
    }
    crate::assets::sha256_hex(&encoded)
}

/// Validate the complete bounded file before admitting any Message. Header names
/// and value maps are explicit operator input, never a claim about Google's format.
#[expect(
    clippy::too_many_lines,
    reason = "one ordered bounded parse validates the entire file and duplicate identities before writes"
)]
pub fn parse(mapping: &Mapping, number: &str, bytes: &[u8]) -> Result<Vec<Row>, FiImportError> {
    let account = account(number)?;
    if bytes.is_empty() || bytes.len() > MAX_CSV_BYTES {
        return Err(FiImportError::Limit);
    }
    if mapping.columns.timestamp.is_empty()
        || mapping.columns.timestamp.len() > 2
        || mapping.timestamp_format.is_empty()
        || mapping.timestamp_format.len() > 128
        || mapping.timestamp_separator.len() > 8
        || mapping.kinds.is_empty()
        || mapping.directions.is_empty()
        || mapping.kinds.len() > 32
        || mapping.directions.len() > 32
        || mapping.statuses.len() > 32
    {
        return Err(FiImportError::Mapping(
            "required columns, value maps and timestamp format are bounded",
        ));
    }
    if serde_json::to_vec(mapping)
        .map_err(|_| FiImportError::Mapping("invalid mapping"))?
        .len()
        > 65_536
        || mapping
            .kinds
            .keys()
            .chain(mapping.directions.keys())
            .chain(mapping.statuses.keys())
            .any(|key| key.is_empty() || key.len() > 128)
    {
        return Err(FiImportError::Mapping("mapping exceeds its bounds"));
    }
    let offset_format = ["%z", "%:z", "%::z", "%:::z", "%#z", "%Z", "%+"]
        .iter()
        .any(|token| mapping.timestamp_format.contains(token));
    if !matches!(&mapping.timezone, Zone::EmbeddedOffset) && offset_format {
        return Err(FiImportError::Mapping(
            "offset-bearing values require embedded_offset mode",
        ));
    }
    let zone = ParsedZone::new(&mapping.timezone)?;
    let text = std::str::from_utf8(bytes)
        .map_err(|_| FiImportError::Headers)?
        .trim_start_matches('\u{feff}');
    let mut csv = csv::ReaderBuilder::new()
        .flexible(false)
        .from_reader(text.as_bytes());
    let headers = csv.headers().map_err(|_| FiImportError::Headers)?.clone();
    let mut unique = BTreeSet::new();
    if headers.is_empty()
        || headers.len() > 64
        || headers.iter().any(|h| {
            h.trim().is_empty() || h.len() > 128 || !unique.insert(h.trim().to_lowercase())
        })
    {
        return Err(FiImportError::Headers);
    }
    let column = |name: &str| {
        headers
            .iter()
            .position(|h| h.trim() == name)
            .ok_or(FiImportError::Mapping("mapped header does not exist"))
    };
    let kind = column(&mapping.columns.kind)?;
    let direction = column(&mapping.columns.direction)?;
    let counterparty_at = column(&mapping.columns.counterparty)?;
    let duration = mapping
        .columns
        .duration
        .as_deref()
        .map(column)
        .transpose()?;
    let timestamps = mapping
        .columns
        .timestamp
        .iter()
        .map(|name| column(name))
        .collect::<Result<Vec<_>, _>>()?;
    let identity = mapping
        .columns
        .identity
        .as_deref()
        .map(column)
        .transpose()?;
    let status = mapping.columns.status.as_deref().map(column).transpose()?;
    if status.is_some() && mapping.statuses.is_empty() {
        return Err(FiImportError::Mapping(
            "status column requires an explicit value map",
        ));
    }
    let mut rows = Vec::new();
    let mut identities = BTreeMap::new();
    for (index, record) in csv.records().enumerate() {
        if index >= MAX_ROWS {
            return Err(FiImportError::Limit);
        }
        let number = index + 2;
        let record = record.map_err(|_| FiImportError::Row {
            row: number,
            field: "CSV structure",
        })?;
        let value = |at: usize| record.get(at).unwrap_or_default().trim();
        let invalid = |field| FiImportError::Row { row: number, field };
        let kind = *mapping
            .kinds
            .get(value(kind))
            .ok_or_else(|| invalid("kind"))?;
        let direction = *mapping
            .directions
            .get(value(direction))
            .ok_or_else(|| invalid("direction"))?;
        let counterparty =
            counterparty(value(counterparty_at)).ok_or_else(|| invalid("counterparty phone"))?;
        let timestamp = timestamps
            .iter()
            .map(|i| value(*i))
            .collect::<Vec<_>>()
            .join(&mapping.timestamp_separator);
        let occurred = zone
            .parse(&timestamp, &mapping.timestamp_format)
            .ok_or_else(|| invalid("timestamp or timezone"))?;
        let nanos = occurred.timestamp_subsec_nanos();
        if nanos >= 1_000_000_000 || nanos % 1_000 != 0 {
            return Err(invalid(
                "timestamp precision (whole microseconds required; no leap seconds)",
            ));
        }
        let duration_seconds = seconds(duration.map_or("", value), mapping.duration_unit)
            .map_err(|_| invalid("duration"))?;
        if kind == Kind::Sms && duration_seconds.is_some() {
            return Err(invalid("SMS duration"));
        }
        let status = if let Some(at) = status {
            if value(at).is_empty() {
                None
            } else {
                Some(
                    *mapping
                        .statuses
                        .get(value(at))
                        .ok_or_else(|| invalid("status"))?,
                )
            }
        } else {
            None
        };
        let metadata = Metadata {
            source: "google_fi:v1".into(),
            kind,
            direction,
            counterparty,
            occurred_at: occurred.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            duration_seconds,
            status,
        };
        let body = body(&metadata);
        let identity = if let Some(at) = identity {
            let native = value(at);
            if native.is_empty() || native.len() > 256 {
                return Err(invalid("identity"));
            }
            format!("google_fi:v1:id:{}", digest(&[&account, native]))
        } else {
            format!("google_fi:v1:row:{}", digest(&[&account, &body]))
        };
        if let Some(first) = identities.insert(identity.clone(), number) {
            return Err(FiImportError::Duplicate {
                first,
                second: number,
            });
        }
        rows.push(Row {
            number,
            metadata,
            provider_message_id: identity,
            body,
        });
    }
    if rows.is_empty() {
        return Err(FiImportError::Limit);
    }
    Ok(rows)
}

/// Scope resolved by the authenticated import door, never by CSV columns.
#[derive(Clone, Debug)]
pub struct Scope {
    pub actor: Uuid,
    pub firm: Uuid,
    pub brand: String,
    pub account: String,
    pub project: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Existing {
    New,
    Identical,
    Conflict,
}

/// The same native authority gate used by #151's import writer.
pub async fn native_pool() -> Result<crate::postgres::PostgresDb, FiImportError> {
    crate::inbox::inbox_authority()
        .await
        .map_err(|_| FiImportError::Storage)?
        .ok_or(FiImportError::Storage)
}

/// Read-only preflight. Apply verifies the persisted winner again because
/// another importer can commit between preview and submission.
pub async fn preview_on(
    pg: &crate::postgres::PostgresDb,
    scope: &Scope,
    row: &Row,
) -> Result<Existing, FiImportError> {
    // A native ID changing kind must not acquire a second channel-scoped ingress.
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT DISTINCT message_id FROM inbox_ingress WHERE firm_id=$1 AND provider_account=$2 AND provider_message_id=$3")
        .bind(scope.firm).bind(&scope.account).bind(&row.provider_message_id)
        .fetch_all(pg.pool()).await.map_err(|_|FiImportError::Storage)?;
    if ids.len() > 1 {
        return Ok(Existing::Conflict);
    }
    let existing = ids.first().copied();
    let Some(id) = existing else {
        return Ok(Existing::New);
    };
    Ok(if matches_existing(pg, scope, row, id).await? {
        Existing::Identical
    } else {
        Existing::Conflict
    })
}

async fn matches_existing(
    pg: &crate::postgres::PostgresDb,
    scope: &Scope,
    row: &Row,
    id: Uuid,
) -> Result<bool, FiImportError> {
    let message = crate::postgres::inbox::message_by_id_on(pg, id)
        .await
        .map_err(|_| FiImportError::Storage)?
        .ok_or(FiImportError::Storage)?;
    let conversation = crate::postgres::inbox::conversation_by_id_on(pg, message.conversation_id)
        .await
        .map_err(|_| FiImportError::Storage)?
        .ok_or(FiImportError::Storage)?;
    matches_facts(scope, row, &message, &conversation)
}

pub(crate) fn matches_facts(
    scope: &Scope,
    row: &Row,
    message: &inbox::Message,
    conversation: &inbox::Conversation,
) -> Result<bool, FiImportError> {
    let expected = row.ingest(scope.firm, &scope.brand, &scope.account);
    let original_time =
        DateTime::parse_from_rfc3339(&message.occurred_at).map_err(|_| FiImportError::Storage)?;
    let requested_time = DateTime::parse_from_rfc3339(&row.metadata.occurred_at)
        .map_err(|_| FiImportError::Storage)?;
    Ok(conversation.firm_id == scope.firm
        && conversation.brand_key.as_deref() == Some(scope.brand.as_str())
        && conversation.provider_account == scope.account
        && conversation.channel == expected.channel
        && scope
            .project
            .is_none_or(|project| conversation.project_id.is_none_or(|id| id == project))
        && message.provider_message_id.as_deref() == Some(row.provider_message_id.as_str())
        && message.body == row.body
        && original_time == requested_time
        && message.direction == row.metadata.direction.as_str()
        && message.author_person_id.is_none()
        && message.delivery_state == inbox::delivery::RECEIVED
        && message.raw_storage_key.is_none()
        && message.attachment_asset_id.is_none())
}

fn canonical(row: &Row) -> bool {
    let value = &row.metadata;
    let Some(identity) = row
        .provider_message_id
        .strip_prefix("google_fi:v1:id:")
        .or_else(|| row.provider_message_id.strip_prefix("google_fi:v1:row:"))
    else {
        return false;
    };
    let Ok(time) = DateTime::parse_from_rfc3339(&value.occurred_at) else {
        return false;
    };
    value.source == "google_fi:v1"
        && counterparty(&value.counterparty).as_deref() == Some(value.counterparty.as_str())
        && time.timestamp_subsec_nanos() < 1_000_000_000
        && time.timestamp_subsec_nanos() % 1_000 == 0
        && time
            .with_timezone(&Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
            == value.occurred_at
        && value.duration_seconds.is_none_or(|seconds| seconds >= 0)
        && (value.kind != Kind::Sms || value.duration_seconds.is_none())
        && identity.len() == 64
        && identity
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && body(value) == row.body
}

/// Fence Fi imports and assignments through destination verification. Try-locking
/// releases competing coordinator connections instead of exhausting the pool.
pub(crate) async fn lock_on(
    pg: &crate::postgres::PostgresDb,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, FiImportError> {
    let mut guard = pg
        .pool()
        .begin()
        .await
        .map_err(|_| FiImportError::Storage)?;
    let acquired: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtextextended('google_fi:import:v1',168))",
    )
    .fetch_one(&mut *guard)
    .await
    .map_err(|_| FiImportError::Storage)?;
    if !acquired {
        return Err(FiImportError::Busy);
    }
    Ok(guard)
}

/// Commit immutable history and its requested initial attribution atomically.
pub async fn ingest_on(
    pg: &crate::postgres::PostgresDb,
    scope: &Scope,
    row: &Row,
) -> Result<inbox::IngestedInbound, FiImportError> {
    let number = scope
        .account
        .strip_prefix(ACCOUNT_PREFIX)
        .ok_or(FiImportError::Mapping("Fi account namespace required"))?;
    if account(number)? != scope.account || !canonical(row) {
        return Err(FiImportError::Mapping("canonical Fi metadata required"));
    }
    let mut guard = lock_on(pg).await?;
    let saved = crate::postgres::inbox::ingest_fi_message_in(&mut guard, scope, row).await?;
    guard.commit().await.map_err(|_| FiImportError::Storage)?;
    Ok(saved)
}

#[cfg(test)]
#[path = "fi_import_tests.rs"]
mod tests;
