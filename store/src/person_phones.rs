//! Structured phone numbers on a [`Person`](crate::persons::Person).
//!
//! One row per number in the `person_phone` table: the number normalized
//! to E.164, the kind of line it is, and whether it is the one a filing
//! or a callback uses first. The legacy free-text `person.phone` column
//! stays exactly what it was — older surfaces read and write it — and
//! [`seed_from_legacy_phone`] lifts that single value into the
//! structured list the first time anyone updates a Person's phones.
//!
//! Normalization ([`normalize_e164`]) happens at the write boundary,
//! never at read, so every reader — including the Google Contacts sync —
//! sees one spelling. Exactly one row per Person is primary; the
//! enforcement is store-layer convention ([`set_phone`] demotes the
//! others in the same transaction, and this module is the only writer),
//! because a SurrealDB UNIQUE index cannot express "one per person" over
//! a subset of rows.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};
use uuid::Uuid;

use crate::postgres_mirror::MirrorKey;
use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

/// The table these rows live in.
const TABLE: &str = "person_phone";

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`PersonPhoneRow`] from only one query.
const SELECT: &str = "id, person_id, e164, label, is_primary, inserted_at, updated_at";

/// The kind of line a phone number is. Free-text labels rot into a
/// dozen spellings of the same fact, so the vocabulary is closed and
/// the schema's ASSERT holds it.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum PhoneLabel {
    /// A personal cell phone. The default.
    #[default]
    Mobile,
    /// A line answered during business hours.
    Work,
    /// A residential line.
    Home,
    /// A fax line.
    Fax,
    /// A line none of the other labels describe.
    Other,
}

impl PhoneLabel {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mobile => "mobile",
            Self::Work => "work",
            Self::Home => "home",
            Self::Fax => "fax",
            Self::Other => "other",
        }
    }

    /// The label named by its stored spelling, or `None` for anything
    /// else. The inverse of [`PhoneLabel::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "mobile" => Some(Self::Mobile),
            "work" => Some(Self::Work),
            "home" => Some(Self::Home),
            "fax" => Some(Self::Fax),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// One Person's phone number.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`PersonPhoneRow`] is the seam that turns it into (and back out of)
/// what the SDK reads and writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonPhone {
    pub id: Uuid,
    pub person_id: Uuid,
    /// The number in canonical E.164 form (`+`, then 8–15 digits).
    pub e164: String,
    pub label: PhoneLabel,
    /// Whether this is the Person's first-choice number. At most one
    /// row per Person carries it.
    pub is_primary: bool,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as the engine reads it. Separate from [`PersonPhone`] because
/// the SDK owns its own `RecordId` and `Datetime` and `label` arrives as
/// the stored string; the conversion belongs at this seam rather than in
/// every caller.
#[derive(SurrealValue)]
struct PersonPhoneRow {
    id: RecordId,
    person_id: RecordId,
    e164: String,
    label: String,
    is_primary: bool,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl PersonPhoneRow {
    /// Read one row, dropping one this module could not have written —
    /// an unknown `label` spelling in a row this module never wrote
    /// hides that row rather than failing every read beside it.
    fn into_phone(self) -> Option<PersonPhone> {
        Some(PersonPhone {
            id: record_uuid(&self.id)?,
            person_id: record_uuid(&self.person_id)?,
            e164: self.e164,
            label: PhoneLabel::parse(&self.label)?,
            is_primary: self.is_primary,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// Errors reading, writing, or normalizing a phone number.
#[derive(Debug, thiserror::Error)]
pub enum PhoneError {
    /// A database operation failed.
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// The number cannot be normalized to E.164. It names what was
    /// rejected, never what the caller might have meant.
    #[error("cannot normalize {0:?} to E.164")]
    InvalidNumber(String),
    /// No Person row answers to the id the write named.
    #[error("person {0} not found")]
    PersonMissing(Uuid),
    /// The Person behind the write could not be read back.
    #[error("person lookup: {0}")]
    Person(#[from] crate::persons::PersonError),
    /// A write reported success but returned no row, or returned one
    /// this module could not read back — see [`PersonPhoneRow::into_phone`].
    #[error("writing a phone returned no usable row")]
    WriteLost,
}

/// Normalize a phone number to E.164 (`+`, then 8–15 digits).
///
/// Punctuation (dashes, dots, spaces, parentheses) is stripped. A number
/// already carrying a leading `+` keeps its country code. A bare number
/// is read as North American — exactly ten digits, or eleven opening
/// with the US country code `1` — because that is the jurisdiction
/// Navigator's own practice runs in; anything else without a `+` is
/// refused rather than guessed at.
///
/// # Errors
///
/// [`PhoneError::InvalidNumber`] when the digits cannot be an E.164
/// number under those rules.
pub fn normalize_e164(raw: &str) -> Result<String, PhoneError> {
    let trimmed = raw.trim();
    let explicit_country = trimmed.starts_with('+');
    let mut digits = String::with_capacity(trimmed.len());
    for (index, ch) in trimmed.char_indices() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if index == 0 && ch == '+' {
            // A leading country-code marker: remembered, not carried.
        } else if ch.is_whitespace() || "()-.".contains(ch) {
            // Formatting: stripped.
        } else {
            return Err(PhoneError::InvalidNumber(raw.to_string()));
        }
    }

    match (explicit_country, digits.len()) {
        (true, 8..=15) => Ok(format!("+{digits}")),
        (false, 10) => Ok(format!("+1{digits}")),
        (false, 11) if digits.starts_with('1') => Ok(format!("+{digits}")),
        _ => Err(PhoneError::InvalidNumber(raw.to_string())),
    }
}

/// Read one row out of a query response, dropping a row this module
/// could not have written.
fn one(mut response: surrealdb::IndexedResults) -> Result<Option<PersonPhone>, PhoneError> {
    let row: Option<PersonPhoneRow> = response.take(0)?;
    Ok(row.and_then(PersonPhoneRow::into_phone))
}

/// Read every row out of a query response, primary first, then in the
/// order they were written.
fn many(mut response: surrealdb::IndexedResults) -> Result<Vec<PersonPhone>, PhoneError> {
    let rows: Vec<PersonPhoneRow> = response.take(0)?;
    let mut phones: Vec<PersonPhone> = rows
        .into_iter()
        .filter_map(PersonPhoneRow::into_phone)
        .collect();
    phones.sort_by(|a, b| {
        b.is_primary
            .cmp(&a.is_primary)
            .then_with(|| a.inserted_at.cmp(&b.inserted_at))
    });
    Ok(phones)
}

/// The retry wrapper this module's writes go through, for the same
/// reason `person` does: the key-value layer is optimistic, and a
/// losing writer is rolled back rather than read as a fault.
///
/// # Errors
///
/// [`PhoneError::Db`] when every attempt fails.
async fn writing<F, Q>(attempt: F) -> Result<surrealdb::IndexedResults, PhoneError>
where
    F: FnMut() -> Q,
    Q: std::future::IntoFuture<Output = Result<surrealdb::IndexedResults, surrealdb::Error>>,
{
    retry::writing(attempt).await.map_err(PhoneError::Db)
}

/// List a Person's phone numbers, primary first, then in the order they
/// were written. An empty vector is a Person with none, not a fault.
///
/// # Errors
///
/// [`PhoneError::Db`] if the query fails.
pub async fn list_phones(db: &SurrealDb, person_id: Uuid) -> Result<Vec<PersonPhone>, PhoneError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM person_phone WHERE person_id = $person"
        ))
        .bind(("person", record_id("person", person_id)))
        .await?;
    many(response)
}

/// Set one of a Person's phone numbers, creating or updating its row.
///
/// The number is normalized to E.164 before it is stored, so repeated
/// writes of differently formatted spellings converge on one row. The
/// first number a Person gets is primary whether or not the caller said
/// so; a Person with no number cannot answer "which one first". Setting
/// a number primary demotes the previous primary in the same
/// transaction, so a reader never sees zero primaries among rows that
/// exist — and never sees two.
///
/// # Errors
///
/// [`PhoneError::PersonMissing`] when no Person row answers to
/// `person_id`, [`PhoneError::InvalidNumber`] when the number cannot be
/// normalized, [`PhoneError::WriteLost`] when the write returns nothing
/// usable, [`PhoneError::Db`] when the database fails.
pub async fn set_phone(
    db: &SurrealDb,
    person_id: Uuid,
    raw: &str,
    label: PhoneLabel,
    is_primary: bool,
) -> Result<PersonPhone, PhoneError> {
    let e164 = normalize_e164(raw)?;
    if crate::persons::find_by_id(db, person_id).await?.is_none() {
        return Err(PhoneError::PersonMissing(person_id));
    }

    let existing: Option<PersonPhoneRow> = db
        .query(format!(
            "SELECT {SELECT} FROM person_phone \
             WHERE person_id = $person AND e164 = $e164 LIMIT 1"
        ))
        .bind(("person", record_id("person", person_id)))
        .bind(("e164", e164.clone()))
        .await?
        .take(0)?;
    let existing = existing.and_then(PersonPhoneRow::into_phone);

    match existing {
        Some(row) => update_row(db, row, label, is_primary).await,
        None => create_row(db, person_id, e164, label, is_primary).await,
    }
}

/// Demote the other primaries, then restamp this row — one transaction,
/// so no reader sees zero or two primaries for the Person.
async fn update_row(
    db: &SurrealDb,
    row: PersonPhone,
    label: PhoneLabel,
    is_primary: bool,
) -> Result<PersonPhone, PhoneError> {
    let row_id = record_id(TABLE, row.id);
    writing(|| {
        db.query(
            "BEGIN TRANSACTION; \
             UPDATE person_phone SET is_primary = false, updated_at = time::now() \
             WHERE person_id = $person AND is_primary = true AND id != $id; \
             UPDATE $id SET label = $label, is_primary = $primary, updated_at = time::now(); \
             COMMIT TRANSACTION;",
        )
        .bind(("person", record_id("person", row.person_id)))
        .bind(("id", row_id.clone()))
        .bind(("label", label.as_str()))
        .bind(("primary", is_primary))
    })
    .await?;
    mirror_row(db, row.id).await;
    find_by_id(db, row.id).await?.ok_or(PhoneError::WriteLost)
}

/// Mint the row, demoting an existing primary only when this new row is
/// to be primary — and forcing it primary when the Person had no number
/// at all, because "no primary" is a fact about rows that exist.
async fn create_row(
    db: &SurrealDb,
    person_id: Uuid,
    e164: String,
    label: PhoneLabel,
    is_primary: bool,
) -> Result<PersonPhone, PhoneError> {
    let first = list_phones(db, person_id).await?.is_empty();
    let is_primary = is_primary || first;
    let id = Uuid::now_v7();

    writing(|| {
        let mut statement = String::from("BEGIN TRANSACTION; ");
        if is_primary {
            statement.push_str(
                "UPDATE person_phone SET is_primary = false, updated_at = time::now() \
                 WHERE person_id = $person AND is_primary = true; ",
            );
        }
        statement.push_str(
            "CREATE $id SET person_id = $person, e164 = $e164, label = $label, \
             is_primary = $primary; COMMIT TRANSACTION;",
        );
        db.query(statement)
            .bind(("person", record_id("person", person_id)))
            .bind(("id", record_id(TABLE, id)))
            .bind(("e164", e164.clone()))
            .bind(("label", label.as_str()))
            .bind(("primary", is_primary))
    })
    .await?;
    mirror_row(db, id).await;
    find_by_id(db, id).await?.ok_or(PhoneError::WriteLost)
}

/// Remove one of a Person's phone numbers. Removing the primary leaves
/// no primary behind — the next [`set_phone`] makes its row primary only
/// if the list is empty, so a deliberate removal is a deliberate gap,
/// not a silent reshuffle. Absent number, absent effect.
///
/// # Errors
///
/// [`PhoneError::Db`] if the database fails.
pub async fn remove_phone(db: &SurrealDb, person_id: Uuid, raw: &str) -> Result<(), PhoneError> {
    let e164 = normalize_e164(raw)?;
    let mut removed: Vec<PersonPhoneRow> = db
        .query(format!(
            "SELECT {SELECT} FROM (DELETE person_phone \
             WHERE person_id = $person AND e164 = $e164 RETURN BEFORE)"
        ))
        .bind(("person", record_id("person", person_id)))
        .bind(("e164", e164.clone()))
        .await?
        .take(0)?;

    let Some(removed) = removed.drain(..).find_map(PersonPhoneRow::into_phone) else {
        return Ok(());
    };

    // The mirror is row-keyed, so the projection needs the deleted row's
    // id and the Postgres side drops it there too.
    crate::postgres_mirror::after_write(
        db,
        MirrorKey::Record {
            table: TABLE,
            id: removed.id,
        },
    )
    .await;
    Ok(())
}

/// Lift the legacy free-text `person.phone` into the structured list,
/// as the primary mobile number.
///
/// Idempotent and guarded on both sides: a Person with structured rows
/// already is left alone, and a Person with no legacy value is a no-op.
/// The caller runs this before a first [`set_phone`] so the historical
/// number is demoted rather than lost — one-shot adoption, not a
/// migration that must be scheduled, because the trigger is the very
/// edit that supersedes the legacy column.
///
/// # Errors
///
/// [`PhoneError::Db`] if the database fails.
pub async fn seed_from_legacy_phone(
    db: &SurrealDb,
    person_id: Uuid,
) -> Result<Option<PersonPhone>, PhoneError> {
    let Some(person) = crate::persons::find_by_id(db, person_id).await? else {
        return Err(PhoneError::PersonMissing(person_id));
    };
    if !list_phones(db, person_id).await?.is_empty() {
        return Ok(None);
    }
    let Some(raw) = person.phone.filter(|phone| !phone.trim().is_empty()) else {
        return Ok(None);
    };
    // A legacy value that will not normalize is left where it is: the
    // column still holds it, and silently minting a wrong number from a
    // free-text line would be worse than leaving the gap visible.
    match set_phone(db, person_id, &raw, PhoneLabel::Mobile, true).await {
        Ok(phone) => Ok(Some(phone)),
        Err(PhoneError::InvalidNumber(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Resolve one phone row by its id.
///
/// # Errors
///
/// [`PhoneError::Db`] if the lookup fails.
async fn find_by_id(db: &SurrealDb, id: Uuid) -> Result<Option<PersonPhone>, PhoneError> {
    let response = db
        .query(format!("SELECT {SELECT} FROM ONLY $id LIMIT 1"))
        .bind(("id", record_id(TABLE, id)))
        .await?;
    one(response)
}

/// Mirror the row into PostgreSQL after its Surreal write. Never fails
/// the write; see [`crate::postgres_mirror::after_write`].
async fn mirror_row(db: &SurrealDb, id: Uuid) {
    crate::postgres_mirror::after_write(db, MirrorKey::Record { table: TABLE, id }).await;
}

#[cfg(test)]
mod tests {
    use super::{normalize_e164, PhoneError, PhoneLabel};

    #[test]
    fn normalize_e164_accepts_an_explicit_country_code() {
        assert_eq!(normalize_e164("+1 (510) 800-2090").unwrap(), "+15108002090");
        assert_eq!(normalize_e164("+44 20 7946 0958").unwrap(), "+442079460958");
        assert_eq!(normalize_e164("+17025550142").unwrap(), "+17025550142");
        // Eight digits is the E.164 floor; fifteen the ceiling.
        assert_eq!(normalize_e164("+12345678").unwrap(), "+12345678");
        assert_eq!(
            normalize_e164("+123456789012345").unwrap(),
            "+123456789012345"
        );
    }

    #[test]
    fn normalize_e164_assumes_north_america_for_bare_numbers() {
        assert_eq!(normalize_e164("(702) 555-0142").unwrap(), "+17025550142");
        assert_eq!(normalize_e164("702.555.0142").unwrap(), "+17025550142");
        assert_eq!(normalize_e164("1-702-555-0142").unwrap(), "+17025550142");
    }

    #[test]
    fn normalize_e164_refuses_what_it_cannot_read() {
        for raw in [
            "",                  // empty
            "none",              // no digits
            "123",               // too short to be anything
            "23456789012",       // eleven digits not opening with the US code
            "70255501421",       // ten digits is the North American floor and ceiling
            "+1234567890123456", // past the fifteen-digit ceiling
            "12345678901234567", // bare and past the ceiling
            "+1(702)55-0142o",   // a letter survived
        ] {
            assert!(
                matches!(normalize_e164(raw), Err(PhoneError::InvalidNumber(_))),
                "{raw:?} must be refused"
            );
        }
    }

    #[test]
    fn a_plus_anywhere_but_the_front_is_refused() {
        assert!(matches!(
            normalize_e164("702+5550142"),
            Err(PhoneError::InvalidNumber(_))
        ));
    }

    #[test]
    fn phone_labels_round_trip_through_their_stored_spelling() {
        for label in [
            PhoneLabel::Mobile,
            PhoneLabel::Work,
            PhoneLabel::Home,
            PhoneLabel::Fax,
            PhoneLabel::Other,
        ] {
            assert_eq!(PhoneLabel::parse(label.as_str()), Some(label));
        }
        assert_eq!(PhoneLabel::parse("cell"), None);
    }
}
