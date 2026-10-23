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

/// Why a link check refused a write. Each module folds this into its own
/// `Invalid` error so the caller reads one plain sentence.
pub(crate) type LinkRefusal = String;

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

/// `value` is one of `allowed`, or the refusal naming the field.
pub(crate) fn one_of(name: &str, value: &str, allowed: &[&str]) -> Result<String, LinkRefusal> {
    let value = value.trim();
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(format!("{name} must be one of {}", allowed.join(", ")))
    }
}
