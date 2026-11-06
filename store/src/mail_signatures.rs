//! The email signature a [Person](crate::persons) attorney signs their mail
//! drafts with (LAW-280).
//!
//! One `mail_signature` row per Person, born in PostgreSQL with no Surreal
//! source, keyed by the Person's id. The attorney themself edits it;
//! [`save_for_actor`] carries that gate by refusing an actor whose id is not
//! the subject's with the same non-disclosing [`MailSignatureError::NotFound`]
//! an unknown subject gets — whoever may set a signature signs mail as that
//! attorney, so nothing else may.
//!
//! The stored row ([`MailSignatureFields`]) holds the signature text exactly
//! as it should appear above the `-- ` delimiter the draft command manages,
//! so [`validate`] refuses a body carrying a stray delimiter line of its own.
//! [`for_person`] is the one read every consumer uses, and its
//! [`MailSignature::updated_at`] is the revision a draft's provenance names.
//!
//! The name is deliberately `mail_signatures`: [`crate::signatures`] already
//! means DocuSign wet-ink e-signature plumbing, and a grep for "signature"
//! must not conflate the two.
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub enum MailSignatureError {
    /// A field failed validation; the message names it for the form.
    #[error("{0}")]
    Invalid(String),
    /// The subject Person does not exist, or the actor may not edit them.
    /// One variant so a caller answers both with the same non-disclosing 404
    /// that never reveals which identities exist.
    #[error("no such person")]
    NotFound,
    #[error("person lookup failed: {0}")]
    Person(#[from] crate::persons::PersonError),
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

/// What an attorney set as the block that signs their mail.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MailSignatureFields {
    /// The signature text above the `-- ` delimiter, natural case.
    pub body: String,
}

/// The stored signature and its revision: the block a draft wears, and the
/// `updated_at` its provenance names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailSignature {
    pub person_id: Uuid,
    pub body: String,
    /// The stored row's last edit as RFC 3339 — the revision a draft's
    /// provenance names. `None` when no row was read.
    pub updated_at: Option<String>,
}

/// The RFC 3676 signature delimiter, which the draft command inserts and
/// manages itself: a stored body never carries a line of its own.
const SIG_DELIMITER: &str = "--";

/// The most characters a stored signature may run to: a footer, not a letter.
const BODY_MAX_CHARS: usize = 2000;

/// Trim and validate an attorney's submission.
///
/// # Errors
/// [`MailSignatureError::Invalid`] naming the first refusal.
pub fn validate(input: &MailSignatureFields) -> Result<MailSignatureFields, MailSignatureError> {
    let invalid = |message: String| Err(MailSignatureError::Invalid(message));
    let body = input.body.trim();
    if body.is_empty() {
        return invalid("The signature must not be blank.".to_string());
    }
    if body.chars().count() > BODY_MAX_CHARS {
        return invalid(format!(
            "The signature must be at most {BODY_MAX_CHARS} characters."
        ));
    }
    if body.lines().any(|line| line.trim() == SIG_DELIMITER) {
        return invalid(
            "The signature must not contain a `--` delimiter line; \
             the draft command adds the delimiter itself."
                .to_string(),
        );
    }
    Ok(MailSignatureFields {
        body: body.to_string(),
    })
}

/// The stored row and its revision: the fields the attorney set, and the
/// `updated_at` a draft's provenance names.
struct StoredRow {
    fields: MailSignatureFields,
    updated_at: chrono::DateTime<chrono::Utc>,
}

fn from_row_with_updated_at(row: &sqlx::postgres::PgRow) -> Result<StoredRow, sqlx::Error> {
    Ok(StoredRow {
        fields: from_row(row)?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<MailSignatureFields, sqlx::Error> {
    Ok(MailSignatureFields {
        body: row.try_get("body")?,
    })
}

/// The stored signature for a Person id, or `None` when none is set.
/// Callers authorize first; [`save_for_actor`] is the authorized path.
///
/// # Errors
/// [`MailSignatureError::Database`] when the read fails.
pub async fn find(
    pg: &PostgresDb,
    person_id: Uuid,
) -> Result<Option<MailSignatureFields>, MailSignatureError> {
    Ok(find_row(pg, person_id).await?.map(|row| row.fields))
}

/// The stored row and its revision, or `None` when none is set.
async fn find_row(
    pg: &PostgresDb,
    person_id: Uuid,
) -> Result<Option<StoredRow>, MailSignatureError> {
    let row = sqlx::query("SELECT * FROM mail_signature WHERE person_id = $1")
        .bind(person_id)
        .fetch_optional(&pg.0)
        .await?;
    Ok(row.as_ref().map(from_row_with_updated_at).transpose()?)
}

/// Replace a Person's stored signature with already-[`validate`]d fields.
/// Callers authorize first; [`save_for_actor`] is the authorized path.
///
/// # Errors
/// [`MailSignatureError::Database`] when the write fails.
pub async fn put(
    pg: &PostgresDb,
    person_id: Uuid,
    fields: &MailSignatureFields,
) -> Result<MailSignatureFields, MailSignatureError> {
    Ok(put_row(pg, person_id, fields).await?.fields)
}

/// [`put`], keeping the revision the write stamped.
async fn put_row(
    pg: &PostgresDb,
    person_id: Uuid,
    fields: &MailSignatureFields,
) -> Result<StoredRow, MailSignatureError> {
    let row = sqlx::query(
        "INSERT INTO mail_signature (person_id, body) VALUES ($1, $2) \
         ON CONFLICT (person_id) DO UPDATE SET body = EXCLUDED.body, updated_at = now() \
         RETURNING *",
    )
    .bind(person_id)
    .bind(fields.body.as_str())
    .fetch_one(&pg.0)
    .await?;
    Ok(from_row_with_updated_at(&row)?)
}

/// Save a Person's mail signature as an actor: the attorney themself, only.
/// Any other actor, and an unknown subject, is
/// [`MailSignatureError::NotFound`] — the same non-disclosing refusal either
/// way, so an error never reveals which identities exist.
///
/// # Errors
/// [`MailSignatureError::NotFound`] for another actor or an unknown subject,
/// [`MailSignatureError::Invalid`] for a refused field, and the store errors.
pub async fn save_for_actor(
    surreal: &crate::surreal::SurrealDb,
    pg: &PostgresDb,
    actor_person_id: Uuid,
    subject_person_id: Uuid,
    input: &MailSignatureFields,
) -> Result<MailSignature, MailSignatureError> {
    let fields = validate(input)?;
    if actor_person_id != subject_person_id {
        return Err(MailSignatureError::NotFound);
    }
    if crate::persons::find_by_id(surreal, subject_person_id)
        .await?
        .is_none()
    {
        return Err(MailSignatureError::NotFound);
    }
    let stored = put_row(pg, subject_person_id, &fields).await?;
    Ok(MailSignature {
        person_id: subject_person_id,
        body: stored.fields.body,
        updated_at: Some(stored.updated_at.to_rfc3339()),
    })
}

/// The stored signature for a Person id, or `None` when the Person is
/// unknown or has not set one — the read every consumer uses.
///
/// # Errors
/// The Person lookup and the PostgreSQL read errors.
pub async fn for_person(
    surreal: &crate::surreal::SurrealDb,
    pg: &PostgresDb,
    person_id: Uuid,
) -> Result<Option<MailSignature>, MailSignatureError> {
    if crate::persons::find_by_id(surreal, person_id)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    Ok(find_row(pg, person_id).await?.map(|row| MailSignature {
        person_id,
        body: row.fields.body,
        updated_at: Some(row.updated_at.to_rfc3339()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(body: &str) -> MailSignatureFields {
        MailSignatureFields {
            body: body.to_string(),
        }
    }

    #[test]
    fn validation_trims_surrounding_blank_lines_and_spaces() {
        let clean = validate(&fields("  Example Attorney\nConfidential notice.  ")).unwrap();
        assert_eq!(clean.body, "Example Attorney\nConfidential notice.");
    }

    #[test]
    fn a_blank_signature_is_refused() {
        for body in ["", "   ", "\n"] {
            let error = validate(&fields(body)).unwrap_err().to_string();
            assert!(error.contains("blank"), "{error}");
        }
    }

    #[test]
    fn a_signature_longer_than_its_cap_is_refused() {
        let long = "x".repeat(2001);
        let error = validate(&fields(&long)).unwrap_err().to_string();
        assert!(error.contains("at most 2000"), "{error}");
    }

    #[test]
    fn a_stray_delimiter_line_is_refused() {
        for body in [
            "-- \nExample Attorney",
            "Example Attorney\n--",
            "Example Attorney\n-- \nConfidential notice.",
        ] {
            let error = validate(&fields(body)).unwrap_err().to_string();
            assert!(error.contains("delimiter"), "{error}");
        }
    }

    #[test]
    fn delimiter_lookalikes_that_are_not_delimiter_lines_are_kept() {
        // An em-dash aside, a longer rule, and an inline mention of the
        // delimiter are prose, not the RFC 3676 separator line.
        for body in [
            "Example Attorney -- Senior Partner\nConfidential notice.",
            "Example Attorney\n---\nConfidential notice.",
        ] {
            assert_eq!(validate(&fields(body)).unwrap().body, body);
        }
    }
}
