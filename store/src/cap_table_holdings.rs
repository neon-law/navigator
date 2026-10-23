//! Project-scoped [Cap Table Holding](../../docs/glossary/cap-table-holding.md)
//! rows (`cap_table_holding`): one holder's shares of one class.
//!
//! The holder is one Person or one Entity. `shares` is a whole
//! number of shares, never a percentage; a reader derives ownership from the
//! rows. One row holds each (holder, share class) line on a Project.

use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::matter_record_links::{
    optional_text, party_from_record, party_record, require_party, require_project, required_text,
};
use crate::project_references::Party;
use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

const TABLE: &str = "cap_table_holding";
const LINE_INDEX: &str = "cap_table_holding_line";

/// One stored Cap Table Holding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapTableHolding {
    pub id: Uuid,
    pub project_id: Uuid,
    pub holder: Party,
    pub share_class: String,
    pub shares: i64,
    pub vesting_note: Option<String>,
    pub inserted_at: String,
    pub updated_at: String,
}

/// The fields a caller writes. Insert and update both take the whole set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapTableHoldingFields {
    pub holder: Party,
    pub share_class: String,
    pub shares: i64,
    pub vesting_note: Option<String>,
}

#[derive(SurrealValue)]
struct CapTableHoldingRow {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    holder_id: surrealdb::types::RecordId,
    share_class: String,
    shares: i64,
    vesting_note: Option<String>,
    inserted_at: String,
    updated_at: String,
}

impl CapTableHoldingRow {
    fn into_holding(self) -> Option<CapTableHolding> {
        Some(CapTableHolding {
            id: record_uuid(&self.id)?,
            project_id: record_uuid(&self.project_id)?,
            holder: party_from_record(&self.holder_id)?,
            share_class: self.share_class,
            shares: self.shares,
            vesting_note: self.vesting_note,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

const SELECT: &str = "id, project_id, holder_id, share_class, shares, vesting_note, inserted_at, \
     updated_at";

#[derive(Debug, thiserror::Error)]
pub enum CapTableHoldingError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("no such cap table holding")]
    NotFound,
    #[error("writing a cap table holding returned no usable row")]
    WriteReturnedNothing,
}

/// The fields as stored: trimmed and checked, the holder resolved.
async fn validate(
    surreal: &SurrealDb,
    project_id: Uuid,
    fields: &CapTableHoldingFields,
) -> Result<CapTableHoldingFields, CapTableHoldingError> {
    let share_class =
        required_text("share_class", &fields.share_class).map_err(CapTableHoldingError::Invalid)?;
    if fields.shares < 0 {
        return Err(CapTableHoldingError::Invalid(
            "shares must be zero or more".into(),
        ));
    }
    require_project(surreal, project_id)
        .await
        .map_err(CapTableHoldingError::Invalid)?;
    require_party(surreal, "holder", fields.holder)
        .await
        .map_err(CapTableHoldingError::Invalid)?;
    Ok(CapTableHoldingFields {
        holder: fields.holder,
        share_class,
        shares: fields.shares,
        vesting_note: optional_text(fields.vesting_note.as_deref()),
    })
}

async fn persist(
    surreal: &SurrealDb,
    id: Uuid,
    project_id: Uuid,
    fields: CapTableHoldingFields,
    inserted_at: &str,
) -> Result<CapTableHolding, CapTableHoldingError> {
    let now = chrono::Utc::now().to_rfc3339();
    let response = surreal
        .query(format!(
            "UPSERT $id SET project_id = $project_id, holder_id = $holder_id, \
             share_class = $share_class, shares = $shares, \
             vesting_note = $vesting_note, inserted_at = $inserted_at, updated_at = $now \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("holder_id", party_record(fields.holder)))
        .bind(("share_class", fields.share_class))
        .bind(("shares", fields.shares))
        .bind(("vesting_note", fields.vesting_note))
        .bind(("inserted_at", inserted_at.to_string()))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check);
    let mut response = match response {
        Ok(response) => response,
        Err(error) if retry::unique_violation(&error) == Some(LINE_INDEX) => {
            return Err(CapTableHoldingError::Invalid(
                "this holder already has a line for that share class on this Project".into(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let row: Option<CapTableHoldingRow> = response.take(0)?;
    row.and_then(CapTableHoldingRow::into_holding)
        .ok_or(CapTableHoldingError::WriteReturnedNothing)
}

/// A Project's holdings, by share class, largest holding first.
///
/// # Errors
///
/// [`CapTableHoldingError::Db`] if the read fails.
pub async fn list(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<CapTableHolding>, CapTableHoldingError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project_id \
             ORDER BY share_class, shares DESC, id"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<CapTableHoldingRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(CapTableHoldingRow::into_holding)
        .collect())
}

/// One Cap Table Holding on a Project, or `None`.
///
/// # Errors
///
/// [`CapTableHoldingError::Db`] if the read fails.
pub async fn find(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<CapTableHolding>, CapTableHoldingError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} \
             WHERE id = $id AND project_id = $project_id LIMIT 1"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<CapTableHoldingRow> = response.take(0)?;
    Ok(row.and_then(CapTableHoldingRow::into_holding))
}

/// The row holding one (holder, share class) line, or `None`.
///
/// # Errors
///
/// [`CapTableHoldingError::Db`] if the read fails.
pub async fn find_by_line(
    surreal: &SurrealDb,
    project_id: Uuid,
    holder: Party,
    share_class: &str,
) -> Result<Option<CapTableHolding>, CapTableHoldingError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} WHERE project_id = $project_id \
             AND holder_id = $holder_id AND share_class = $share_class LIMIT 1"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("holder_id", party_record(holder)))
        .bind(("share_class", share_class.trim().to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<CapTableHoldingRow> = response.take(0)?;
    Ok(row.and_then(CapTableHoldingRow::into_holding))
}

/// Store a new Cap Table Holding and return it.
///
/// # Errors
///
/// [`CapTableHoldingError::Invalid`] for a blank share class, negative
/// shares, a missing Project, a holder with no row behind it, or a line already held; [`CapTableHoldingError::Db`] if
/// the write fails.
pub async fn insert(
    surreal: &SurrealDb,
    project_id: Uuid,
    fields: &CapTableHoldingFields,
) -> Result<CapTableHolding, CapTableHoldingError> {
    let fields = validate(surreal, project_id, fields).await?;
    let inserted_at = chrono::Utc::now().to_rfc3339();
    persist(surreal, Uuid::now_v7(), project_id, fields, &inserted_at).await
}

/// Replace a Cap Table Holding's fields and return the stored row.
///
/// # Errors
///
/// [`CapTableHoldingError::NotFound`] when the row is not on this Project;
/// otherwise as [`insert`].
pub async fn update(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
    fields: &CapTableHoldingFields,
) -> Result<CapTableHolding, CapTableHoldingError> {
    let existing = find(surreal, project_id, id)
        .await?
        .ok_or(CapTableHoldingError::NotFound)?;
    let fields = validate(surreal, project_id, fields).await?;
    persist(
        surreal,
        existing.id,
        project_id,
        fields,
        &existing.inserted_at,
    )
    .await
}

/// Remove one Cap Table Holding.
///
/// # Errors
///
/// [`CapTableHoldingError::NotFound`] when the row is not on this Project;
/// [`CapTableHoldingError::Db`] if the delete fails.
pub async fn delete(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<(), CapTableHoldingError> {
    find(surreal, project_id, id)
        .await?
        .ok_or(CapTableHoldingError::NotFound)?;
    surreal
        .query("DELETE $id")
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    Ok(())
}

/// Remove every Cap Table Holding on a Project. The rows are imported from
/// the Project repository's seeds, so they go with the Project row.
///
/// # Errors
///
/// [`CapTableHoldingError::Db`] if the delete fails.
pub async fn delete_for_project(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<(), CapTableHoldingError> {
    surreal
        .query(format!("DELETE {TABLE} WHERE project_id = $project_id"))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persons::{NewPerson, Role};
    use crate::test_support::{mem_surreal, seed_entity};

    async fn project(surreal: &SurrealDb, code: &str) -> Uuid {
        crate::projects::create(
            surreal,
            &crate::projects::NewProject {
                code: code.into(),
                name: "Sample outside counsel".into(),
                status: "open".into(),
                entity_id: seed_entity(surreal).await,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn person(surreal: &SurrealDb, email: &str) -> Uuid {
        crate::persons::create(
            surreal,
            &NewPerson::with_role("Sample Holder", email, Role::Client),
        )
        .await
        .unwrap()
        .id
    }

    fn holding(holder: Uuid, share_class: &str, shares: i64) -> CapTableHoldingFields {
        CapTableHoldingFields {
            holder: Party::person(holder),
            share_class: share_class.into(),
            shares,
            vesting_note: None,
        }
    }

    #[tokio::test]
    async fn insert_list_update_and_delete_round_trip() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-cap-table").await;
        let founder = person(&surreal, "founder@example.com").await;
        let fund = seed_entity(&surreal).await;

        let common = insert(
            &surreal,
            project_id,
            &holding(founder, " Common ", 6_000_000),
        )
        .await
        .unwrap();
        assert_eq!(common.share_class, "Common");
        let preferred = insert(
            &surreal,
            project_id,
            &CapTableHoldingFields {
                holder: Party::entity(fund),
                ..holding(founder, "Series Seed Preferred", 1_500_000)
            },
        )
        .await
        .unwrap();
        let options = insert(
            &surreal,
            project_id,
            &CapTableHoldingFields {
                vesting_note: Some("Four years, one-year cliff".into()),
                ..holding(
                    person(&surreal, "employee@example.com").await,
                    "Common",
                    250_000,
                )
            },
        )
        .await
        .unwrap();

        let table: Vec<Uuid> = list(&surreal, project_id)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(table, vec![common.id, options.id, preferred.id]);

        let updated = update(
            &surreal,
            project_id,
            common.id,
            &holding(founder, "Common", 5_750_000),
        )
        .await
        .unwrap();
        assert_eq!(updated.shares, 5_750_000);
        assert_eq!(updated.inserted_at, common.inserted_at);

        delete(&surreal, project_id, options.id).await.unwrap();
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 2);
        assert!(matches!(
            delete(&surreal, project_id, options.id).await,
            Err(CapTableHoldingError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_holder_with_no_row_behind_it_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-cap-holder").await;
        for holder in [Party::person(Uuid::now_v7()), Party::entity(Uuid::now_v7())] {
            let error = insert(
                &surreal,
                project_id,
                &CapTableHoldingFields {
                    holder,
                    ..holding(Uuid::now_v7(), "Common", 1)
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(error, CapTableHoldingError::Invalid(_)), "{error}");
        }
        assert!(list(&surreal, project_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn negative_shares_or_a_blank_class_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-cap-refusals").await;
        let person_id = person(&surreal, "refused@example.com").await;
        for bad in [
            holding(person_id, "Common", -1),
            holding(person_id, "  ", 10),
        ] {
            assert!(matches!(
                insert(&surreal, project_id, &bad).await.unwrap_err(),
                CapTableHoldingError::Invalid(_)
            ));
        }
    }

    #[tokio::test]
    async fn one_row_holds_each_holder_and_class() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-cap-line").await;
        let person_id = person(&surreal, "line@example.com").await;
        let other = person(&surreal, "line-other@example.com").await;
        insert(&surreal, project_id, &holding(person_id, "Common", 100))
            .await
            .unwrap();
        insert(&surreal, project_id, &holding(other, "Common", 100))
            .await
            .unwrap();
        let error = insert(&surreal, project_id, &holding(person_id, "Common", 5))
            .await
            .unwrap_err();
        assert!(matches!(error, CapTableHoldingError::Invalid(_)), "{error}");
        let found = find_by_line(&surreal, project_id, Party::person(person_id), " Common")
            .await
            .unwrap()
            .expect("the line");
        assert_eq!(found.shares, 100);
    }

    #[tokio::test]
    async fn the_engine_refuses_negative_shares() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-cap-engine").await;
        let result = surreal
            .query(
                "CREATE cap_table_holding SET project_id = $project_id, holder_id = $holder, \
                 share_class = 'Common', shares = -5, inserted_at = '', updated_at = ''",
            )
            .bind(("project_id", record_id("project", project_id)))
            .bind(("holder", record_id("entity", seed_entity(&surreal).await)))
            .await
            .and_then(surrealdb::IndexedResults::check);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rows_are_scoped_to_their_project() {
        let surreal = mem_surreal().await;
        let mine = project(&surreal, "sample-cap-mine").await;
        let other = project(&surreal, "sample-cap-other").await;
        let person_id = person(&surreal, "cap-scoped@example.com").await;
        let stored = insert(&surreal, mine, &holding(person_id, "Common", 10))
            .await
            .unwrap();
        assert!(list(&surreal, other).await.unwrap().is_empty());
        assert!(find(&surreal, other, stored.id).await.unwrap().is_none());
        delete_for_project(&surreal, mine).await.unwrap();
        assert!(list(&surreal, mine).await.unwrap().is_empty());
    }
}
