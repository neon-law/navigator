//! Project-scoped [Fiduciary](../../docs/glossary/fiduciary.md) roles
//! (`fiduciary_role`): who an estate plan names as trustee, executor, agent,
//! or guardian, and in what successor order.
//!
//! The fiduciary is one Person or one Entity. `successor_order`
//! starts at 1 for the first named; one row holds each (role, order) slot on
//! a Project. A row grants no lens on the matter — a fiduciary who reads the
//! matter does so through a `person_delegate`.

use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::matter_record_links::one_of;
use crate::postgres::matter::{self, MatterAuthorityError};
use crate::project_references::Party;
use crate::relationships::Endpoint;
pub use model::fiduciary_roles::ROLES;

const TABLE: &str = "fiduciary_role";
const SLOT_INDEX: &str = "fiduciary_role_order";

/// One stored Fiduciary Role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FiduciaryRole {
    pub id: Uuid,
    pub project_id: Uuid,
    pub role: String,
    pub fiduciary: Party,
    pub successor_order: i64,
    pub notation_id: Option<Uuid>,
    pub inserted_at: String,
    pub updated_at: String,
}

/// The fields a caller writes. Insert and update both take the whole set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiduciaryRoleFields {
    pub role: String,
    pub fiduciary: Party,
    pub successor_order: i64,
    pub notation_id: Option<Uuid>,
}

/// The row as PostgreSQL stores it: the fiduciary is a tagged `JSONB` link.
#[derive(Serialize, serde::Deserialize)]
struct PgRow {
    id: Uuid,
    project_id: Uuid,
    role: String,
    fiduciary_id: serde_json::Value,
    successor_order: i64,
    notation_id: Option<Uuid>,
    inserted_at: String,
    updated_at: String,
}

impl PgRow {
    fn into_role(self) -> Option<FiduciaryRole> {
        let (table, id) = matter::untagged(&self.fiduciary_id)?;
        Some(FiduciaryRole {
            id: self.id,
            project_id: self.project_id,
            role: self.role,
            fiduciary: Party {
                kind: Endpoint::from_table(table)?,
                id,
            },
            successor_order: self.successor_order,
            notation_id: self.notation_id,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

fn roles(rows: Vec<PgRow>) -> Vec<FiduciaryRole> {
    rows.into_iter().filter_map(PgRow::into_role).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum FiduciaryRoleError {
    #[error(transparent)]
    Postgres(#[from] MatterAuthorityError),
    #[error("{0}")]
    Invalid(String),
    #[error("no such fiduciary role")]
    NotFound,
    #[error("writing a fiduciary role returned no usable row")]
    WriteReturnedNothing,
}

async fn validate(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    fields: &FiduciaryRoleFields,
) -> Result<String, FiduciaryRoleError> {
    let role = one_of("role", &fields.role, &ROLES).map_err(FiduciaryRoleError::Invalid)?;
    if fields.successor_order < 1 {
        return Err(FiduciaryRoleError::Invalid(
            "successor_order starts at 1".into(),
        ));
    }
    {
        let pg = matter::authority().await?;
        crate::matter_record_links::require_project_on(&pg, project_id)
            .await
            .map_err(FiduciaryRoleError::Invalid)?;
        if let Some(notation_id) = fields.notation_id {
            crate::matter_record_links::require_project_notation_on(&pg, project_id, notation_id)
                .await
                .map_err(FiduciaryRoleError::Invalid)?;
        }
    }
    Ok(role)
}

async fn persist(
    _application_store: &ApplicationStore,
    id: Uuid,
    project_id: Uuid,
    role: String,
    fields: &FiduciaryRoleFields,
    inserted_at: &str,
) -> Result<FiduciaryRole, FiduciaryRoleError> {
    let now = chrono::Utc::now().to_rfc3339();
    let pg = matter::authority().await?;
    let row = PgRow {
        id,
        project_id,
        role,
        fiduciary_id: matter::tagged(fields.fiduciary.kind.table(), fields.fiduciary.id),
        successor_order: fields.successor_order,
        notation_id: fields.notation_id,
        inserted_at: inserted_at.to_owned(),
        updated_at: now,
    };
    let mut transaction = pg
        .pool()
        .begin()
        .await
        .map_err(MatterAuthorityError::from)?;
    if !crate::matter_record_links::lock_party_on(&mut transaction, fields.fiduciary).await? {
        let noun = match fields.fiduciary.kind {
            Endpoint::Person => "Person",
            Endpoint::Entity => "Entity",
        };
        return Err(FiduciaryRoleError::Invalid(format!(
            "the fiduciary names no such {noun}"
        )));
    }
    let stored: PgRow = match matter::upsert_on(
        &mut *transaction,
        TABLE,
        serde_json::to_value(row).map_err(MatterAuthorityError::from)?,
    )
    .await
    {
        Err(MatterAuthorityError::Conflict(index)) if index == SLOT_INDEX => {
            return Err(slot_taken(fields));
        }
        stored => stored?,
    };
    transaction
        .commit()
        .await
        .map_err(MatterAuthorityError::from)?;
    stored
        .into_role()
        .ok_or(FiduciaryRoleError::WriteReturnedNothing)
}

fn slot_taken(fields: &FiduciaryRoleFields) -> FiduciaryRoleError {
    FiduciaryRoleError::Invalid(format!(
        "this Project already names a {} at successor order {}",
        fields.role.trim(),
        fields.successor_order
    ))
}

fn roster_rank(role: &FiduciaryRole) -> (usize, i64) {
    let rank = ROLES
        .iter()
        .position(|known| *known == role.role)
        .unwrap_or(ROLES.len());
    (rank, role.successor_order)
}

/// A Project's Fiduciary Roles, in roster order: role, then successor order.
///
/// # Errors
///
/// [`FiduciaryRoleError::Db`] if the read fails.
pub async fn list(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<FiduciaryRole>, FiduciaryRoleError> {
    let mut list = {
        let pg = matter::authority().await?;
        let filter = json!({"project_id": project_id});
        roles(matter::select(&pg, TABLE, filter, &[]).await?)
    };
    list.sort_by_key(roster_rank);
    Ok(list)
}

/// One Fiduciary Role on a Project, or `None`.
///
/// # Errors
///
/// [`FiduciaryRoleError::Db`] if the read fails.
pub async fn find(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<FiduciaryRole>, FiduciaryRoleError> {
    let pg = matter::authority().await?;
    let filter = json!({"id": id, "project_id": project_id});
    Ok(roles(matter::select(&pg, TABLE, filter, &[]).await?).pop())
}

/// The row holding one (role, successor order) slot on a Project, or `None`.
///
/// # Errors
///
/// [`FiduciaryRoleError::Db`] if the read fails.
pub async fn find_by_slot(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    role: &str,
    successor_order: i64,
) -> Result<Option<FiduciaryRole>, FiduciaryRoleError> {
    let pg = matter::authority().await?;
    let filter = json!({
        "project_id": project_id,
        "role": role.trim(),
        "successor_order": successor_order,
    });
    Ok(roles(matter::select(&pg, TABLE, filter, &[]).await?).pop())
}

/// Store a new Fiduciary Role and return it.
///
/// # Errors
///
/// [`FiduciaryRoleError::Invalid`] for a role outside [`ROLES`], an order
/// below 1, a missing Project, a fiduciary with no row behind it, a Notation from another Project, or a slot already
/// held; [`FiduciaryRoleError::Db`] if the write fails.
pub async fn insert(
    application_store: &ApplicationStore,
    project_id: Uuid,
    fields: &FiduciaryRoleFields,
) -> Result<FiduciaryRole, FiduciaryRoleError> {
    let role = validate(application_store, project_id, fields).await?;
    let inserted_at = chrono::Utc::now().to_rfc3339();
    persist(
        application_store,
        Uuid::now_v7(),
        project_id,
        role,
        fields,
        &inserted_at,
    )
    .await
}

/// Replace a Fiduciary Role's fields and return the stored row.
///
/// # Errors
///
/// [`FiduciaryRoleError::NotFound`] when the row is not on this Project;
/// otherwise as [`insert`].
pub async fn update(
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
    fields: &FiduciaryRoleFields,
) -> Result<FiduciaryRole, FiduciaryRoleError> {
    let existing = find(application_store, project_id, id)
        .await?
        .ok_or(FiduciaryRoleError::NotFound)?;
    let role = validate(application_store, project_id, fields).await?;
    persist(
        application_store,
        existing.id,
        project_id,
        role,
        fields,
        &existing.inserted_at,
    )
    .await
}

/// Remove one Fiduciary Role.
///
/// # Errors
///
/// [`FiduciaryRoleError::NotFound`] when the row is not on this Project;
/// [`FiduciaryRoleError::Db`] if the delete fails.
pub async fn delete(
    application_store: &ApplicationStore,
    project_id: Uuid,
    id: Uuid,
) -> Result<(), FiduciaryRoleError> {
    find(application_store, project_id, id)
        .await?
        .ok_or(FiduciaryRoleError::NotFound)?;
    let pg = matter::authority().await?;
    matter::delete(&pg, TABLE, json!({"id": id})).await?;
    Ok(())
}

/// Remove every Fiduciary Role on a Project. The rows are imported from the
/// Project repository's seeds, so they go with the Project row.
///
/// # Errors
///
/// [`FiduciaryRoleError::Db`] if the delete fails.
pub async fn delete_for_project(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<(), FiduciaryRoleError> {
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

    fn fields(role: &str, order: i64, fiduciary: Uuid) -> FiduciaryRoleFields {
        FiduciaryRoleFields {
            role: role.into(),
            fiduciary: Party::person(fiduciary),
            successor_order: order,
            notation_id: None,
        }
    }

    #[tokio::test]
    async fn insert_list_update_and_delete_round_trip() {
        with_postgres!(pg, application_store, {
            let project_id =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-fiduciaries").await;
            let first =
                crate::postgres::matter::tests::postgres_person(&pg, "first-trustee@example.com")
                    .await;
            let second =
                crate::postgres::matter::tests::postgres_person(&pg, "second-trustee@example.com")
                    .await;

            let executor = insert(
                &application_store,
                project_id,
                &fields("executor", 1, first),
            )
            .await
            .unwrap();
            let trustee = insert(&application_store, project_id, &fields("trustee", 1, first))
                .await
                .unwrap();
            let successor = insert(
                &application_store,
                project_id,
                &fields("successor_trustee", 1, second),
            )
            .await
            .unwrap();

            let roster: Vec<Uuid> = list(&application_store, project_id)
                .await
                .unwrap()
                .into_iter()
                .map(|role| role.id)
                .collect();
            assert_eq!(roster, vec![trustee.id, successor.id, executor.id]);

            let updated = update(
                &application_store,
                project_id,
                executor.id,
                &fields("executor", 1, second),
            )
            .await
            .unwrap();
            assert_eq!(updated.fiduciary, Party::person(second));
            assert_eq!(updated.inserted_at, executor.inserted_at);

            delete(&application_store, project_id, executor.id)
                .await
                .unwrap();
            assert_eq!(list(&application_store, project_id).await.unwrap().len(), 2);
            assert!(matches!(
                delete(&application_store, project_id, executor.id).await,
                Err(FiduciaryRoleError::NotFound)
            ));
        });
    }

    #[tokio::test]
    async fn an_entity_may_hold_the_role() {
        with_postgres!(pg, application_store, {
            let project_id =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-corporate-trustee")
                    .await;
            let trust_company = crate::postgres::matter::tests::postgres_entity(&pg).await;
            let stored = insert(
                &application_store,
                project_id,
                &FiduciaryRoleFields {
                    fiduciary: Party::entity(trust_company),
                    ..fields("trustee", 1, Uuid::now_v7())
                },
            )
            .await
            .unwrap();
            assert_eq!(stored.fiduciary, Party::entity(trust_company));
        });
    }

    #[tokio::test]
    async fn a_fiduciary_with_no_row_behind_it_is_refused() {
        with_postgres!(pg, application_store, {
            let project_id =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-one-party").await;
            for fiduciary in [Party::person(Uuid::now_v7()), Party::entity(Uuid::now_v7())] {
                let error = insert(
                    &application_store,
                    project_id,
                    &FiduciaryRoleFields {
                        fiduciary,
                        ..fields("trustee", 1, Uuid::now_v7())
                    },
                )
                .await
                .unwrap_err();
                assert!(matches!(error, FiduciaryRoleError::Invalid(_)), "{error}");
            }
            assert!(list(&application_store, project_id)
                .await
                .unwrap()
                .is_empty());
        });
    }

    #[tokio::test]
    async fn an_unknown_role_or_order_below_one_is_refused() {
        with_postgres!(pg, application_store, {
            let project_id =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-bad-role").await;
            let person_id =
                crate::postgres::matter::tests::postgres_person(&pg, "bad-role@example.com").await;
            for bad in [
                fields("beneficiary", 1, person_id),
                fields("trustee", 0, person_id),
            ] {
                assert!(matches!(
                    insert(&application_store, project_id, &bad)
                        .await
                        .unwrap_err(),
                    FiduciaryRoleError::Invalid(_)
                ));
            }
        });
    }

    #[tokio::test]
    async fn an_unknown_instrument_notation_is_refused() {
        with_postgres!(pg, application_store, {
            let project_id =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-no-notation").await;
            let person_id =
                crate::postgres::matter::tests::postgres_person(&pg, "no-notation@example.com")
                    .await;
            let error = insert(
                &application_store,
                project_id,
                &FiduciaryRoleFields {
                    notation_id: Some(Uuid::now_v7()),
                    ..fields("agent_poa", 1, person_id)
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(error, FiduciaryRoleError::Invalid(_)), "{error}");
        });
    }

    #[tokio::test]
    async fn one_row_holds_each_slot() {
        with_postgres!(pg, application_store, {
            let project_id =
                crate::postgres::matter::tests::postgres_project(&pg, "sample-slot").await;
            let person_id =
                crate::postgres::matter::tests::postgres_person(&pg, "slot@example.com").await;
            insert(
                &application_store,
                project_id,
                &fields("guardian", 1, person_id),
            )
            .await
            .unwrap();
            let error = insert(
                &application_store,
                project_id,
                &fields("guardian", 1, person_id),
            )
            .await
            .unwrap_err();
            assert!(matches!(error, FiduciaryRoleError::Invalid(_)), "{error}");
            assert!(find_by_slot(&application_store, project_id, "guardian", 1)
                .await
                .unwrap()
                .is_some());
        });
    }

    #[tokio::test]
    async fn rows_are_scoped_to_their_project() {
        with_postgres!(pg, application_store, {
            let mine = crate::postgres::matter::tests::postgres_project(&pg, "sample-mine").await;
            let other = crate::postgres::matter::tests::postgres_project(&pg, "sample-other").await;
            let person_id =
                crate::postgres::matter::tests::postgres_person(&pg, "scoped@example.com").await;
            let stored = insert(&application_store, mine, &fields("executor", 1, person_id))
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
#[path = "fiduciary_roles/compatibility_tests.rs"]
mod compatibility_tests;
