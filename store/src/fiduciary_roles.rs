//! Project-scoped [Fiduciary](../../docs/glossary/fiduciary.md) roles
//! (`fiduciary_role`): who an estate plan names as trustee, executor, agent,
//! or guardian, and in what successor order.
//!
//! The fiduciary is one Person or one Entity. `successor_order`
//! starts at 1 for the first named; one row holds each (role, order) slot on
//! a Project. A row grants no lens on the matter — a fiduciary who reads the
//! matter does so through a `person_delegate`.

use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::matter_record_links::{
    one_of, party_from_record, party_record, require_party, require_project,
    require_project_notation,
};
use crate::project_references::Party;
use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

const TABLE: &str = "fiduciary_role";
const SLOT_INDEX: &str = "fiduciary_role_order";

/// Every role the schema admits, in the order a roster lists them. The
/// `ASSERT` on `fiduciary_role.role` is the engine's copy of this list.
pub const ROLES: [&str; 7] = [
    "trustee",
    "successor_trustee",
    "executor",
    "alternate_executor",
    "agent_poa",
    "health_care_agent",
    "guardian",
];

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

#[derive(SurrealValue)]
struct FiduciaryRoleRow {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    role: String,
    fiduciary_id: surrealdb::types::RecordId,
    successor_order: i64,
    notation_id: Option<surrealdb::types::RecordId>,
    inserted_at: String,
    updated_at: String,
}

impl FiduciaryRoleRow {
    fn into_role(self) -> Option<FiduciaryRole> {
        Some(FiduciaryRole {
            id: record_uuid(&self.id)?,
            project_id: record_uuid(&self.project_id)?,
            role: self.role,
            fiduciary: party_from_record(&self.fiduciary_id)?,
            successor_order: self.successor_order,
            notation_id: self.notation_id.as_ref().and_then(record_uuid),
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

const SELECT: &str = "id, project_id, role, fiduciary_id, successor_order, notation_id, \
     inserted_at, updated_at";

#[derive(Debug, thiserror::Error)]
pub enum FiduciaryRoleError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("no such fiduciary role")]
    NotFound,
    #[error("writing a fiduciary role returned no usable row")]
    WriteReturnedNothing,
}

async fn validate(
    surreal: &SurrealDb,
    project_id: Uuid,
    fields: &FiduciaryRoleFields,
) -> Result<String, FiduciaryRoleError> {
    let role = one_of("role", &fields.role, &ROLES).map_err(FiduciaryRoleError::Invalid)?;
    if fields.successor_order < 1 {
        return Err(FiduciaryRoleError::Invalid(
            "successor_order starts at 1".into(),
        ));
    }
    require_project(surreal, project_id)
        .await
        .map_err(FiduciaryRoleError::Invalid)?;
    require_party(surreal, "fiduciary", fields.fiduciary)
        .await
        .map_err(FiduciaryRoleError::Invalid)?;
    if let Some(notation_id) = fields.notation_id {
        require_project_notation(surreal, project_id, notation_id)
            .await
            .map_err(FiduciaryRoleError::Invalid)?;
    }
    Ok(role)
}

async fn persist(
    surreal: &SurrealDb,
    id: Uuid,
    project_id: Uuid,
    role: String,
    fields: &FiduciaryRoleFields,
    inserted_at: &str,
) -> Result<FiduciaryRole, FiduciaryRoleError> {
    let now = chrono::Utc::now().to_rfc3339();
    let response = surreal
        .query(format!(
            "UPSERT $id SET project_id = $project_id, role = $role, \
             fiduciary_id = $fiduciary_id, successor_order = $successor_order, \
             notation_id = $notation_id, inserted_at = $inserted_at, updated_at = $now \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("role", role))
        .bind(("fiduciary_id", party_record(fields.fiduciary)))
        .bind(("successor_order", fields.successor_order))
        .bind((
            "notation_id",
            fields.notation_id.map(|id| record_id("notation", id)),
        ))
        .bind(("inserted_at", inserted_at.to_string()))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check);
    let mut response = match response {
        Ok(response) => response,
        Err(error) if retry::unique_violation(&error) == Some(SLOT_INDEX) => {
            return Err(FiduciaryRoleError::Invalid(format!(
                "this Project already names a {} at successor order {}",
                fields.role.trim(),
                fields.successor_order
            )));
        }
        Err(error) => return Err(error.into()),
    };
    let row: Option<FiduciaryRoleRow> = response.take(0)?;
    row.and_then(FiduciaryRoleRow::into_role)
        .ok_or(FiduciaryRoleError::WriteReturnedNothing)
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
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<FiduciaryRole>, FiduciaryRoleError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project_id"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<FiduciaryRoleRow> = response.take(0)?;
    let mut roles: Vec<FiduciaryRole> = rows
        .into_iter()
        .filter_map(FiduciaryRoleRow::into_role)
        .collect();
    roles.sort_by_key(roster_rank);
    Ok(roles)
}

/// One Fiduciary Role on a Project, or `None`.
///
/// # Errors
///
/// [`FiduciaryRoleError::Db`] if the read fails.
pub async fn find(
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<Option<FiduciaryRole>, FiduciaryRoleError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} \
             WHERE id = $id AND project_id = $project_id LIMIT 1"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<FiduciaryRoleRow> = response.take(0)?;
    Ok(row.and_then(FiduciaryRoleRow::into_role))
}

/// The row holding one (role, successor order) slot on a Project, or `None`.
///
/// # Errors
///
/// [`FiduciaryRoleError::Db`] if the read fails.
pub async fn find_by_slot(
    surreal: &SurrealDb,
    project_id: Uuid,
    role: &str,
    successor_order: i64,
) -> Result<Option<FiduciaryRole>, FiduciaryRoleError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} WHERE project_id = $project_id \
             AND role = $role AND successor_order = $successor_order LIMIT 1"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("role", role.trim().to_string()))
        .bind(("successor_order", successor_order))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<FiduciaryRoleRow> = response.take(0)?;
    Ok(row.and_then(FiduciaryRoleRow::into_role))
}

/// Store a new Fiduciary Role and return it.
///
/// # Errors
///
/// [`FiduciaryRoleError::Invalid`] for a role outside [`ROLES`], an order
/// below 1, a missing Project, a fiduciary with no row behind it, a Notation from another Project, or a slot already
/// held; [`FiduciaryRoleError::Db`] if the write fails.
pub async fn insert(
    surreal: &SurrealDb,
    project_id: Uuid,
    fields: &FiduciaryRoleFields,
) -> Result<FiduciaryRole, FiduciaryRoleError> {
    let role = validate(surreal, project_id, fields).await?;
    let inserted_at = chrono::Utc::now().to_rfc3339();
    persist(
        surreal,
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
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
    fields: &FiduciaryRoleFields,
) -> Result<FiduciaryRole, FiduciaryRoleError> {
    let existing = find(surreal, project_id, id)
        .await?
        .ok_or(FiduciaryRoleError::NotFound)?;
    let role = validate(surreal, project_id, fields).await?;
    persist(
        surreal,
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
    surreal: &SurrealDb,
    project_id: Uuid,
    id: Uuid,
) -> Result<(), FiduciaryRoleError> {
    find(surreal, project_id, id)
        .await?
        .ok_or(FiduciaryRoleError::NotFound)?;
    surreal
        .query("DELETE $id")
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    Ok(())
}

/// Remove every Fiduciary Role on a Project. The rows are imported from the
/// Project repository's seeds, so they go with the Project row.
///
/// # Errors
///
/// [`FiduciaryRoleError::Db`] if the delete fails.
pub async fn delete_for_project(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<(), FiduciaryRoleError> {
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
                name: "Sample estate plan".into(),
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
            &NewPerson::with_role("Sample Fiduciary", email, Role::Client),
        )
        .await
        .unwrap()
        .id
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
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-fiduciaries").await;
        let first = person(&surreal, "first-trustee@example.com").await;
        let second = person(&surreal, "second-trustee@example.com").await;

        let executor = insert(&surreal, project_id, &fields("executor", 1, first))
            .await
            .unwrap();
        let trustee = insert(&surreal, project_id, &fields("trustee", 1, first))
            .await
            .unwrap();
        let successor = insert(
            &surreal,
            project_id,
            &fields("successor_trustee", 1, second),
        )
        .await
        .unwrap();

        let roster: Vec<Uuid> = list(&surreal, project_id)
            .await
            .unwrap()
            .into_iter()
            .map(|role| role.id)
            .collect();
        assert_eq!(roster, vec![trustee.id, successor.id, executor.id]);

        let updated = update(
            &surreal,
            project_id,
            executor.id,
            &fields("executor", 1, second),
        )
        .await
        .unwrap();
        assert_eq!(updated.fiduciary, Party::person(second));
        assert_eq!(updated.inserted_at, executor.inserted_at);

        delete(&surreal, project_id, executor.id).await.unwrap();
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 2);
        assert!(matches!(
            delete(&surreal, project_id, executor.id).await,
            Err(FiduciaryRoleError::NotFound)
        ));
    }

    #[tokio::test]
    async fn an_entity_may_hold_the_role() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-corporate-trustee").await;
        let trust_company = seed_entity(&surreal).await;
        let stored = insert(
            &surreal,
            project_id,
            &FiduciaryRoleFields {
                fiduciary: Party::entity(trust_company),
                ..fields("trustee", 1, Uuid::now_v7())
            },
        )
        .await
        .unwrap();
        assert_eq!(stored.fiduciary, Party::entity(trust_company));
    }

    #[tokio::test]
    async fn a_fiduciary_with_no_row_behind_it_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-one-party").await;
        for fiduciary in [Party::person(Uuid::now_v7()), Party::entity(Uuid::now_v7())] {
            let error = insert(
                &surreal,
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
        assert!(list(&surreal, project_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_role_or_order_below_one_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-bad-role").await;
        let person_id = person(&surreal, "bad-role@example.com").await;
        for bad in [
            fields("beneficiary", 1, person_id),
            fields("trustee", 0, person_id),
        ] {
            assert!(matches!(
                insert(&surreal, project_id, &bad).await.unwrap_err(),
                FiduciaryRoleError::Invalid(_)
            ));
        }
    }

    #[tokio::test]
    async fn an_unknown_instrument_notation_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-no-notation").await;
        let person_id = person(&surreal, "no-notation@example.com").await;
        let error = insert(
            &surreal,
            project_id,
            &FiduciaryRoleFields {
                notation_id: Some(Uuid::now_v7()),
                ..fields("agent_poa", 1, person_id)
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, FiduciaryRoleError::Invalid(_)), "{error}");
    }

    #[tokio::test]
    async fn one_row_holds_each_slot() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-slot").await;
        let person_id = person(&surreal, "slot@example.com").await;
        insert(&surreal, project_id, &fields("guardian", 1, person_id))
            .await
            .unwrap();
        let error = insert(&surreal, project_id, &fields("guardian", 1, person_id))
            .await
            .unwrap_err();
        assert!(matches!(error, FiduciaryRoleError::Invalid(_)), "{error}");
        assert!(find_by_slot(&surreal, project_id, "guardian", 1)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn the_engine_refuses_a_role_outside_the_closed_list() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal, "sample-engine-assert").await;
        let result = surreal
            .query(
                "CREATE fiduciary_role SET project_id = $project_id, role = 'beneficiary', \
                 fiduciary_id = $person, successor_order = 1, inserted_at = '', updated_at = ''",
            )
            .bind(("project_id", record_id("project", project_id)))
            .bind((
                "person",
                record_id("person", person(&surreal, "assert@example.com").await),
            ))
            .await
            .and_then(surrealdb::IndexedResults::check);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rows_are_scoped_to_their_project() {
        let surreal = mem_surreal().await;
        let mine = project(&surreal, "sample-mine").await;
        let other = project(&surreal, "sample-other").await;
        let person_id = person(&surreal, "scoped@example.com").await;
        let stored = insert(&surreal, mine, &fields("executor", 1, person_id))
            .await
            .unwrap();
        assert!(list(&surreal, other).await.unwrap().is_empty());
        assert!(find(&surreal, other, stored.id).await.unwrap().is_none());
        delete_for_project(&surreal, mine).await.unwrap();
        assert!(list(&surreal, mine).await.unwrap().is_empty());
    }
}
