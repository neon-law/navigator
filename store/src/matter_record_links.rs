//! The link checks the estate and cap-table rows share
//! ([`crate::fiduciary_roles`], [`crate::estate_assets`],
//! [`crate::cap_table_holdings`]).
//!
//! A record link constrains the target table but does not prove its row
//! exists, so each write reads the Project, the Person or Entity it names,
//! and any instrument Notation before it stores the row.

use uuid::Uuid;

use crate::project_references::Party;
use crate::relationships::Endpoint;
pub use model::matter_record_links::one_of;
pub use model::matter_record_links::LinkRefusal;

/// The Project exists.
pub(crate) async fn require_project(
    surreal: &crate::surreal::SurrealDb,
    project_id: Uuid,
) -> Result<(), LinkRefusal> {
    match crate::projects::find_by_id(surreal, project_id).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err("no such Project".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// The Project exists in PostgreSQL.
pub(crate) async fn require_project_on(
    pg: &crate::postgres::PostgresDb,
    project_id: Uuid,
) -> Result<(), LinkRefusal> {
    let found: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM public.project WHERE id=$1)")
        .bind(project_id)
        .fetch_one(pg.pool())
        .await
        .map_err(|error| LinkRefusal::from(error.to_string()))?;
    if found {
        Ok(())
    } else {
        Err("no such Project".into())
    }
}

/// The Person or Entity `party` names exists. `noun` names the link in the
/// refusal ("fiduciary", "holder").
pub(crate) async fn require_party(
    surreal: &crate::surreal::SurrealDb,
    noun: &str,
    party: Party,
) -> Result<(), LinkRefusal> {
    let found = match party.kind {
        Endpoint::Person => crate::persons::find_by_id(surreal, party.id)
            .await
            .map(|row| row.is_some())
            .map_err(|error| error.to_string())?,
        Endpoint::Entity => crate::entities::find_by_id(surreal, party.id)
            .await
            .map(|row| row.is_some())
            .map_err(|error| error.to_string())?,
    };
    if found {
        Ok(())
    } else {
        let table = match party.kind {
            Endpoint::Person => "Person",
            Endpoint::Entity => "Entity",
        };
        Err(format!("the {noun} names no such {table}"))
    }
}

/// Keep a native party alive until its caller's record transaction finishes.
/// Recheck here because public validation can precede a concurrent Person merge.
pub(crate) async fn lock_party_on(
    connection: &mut sqlx::PgConnection,
    party: Party,
) -> Result<bool, crate::postgres::matter::MatterAuthorityError> {
    let sql = match party.kind {
        Endpoint::Person => "SELECT id FROM public.person WHERE id=$1 FOR KEY SHARE",
        Endpoint::Entity => "SELECT id FROM public.entity WHERE id=$1 FOR KEY SHARE",
    };
    let found: Option<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(party.id)
        .fetch_optional(connection)
        .await?;
    Ok(found.is_some())
}

/// The union record link a [`Party`] is stored as.
pub(crate) fn party_record(party: Party) -> surrealdb::types::RecordId {
    crate::surreal::record_id(party.kind.table(), party.id)
}

/// The [`Party`] a stored `record<person|entity>` link names, or `None` for
/// a link this module cannot read.
pub(crate) fn party_from_record(record: &surrealdb::types::RecordId) -> Option<Party> {
    Some(Party {
        kind: Endpoint::from_table(record.table.as_str())?,
        id: crate::surreal::record_uuid(record)?,
    })
}

/// The Notation exists and belongs to this Project.
pub(crate) async fn require_project_notation(
    surreal: &crate::surreal::SurrealDb,
    project_id: Uuid,
    notation_id: Uuid,
) -> Result<(), LinkRefusal> {
    match crate::notations::find_by_id(surreal, notation_id).await {
        Ok(Some(notation)) if notation.project_id == project_id => Ok(()),
        Ok(Some(_)) => Err("the instrument Notation is not on this Project".into()),
        Ok(None) => Err("no such instrument Notation".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// The Notation exists in PostgreSQL and belongs to this Project.
pub(crate) async fn require_project_notation_on(
    pg: &crate::postgres::PostgresDb,
    project_id: Uuid,
    notation_id: Uuid,
) -> Result<(), LinkRefusal> {
    let notation_project: Option<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM public.notation WHERE id=$1")
            .bind(notation_id)
            .fetch_optional(pg.pool())
            .await
            .map_err(|error| LinkRefusal::from(error.to_string()))?;
    match notation_project {
        Some(owner) if owner == project_id => Ok(()),
        Some(_) => Err("the instrument Notation is not on this Project".into()),
        None => Err("no such instrument Notation".into()),
    }
}

/// A trimmed required text field, or the refusal naming it.
pub(crate) fn required_text(name: &str, value: &str) -> Result<String, LinkRefusal> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{name} must not be blank"))
    } else {
        Ok(trimmed.to_string())
    }
}

/// A trimmed optional text field; blank reads as none.
pub(crate) fn optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
