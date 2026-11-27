//! Physical mail, and every query against a `letter` row.
//!
//! PostgreSQL enforces each Letter's Mailroom relationship. Source reads
//! remain available for historical import.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::mailrooms::{self, MailroomError};

/// The table these rows live in.
pub(crate) const TABLE: &str = "letter";

/// Mail arriving at a mailroom.
pub const DIRECTION_INCOMING: &str = "incoming";
/// Mail leaving one.
pub const DIRECTION_OUTGOING: &str = "outgoing";

/// One piece of physical mail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Letter {
    pub id: Uuid,
    pub mailroom_id: Uuid,
    /// The matter this letter belongs to, when one is known. `None` for
    /// every historical row and for mail not yet linked to a matter — see
    /// the schema note on `letter.project_id`.
    pub project_id: Option<Uuid>,
    /// [`DIRECTION_INCOMING`] or [`DIRECTION_OUTGOING`].
    pub direction: String,
    pub sender: String,
    pub recipient: String,
    pub summary: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Everything [`record`] needs to log one piece of mail.
#[derive(Debug, Clone)]
pub struct NewLetter {
    pub mailroom_id: Uuid,
    pub project_id: Option<Uuid>,
    pub direction: String,
    pub sender: String,
    pub recipient: String,
    pub summary: String,
}

/// Errors reading or writing a letter.
#[derive(Debug, thiserror::Error)]
pub enum LetterError {
    #[error(transparent)]
    Postgres(#[from] crate::postgres::matter::MatterAuthorityError),
    /// A database operation failed.
    /// Reading the mailroom a write links to failed.
    #[error(transparent)]
    Mailroom(#[from] MailroomError),
    /// The mailroom named by a write does not exist. The engine would
    /// have accepted the dangling link; this is the check that does not.
    #[error("no mailroom {0}")]
    NoSuchMailroom(Uuid),
    /// A write reported success but returned no row, or returned one
    /// this module could not read back.
    #[error("writing a letter returned no usable row")]
    WriteReturnedNothing,
    #[error("letter replay differs from the recorded letter")]
    ReplayConflict,
}

/// Log one piece of mail at its mailroom.
///
/// # Errors
///
/// [`LetterError::NoSuchMailroom`] when the mailroom does not exist —
/// checked before writing — and
/// a database error if the insert fails, including an invalid direction.
pub async fn record(db: &ApplicationStore, new: &NewLetter) -> Result<Letter, LetterError> {
    record_with_id(db, Uuid::now_v7(), new).await
}

/// Record a letter at a caller-selected id.
///
/// The inbound summary lane uses a digest-derived id so a retry after a
/// partial archive/database failure can finish the same letter without
/// allocating a second one.
pub async fn record_with_id(
    db: &ApplicationStore,
    id: Uuid,
    new: &NewLetter,
) -> Result<Letter, LetterError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    if mailrooms::find_by_id(db, new.mailroom_id).await?.is_none() {
        return Err(LetterError::NoSuchMailroom(new.mailroom_id));
    }
    let row: serde_json::Value = sqlx::query_scalar(
        "INSERT INTO letter AS r (id,mailroom_id,project_id,direction,sender,recipient,summary)
         VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (id) DO NOTHING RETURNING to_jsonb(r)",
    )
    .bind(id)
    .bind(new.mailroom_id)
    .bind(new.project_id)
    .bind(&new.direction)
    .bind(&new.sender)
    .bind(&new.recipient)
    .bind(&new.summary)
    .fetch_optional(pg.pool())
    .await
    .map_err(crate::postgres::matter::MatterAuthorityError::from)?
    .unwrap_or(serde_json::Value::Null);
    let existing: Letter = if row.is_null() {
        crate::postgres::matter::find(&pg, TABLE, serde_json::json!({"id": id}))
            .await?
            .ok_or(LetterError::WriteReturnedNothing)?
    } else {
        serde_json::from_value(row).map_err(crate::postgres::matter::MatterAuthorityError::from)?
    };
    if existing.mailroom_id != new.mailroom_id
        || existing.project_id != new.project_id
        || existing.direction != new.direction
        || existing.sender != new.sender
        || existing.recipient != new.recipient
        || existing.summary != new.summary
    {
        return Err(LetterError::ReplayConflict);
    }
    Ok(existing)
}

/// One letter by id.
///
/// # Errors
///
/// [`LetterError::Db`] if the lookup fails.
pub async fn find_by_id(_db: &ApplicationStore, id: Uuid) -> Result<Option<Letter>, LetterError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    Ok(crate::postgres::matter::find(&pg, TABLE, serde_json::json!({"id": id})).await?)
}

/// The letter at this mailroom with this sender and summary, if any —
/// the natural key the canonical seeder matches on.
///
/// # Errors
///
/// [`LetterError::Db`] if the lookup fails.
pub async fn find_by_mailroom_sender_summary(
    _db: &ApplicationStore,
    mailroom_id: Uuid,
    sender: &str,
    summary: &str,
) -> Result<Option<Letter>, LetterError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    Ok(crate::postgres::matter::find(
        &pg,
        TABLE,
        serde_json::json!({"mailroom_id": mailroom_id,"sender": sender,"summary": summary}),
    )
    .await?)
}

/// Every letter, oldest first — the lawyer listing's order.
///
/// # Errors
///
/// [`LetterError::Db`] if the lookup fails.
pub async fn list_all(_db: &ApplicationStore) -> Result<Vec<Letter>, LetterError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    Ok(
        crate::postgres::matter::select(&pg, TABLE, serde_json::json!({}), &["inserted_at", "id"])
            .await?,
    )
}

/// How many letters exist. The production-emptiness gate asks this of
/// the engine that holds the table.
///
/// # Errors
///
/// [`LetterError::Db`] if the count fails.
pub async fn count(_db: &ApplicationStore) -> Result<i64, LetterError> {
    let pg = crate::postgres::matter::authority_for(TABLE).await?;
    sqlx::query_scalar("SELECT count(*) FROM letter")
        .fetch_one(pg.pool())
        .await
        .map_err(crate::postgres::matter::MatterAuthorityError::from)
        .map_err(LetterError::from)
}

#[cfg(test)]
mod tests {
    use super::{
        count, find_by_id, find_by_mailroom_sender_summary, list_all, record, record_with_id,
        LetterError, NewLetter, DIRECTION_INCOMING, DIRECTION_OUTGOING,
    };
    use crate::addresses::{self, NewAddress};
    use crate::application_store::ApplicationStore;
    use crate::mailrooms;
    use crate::test_support;
    use crate::test_support::seed_project_id;
    use uuid::Uuid;

    async fn a_mailroom(db: &ApplicationStore, name: &str) -> Uuid {
        let address = addresses::create(
            db,
            &NewAddress {
                line1: "123 Main St".into(),
                city: "Reno".into(),
                region: "NV".into(),
                postal_code: "89501".into(),
                country: "US".into(),
                ..NewAddress::default()
            },
        )
        .await
        .unwrap();
        mailrooms::create(db, name, address.id).await.unwrap().id
    }

    fn a_letter(mailroom_id: Uuid, summary: &str) -> NewLetter {
        NewLetter {
            mailroom_id,
            project_id: None,
            direction: DIRECTION_INCOMING.to_string(),
            sender: "IRS".into(),
            recipient: "Acme".into(),
            summary: summary.to_string(),
        }
    }

    #[tokio::test]
    async fn a_recorded_letter_reads_back_by_id() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let mailroom_id = a_mailroom(&db, "HQ").await;

            let written = record(&db, &a_letter(mailroom_id, "Form 990 reminder"))
                .await
                .unwrap();
            assert_eq!(written.mailroom_id, mailroom_id);
            assert_eq!(written.project_id, None);
            assert_eq!(written.summary, "Form 990 reminder");
            assert_eq!(written.direction, DIRECTION_INCOMING);
            assert_eq!(
                find_by_id(&db, written.id).await.unwrap(),
                Some(written.clone())
            );
            let input = a_letter(mailroom_id, "Form 990 reminder");
            assert_eq!(
                record_with_id(&db, written.id, &input).await.unwrap(),
                written
            );
            let conflict = a_letter(mailroom_id, "Different letter");
            assert!(matches!(
                record_with_id(&db, written.id, &conflict).await,
                Err(LetterError::ReplayConflict)
            ));
        })
        .await;
    }

    /// ENG-310: a letter recorded for a known matter carries it through the
    /// round trip untouched.
    #[tokio::test]
    async fn a_letter_scoped_to_a_project_reads_back_with_it() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let mailroom_id = a_mailroom(&db, "HQ").await;
            test_support::seed_compiled_brands_postgres(&db).await;
            let project_id = seed_project_id(&db, "matter").await;

            let mut letter = a_letter(mailroom_id, "Engagement notice");
            letter.project_id = Some(project_id);
            let written = record(&db, &letter).await.unwrap();

            assert_eq!(written.project_id, Some(project_id));
            assert_eq!(
                find_by_id(&db, written.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .project_id,
                Some(project_id)
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_letter_at_a_mailroom_that_does_not_exist_is_refused() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let nowhere = Uuid::now_v7();

            let refused = record(&db, &a_letter(nowhere, "Ghost mail")).await;
            assert!(matches!(
                refused,
                Err(LetterError::NoSuchMailroom(id)) if id == nowhere
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn an_unknown_direction_is_refused_by_the_schema() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let mailroom_id = a_mailroom(&db, "HQ").await;
            let mut sideways = a_letter(mailroom_id, "Sideways mail");
            sideways.direction = "sideways".into();

            // A typo must fail before it can become a recorded letter.
            let refused = record(&db, &sideways).await;
            assert!(
                matches!(refused, Err(LetterError::Postgres(_))),
                "got {refused:?}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn both_directions_are_accepted() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let mailroom_id = a_mailroom(&db, "HQ").await;
            for direction in [DIRECTION_INCOMING, DIRECTION_OUTGOING] {
                let mut letter = a_letter(mailroom_id, direction);
                letter.direction = direction.to_string();
                let written = record(&db, &letter).await.unwrap();
                assert_eq!(written.direction, direction);
            }
            assert_eq!(count(&db).await.unwrap(), 2);
        })
        .await;
    }

    #[tokio::test]
    async fn the_seeders_natural_key_finds_only_that_letter() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let mailroom_id = a_mailroom(&db, "HQ").await;
            let written = record(&db, &a_letter(mailroom_id, "Form 990 reminder"))
                .await
                .unwrap();

            assert_eq!(
                find_by_mailroom_sender_summary(&db, mailroom_id, "IRS", "Form 990 reminder")
                    .await
                    .unwrap(),
                Some(written)
            );
            assert_eq!(
                find_by_mailroom_sender_summary(&db, mailroom_id, "IRS", "Something else")
                    .await
                    .unwrap(),
                None
            );
        })
        .await;
    }

    #[tokio::test]
    async fn a_letter_is_scoped_to_its_own_mailroom() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let hq = a_mailroom(&db, "HQ").await;
            let annex = a_mailroom(&db, "Annex").await;
            record(&db, &a_letter(hq, "Form 990 reminder"))
                .await
                .unwrap();

            assert_eq!(
                find_by_mailroom_sender_summary(&db, annex, "IRS", "Form 990 reminder")
                    .await
                    .unwrap(),
                None,
                "the same sender and summary at another mailroom is another letter"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn counting_an_empty_table_is_zero_rather_than_an_error() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            assert_eq!(count(&db).await.unwrap(), 0);
            let mailroom_id = a_mailroom(&db, "HQ").await;
            record(&db, &a_letter(mailroom_id, "One")).await.unwrap();
            assert_eq!(count(&db).await.unwrap(), 1);
            assert_eq!(list_all(&db).await.unwrap().len(), 1);
        })
        .await;
    }
}
