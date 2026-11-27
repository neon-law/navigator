//! Project-scoped [Cap Table Holding](../../docs/glossary/cap-table-holding.md)
//! rows (`cap_table_holding`): one holder's shares of one class.
//!
//! The holder is one Person or one Entity. `shares` is a whole
//! number of shares, never a percentage; a reader derives ownership from the
//! rows. One row holds each (holder, share class) line on a Project.

use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::matter_record_links::{optional_text, required_text};
use crate::postgres::matter::{self, MatterAuthorityError};
use crate::project_references::Party;
use crate::relationships::Endpoint;

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

/// The row as PostgreSQL stores it: the holder is a tagged `JSONB` link.
#[derive(Serialize, serde::Deserialize)]
struct PgRow {
    id: Uuid,
    project_id: Uuid,
    holder_id: serde_json::Value,
    share_class: String,
    shares: i64,
    vesting_note: Option<String>,
    inserted_at: String,
    updated_at: String,
}

impl PgRow {
    fn into_holding(self) -> Option<CapTableHolding> {
        let (table, id) = matter::untagged(&self.holder_id)?;
        Some(CapTableHolding {
            id: self.id,
            project_id: self.project_id,
            holder: Party {
                kind: Endpoint::from_table(table)?,
                id,
            },
            share_class: self.share_class,
            shares: self.shares,
            vesting_note: self.vesting_note,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CapTableHoldingError {
    #[error(transparent)]
    Postgres(MatterAuthorityError),
    #[error("{0}")]
    Invalid(String),
    #[error("no such cap table holding")]
    NotFound,
    #[error("writing a cap table holding returned no usable row")]
    WriteReturnedNothing,
}

const DUPLICATE_LINE: &str = "this holder already has a line for that share class on this Project";

impl From<MatterAuthorityError> for CapTableHoldingError {
    fn from(error: MatterAuthorityError) -> Self {
        match error {
            MatterAuthorityError::Conflict(index) if index == LINE_INDEX => {
                Self::Invalid(DUPLICATE_LINE.into())
            }
            error => Self::Postgres(error),
        }
    }
}

fn holdings(rows: Vec<PgRow>) -> Vec<CapTableHolding> {
    rows.into_iter().filter_map(PgRow::into_holding).collect()
}

/// The fields as stored: trimmed and checked, the holder resolved.
async fn validate(
    _application_store: &ApplicationStore,
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
    {
        let pg = matter::authority().await?;
        crate::matter_record_links::require_project_on(&pg, project_id)
            .await
            .map_err(CapTableHoldingError::Invalid)?;
    }
    Ok(CapTableHoldingFields {
        holder: fields.holder,
        share_class,
        shares: fields.shares,
        vesting_note: optional_text(fields.vesting_note.as_deref()),
    })
}

async fn persist(
    _application_store: &ApplicationStore,
    id: Uuid,
    project_id: Uuid,
    fields: CapTableHoldingFields,
    inserted_at: &str,
) -> Result<CapTableHolding, CapTableHoldingError> {
    let now = chrono::Utc::now().to_rfc3339();
    let pg = matter::authority().await?;
    let row = PgRow {
        id,
        project_id,
        holder_id: matter::tagged(fields.holder.kind.table(), fields.holder.id),
        share_class: fields.share_class,
        shares: fields.shares,
        vesting_note: fields.vesting_note,
        inserted_at: inserted_at.to_owned(),
        updated_at: now,
    };
    let mut transaction = pg
        .pool()
        .begin()
        .await
        .map_err(MatterAuthorityError::from)?;
    if !crate::matter_record_links::lock_party_on(&mut transaction, fields.holder).await? {
        let noun = match fields.holder.kind {
            Endpoint::Person => "Person",
            Endpoint::Entity => "Entity",
        };
        return Err(CapTableHoldingError::Invalid(format!(
            "the holder names no such {noun}"
        )));
    }
    let stored: PgRow = matter::upsert_on(
        &mut *transaction,
        TABLE,
        serde_json::to_value(row).map_err(MatterAuthorityError::from)?,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(MatterAuthorityError::from)?;
    stored
        .into_holding()
        .ok_or(CapTableHoldingError::WriteReturnedNothing)
}

/// A Project's holdings, by share class, largest holding first.
///
/// # Errors
///
/// [`CapTableHoldingError::Db`] if the read fails.
pub async fn list(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<CapTableHolding>, CapTableHoldingError> {
    let pg = matter::authority().await?;
    let filter = json!({"project_id": project_id});
    let order = ["share_class", "shares DESC", "id"];
    Ok(holdings(matter::select(&pg, TABLE, filter, &order).await?))
}

/// One Cap Table Holding on a Project, or `None`.
///
/// # Errors
///
/// [`CapTableHoldingError::Db`] if the read fails.
pub async fn find(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<CapTableHolding>, CapTableHoldingError> {
    let pg = matter::authority().await?;
    let filter = json!({"id": id, "project_id": project_id});
    Ok(holdings(matter::select(&pg, TABLE, filter, &[]).await?).pop())
}

/// The row holding one (holder, share class) line, or `None`.
///
/// # Errors
///
/// [`CapTableHoldingError::Db`] if the read fails.
pub async fn find_by_line(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    holder: Party,
    share_class: &str,
) -> Result<Option<CapTableHolding>, CapTableHoldingError> {
    let pg = matter::authority().await?;
    let filter = json!({
        "project_id": project_id,
        "holder_id": matter::tagged(holder.kind.table(), holder.id),
        "share_class": share_class.trim(),
    });
    Ok(holdings(matter::select(&pg, TABLE, filter, &[]).await?).pop())
}

/// Store a new Cap Table Holding and return it.
///
/// # Errors
///
/// [`CapTableHoldingError::Invalid`] for a blank share class, negative
/// shares, a missing Project, a holder with no row behind it, or a line already held; [`CapTableHoldingError::Db`] if
/// the write fails.
pub async fn insert(
    application_store: &ApplicationStore,
    project_id: Uuid,
    fields: &CapTableHoldingFields,
) -> Result<CapTableHolding, CapTableHoldingError> {
    let fields = validate(application_store, project_id, fields).await?;
    let inserted_at = chrono::Utc::now().to_rfc3339();
    persist(
        application_store,
        Uuid::now_v7(),
        project_id,
        fields,
        &inserted_at,
    )
    .await
}

/// Replace a Cap Table Holding's fields and return the stored row.
///
/// # Errors
///
/// [`CapTableHoldingError::NotFound`] when the row is not on this Project;
/// otherwise as [`insert`].
pub async fn update(
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
    fields: &CapTableHoldingFields,
) -> Result<CapTableHolding, CapTableHoldingError> {
    let existing = find(application_store, project_id, id)
        .await?
        .ok_or(CapTableHoldingError::NotFound)?;
    let fields = validate(application_store, project_id, fields).await?;
    persist(
        application_store,
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
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<(), CapTableHoldingError> {
    find(application_store, project_id, id)
        .await?
        .ok_or(CapTableHoldingError::NotFound)?;
    let pg = matter::authority().await?;
    matter::delete(&pg, TABLE, json!({"id": id})).await?;
    Ok(())
}

/// Remove every Cap Table Holding on a Project. The rows are imported from
/// the Project repository's seeds, so they go with the Project row.
///
/// # Errors
///
/// [`CapTableHoldingError::Db`] if the delete fails.
pub async fn delete_for_project(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<(), CapTableHoldingError> {
    let pg = matter::authority().await?;
    matter::delete(&pg, TABLE, json!({"project_id": project_id})).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! with_postgres {
        ($pg:ident, $application_store:ident, $body:block) => {{
            let Some($pg) = crate::postgres::matter::tests::database().await else {
                return;
            };
            let $application_store = ApplicationStore::uninitialized();
            crate::postgres::matter::with_test_db($pg.clone(), async $body).await;
            $pg.0.close().await;
        }};
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
        with_postgres!(pg, application_store, {
            let project_id =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-cap-table").await;
            let founder =
                crate::postgres::matter::tests::postgres_person(&pg, "founder@example.com").await;
            let fund = crate::postgres::matter::tests::postgres_entity(&pg).await;

            let common = insert(
                &application_store,
                project_id,
                &holding(founder, " Common ", 6_000_000),
            )
            .await
            .unwrap();
            assert_eq!(common.share_class, "Common");
            let preferred = insert(
                &application_store,
                project_id,
                &CapTableHoldingFields {
                    holder: Party::entity(fund),
                    ..holding(founder, "Series Seed Preferred", 1_500_000)
                },
            )
            .await
            .unwrap();
            let employee =
                crate::postgres::matter::tests::postgres_person(&pg, "employee@example.com").await;
            let options = insert(
                &application_store,
                project_id,
                &CapTableHoldingFields {
                    vesting_note: Some("Four years, one-year cliff".into()),
                    ..holding(employee, "Common", 250_000)
                },
            )
            .await
            .unwrap();

            let table: Vec<Uuid> = list(&application_store, project_id)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.id)
                .collect();
            assert_eq!(table, vec![common.id, options.id, preferred.id]);

            let updated = update(
                &application_store,
                project_id,
                common.id,
                &holding(founder, "Common", 5_750_000),
            )
            .await
            .unwrap();
            assert_eq!(updated.shares, 5_750_000);
            assert_eq!(updated.inserted_at, common.inserted_at);

            delete(&application_store, project_id, options.id)
                .await
                .unwrap();
            assert_eq!(list(&application_store, project_id).await.unwrap().len(), 2);
            assert!(matches!(
                delete(&application_store, project_id, options.id).await,
                Err(CapTableHoldingError::NotFound)
            ));
        });
    }

    #[tokio::test]
    async fn a_holder_with_no_row_behind_it_is_refused() {
        with_postgres!(pg, application_store, {
            let project_id =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-cap-holder").await;
            for holder in [Party::person(Uuid::now_v7()), Party::entity(Uuid::now_v7())] {
                let error = insert(
                    &application_store,
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
            assert!(list(&application_store, project_id)
                .await
                .unwrap()
                .is_empty());
        });
    }

    #[tokio::test]
    async fn negative_shares_or_a_blank_class_is_refused() {
        with_postgres!(pg, application_store, {
            let project_id =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-cap-refusals").await;
            let person_id =
                crate::postgres::matter::tests::postgres_person(&pg, "refused@example.com").await;
            for bad in [
                holding(person_id, "Common", -1),
                holding(person_id, "  ", 10),
            ] {
                assert!(matches!(
                    insert(&application_store, project_id, &bad)
                        .await
                        .unwrap_err(),
                    CapTableHoldingError::Invalid(_)
                ));
            }
        });
    }

    #[tokio::test]
    async fn one_row_holds_each_holder_and_class() {
        with_postgres!(pg, application_store, {
            let project_id =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-cap-line").await;
            let person_id =
                crate::postgres::matter::tests::postgres_person(&pg, "line@example.com").await;
            let other =
                crate::postgres::matter::tests::postgres_person(&pg, "line-other@example.com")
                    .await;
            insert(
                &application_store,
                project_id,
                &holding(person_id, "Common", 100),
            )
            .await
            .unwrap();
            insert(
                &application_store,
                project_id,
                &holding(other, "Common", 100),
            )
            .await
            .unwrap();
            let error = insert(
                &application_store,
                project_id,
                &holding(person_id, "Common", 5),
            )
            .await
            .unwrap_err();
            assert!(matches!(error, CapTableHoldingError::Invalid(_)), "{error}");
            let found = find_by_line(
                &application_store,
                project_id,
                Party::person(person_id),
                " Common",
            )
            .await
            .unwrap()
            .expect("the line");
            assert_eq!(found.shares, 100);
        });
    }

    #[tokio::test]
    async fn rows_are_scoped_to_their_project() {
        with_postgres!(pg, application_store, {
            let mine =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-cap-mine").await;
            let other =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-cap-other").await;
            let person_id =
                crate::postgres::matter::tests::postgres_person(&pg, "cap-scoped@example.com")
                    .await;
            let stored = insert(&application_store, mine, &holding(person_id, "Common", 10))
                .await
                .unwrap();
            assert!(list(&application_store, other).await.unwrap().is_empty());
            assert!(find(&application_store, other, stored.id)
                .await
                .unwrap()
                .is_none());
            delete_for_project(&application_store, mine).await.unwrap();
            assert!(list(&application_store, mine).await.unwrap().is_empty());
        });
    }
}

#[cfg(test)]
#[path = "cap_table_holdings/compatibility_tests.rs"]
mod compatibility_tests;
