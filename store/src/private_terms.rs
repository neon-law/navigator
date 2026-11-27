//! Private-terms reconciliation — the four record-integrity checks the
//! Pre-Filing PII Check refuses on, run in report mode.
//!
//! `private_terms_for_project` (the portal's `…/private-terms` door) refuses a
//! matter's private-terms response when a participation, a project
//! reference, or the client Entity names a record that is gone, or Entity
//! identifier data that is malformed. That refusal is fail-closed by design —
//! an incomplete identifier set must never scan clean — and it is also
//! first-only: it answers one matter at a time, so it names neither the
//! matter nor the id. An orphan arrives out of band (`delete_person` refuses
//! while a participation exists), so a deployment that has one learns of it
//! from a refused check and nothing else.
//!
//! This module is the single source of truth for those checks, read in
//! report-don't-refuse mode: [`resolve`] walks the same records through the
//! same accessors and returns *every* finding instead of the first refusal.
//! The portal's private-terms door maps a non-empty finding list back onto
//! its typed 409/422 refusals, and the admin-tier reconciliation door
//! renders the same findings as per-matter report rows — two behaviors, one
//! classification, so the two callers cannot drift.

use std::collections::BTreeSet;

use serde::Serialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::entities;
use crate::persons;
use crate::project_references;
use crate::projects;
use crate::relationships::Endpoint;

/// Why a matter's private-terms response would be incomplete: one dangling or
/// malformed record, named for the operator who has to repair it.
///
/// A dangling UUID is the same value class the terms list treats as private;
/// findings are for an operator who already reads the deployment whole, and
/// carry no name, mailbox, phone, or EIN.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finding {
    /// A participation or referenced-person id with no Person record behind
    /// it. Repair by restoring the Person, or removing the attachment.
    MissingPerson {
        id: Uuid,
        attached_as: PersonAttachment,
    },
    /// The matter's client Entity, or a referenced-entity id, with no Entity
    /// record behind it. Repair by restoring the Entity, or removing the
    /// attachment.
    MissingEntity {
        id: Uuid,
        attached_as: EntityAttachment,
    },
    /// An Entity's stored `metadata` is not a JSON object.
    InvalidEntityMetadata { id: Uuid, field: EntityField },
    /// An Entity's `metadata.ein` is present but not `NN-NNNNNNN`.
    InvalidEntityEin { id: Uuid, field: EntityField },
}

/// Where a dangling Person id hangs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PersonAttachment {
    /// A participation row on the matter.
    #[serde(rename = "participation")]
    Participation,
    /// A `project_reference` edge naming the person.
    #[serde(rename = "reference:person")]
    Reference,
}

/// Where a dangling Entity id hangs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EntityAttachment {
    /// The matter's own client Entity (`project.entity_id`).
    #[serde(rename = "project.client")]
    ProjectClient,
    /// A `project_reference` edge naming the entity.
    #[serde(rename = "reference:entity")]
    Reference,
}

/// The malformed Entity field a finding names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EntityField {
    /// The `metadata` column itself — stored, but not a JSON object
    /// (`DEFINE FIELD … metadata ON entity TYPE any` admits the shape).
    #[serde(rename = "metadata")]
    Metadata,
    /// The `metadata.ein` value — present, but not `NN-NNNNNNN`.
    #[serde(rename = "metadata.ein")]
    MetadataEin,
}

/// Reading the records a matter's private terms reach.
#[derive(Debug, thiserror::Error)]
pub enum PrivateTermsError {
    #[error(transparent)]
    Project(#[from] projects::ProjectStoreError),
    #[error(transparent)]
    Person(#[from] persons::PersonError),
    #[error(transparent)]
    Entity(#[from] entities::EntityError),
    #[error(transparent)]
    Reference(#[from] project_references::ProjectReferenceError),
}

/// What one matter's reconciliation read: the findings, and the records a
/// clean resolution builds the terms from.
///
/// A non-empty [`Resolution::findings`] means the private-terms response for
/// this matter is refused; the records stay here so the refusing caller
/// reads exactly what the report read — the two callers share one read.
#[derive(Debug)]
pub struct Resolution {
    /// Every dangling or malformed record, in the order the refusal reads
    /// them: dangling people, then dangling entities, then malformed Entity
    /// identifier data per entity in id order.
    pub findings: Vec<Finding>,
    /// The Person records behind the matter's participations and references.
    pub people: Vec<persons::Person>,
    /// The Entity records behind the client and the references, in id order.
    pub entities: Vec<entities::Entity>,
    /// The people holding a participation on the matter.
    pub participation_ids: BTreeSet<Uuid>,
}

/// Reconcile one matter's private-terms records, reporting findings instead
/// of refusing. Reads the participations, the project references, and the
/// client and referenced people and entities, and reports every dangling or
/// malformed record — not just the first one the refusal would meet.
///
/// # Errors
///
/// [`PrivateTermsError`] when the underlying reads fail.
pub async fn resolve(
    db: &ApplicationStore,
    project: &projects::Project,
) -> Result<Resolution, PrivateTermsError> {
    let participation_ids: BTreeSet<Uuid> = projects::participations_for_project(db, project.id)
        .await?
        .into_iter()
        .map(|row| row.person_id)
        .collect();
    let mut reference_people = BTreeSet::new();
    let mut reference_entities = BTreeSet::new();
    for reference in project_references::for_project(db, project.id).await? {
        match reference.party.kind {
            Endpoint::Person => reference_people.insert(reference.party.id),
            Endpoint::Entity => reference_entities.insert(reference.party.id),
        };
    }
    let person_ids: BTreeSet<Uuid> = participation_ids
        .union(&reference_people)
        .copied()
        .collect();
    let mut entity_ids = reference_entities.clone();
    entity_ids.insert(project.entity_id);

    let people = persons::find_by_ids(db, &person_ids.iter().copied().collect::<Vec<_>>()).await?;
    let mut resolved_entities =
        entities::find_by_ids(db, &entity_ids.iter().copied().collect::<Vec<_>>()).await?;
    resolved_entities.sort_unstable_by_key(|entity| entity.id);

    let mut findings = Vec::new();
    let found_people: BTreeSet<Uuid> = people.iter().map(|person| person.id).collect();
    for id in person_ids.difference(&found_people) {
        if participation_ids.contains(id) {
            findings.push(Finding::MissingPerson {
                id: *id,
                attached_as: PersonAttachment::Participation,
            });
        }
        if reference_people.contains(id) {
            findings.push(Finding::MissingPerson {
                id: *id,
                attached_as: PersonAttachment::Reference,
            });
        }
    }
    let found_entities: BTreeSet<Uuid> = resolved_entities.iter().map(|entity| entity.id).collect();
    for id in entity_ids.difference(&found_entities) {
        if *id == project.entity_id {
            findings.push(Finding::MissingEntity {
                id: *id,
                attached_as: EntityAttachment::ProjectClient,
            });
        }
        if reference_entities.contains(id) {
            findings.push(Finding::MissingEntity {
                id: *id,
                attached_as: EntityAttachment::Reference,
            });
        }
    }
    for entity in &resolved_entities {
        if let Some(metadata) = entity.metadata.as_ref() {
            let object = metadata.as_object();
            if object.is_none() {
                findings.push(Finding::InvalidEntityMetadata {
                    id: entity.id,
                    field: EntityField::Metadata,
                });
                continue;
            }
            if let Some(value) = object.and_then(|object| object.get("ein")) {
                if !is_standardized_ein(value) {
                    findings.push(Finding::InvalidEntityEin {
                        id: entity.id,
                        field: EntityField::MetadataEin,
                    });
                }
            }
        }
    }

    Ok(Resolution {
        findings,
        people,
        entities: resolved_entities,
        participation_ids,
    })
}

/// `NN-NNNNNNN` — the standardized EIN shape the identifier list carries.
fn is_standardized_ein(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|ein| {
        ein.len() == 10
            && ein.bytes().enumerate().all(|(index, byte)| {
                if index == 2 {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit()
                }
            })
    })
}

#[cfg(test)]
mod tests {
    use super::{EntityField, Finding, PersonAttachment};

    use uuid::Uuid;

    #[tokio::test]
    async fn findings_serialize_the_report_payload_shape() {
        let finding = Finding::MissingPerson {
            id: Uuid::now_v7(),
            attached_as: PersonAttachment::Reference,
        };
        let rendered = serde_json::to_value(&finding).unwrap();
        assert_eq!(rendered["kind"], "missing_person");
        assert_eq!(rendered["attached_as"], "reference:person");
        assert!(rendered.get("id").is_some());

        let finding = Finding::MissingEntity {
            id: Uuid::now_v7(),
            attached_as: super::EntityAttachment::ProjectClient,
        };
        let rendered = serde_json::to_value(&finding).unwrap();
        assert_eq!(rendered["kind"], "missing_entity");
        assert_eq!(rendered["attached_as"], "project.client");

        let finding = Finding::InvalidEntityMetadata {
            id: Uuid::now_v7(),
            field: EntityField::Metadata,
        };
        let rendered = serde_json::to_value(&finding).unwrap();
        assert_eq!(rendered["kind"], "invalid_entity_metadata");
        assert_eq!(rendered["field"], "metadata");

        let finding = Finding::InvalidEntityEin {
            id: Uuid::now_v7(),
            field: EntityField::MetadataEin,
        };
        let rendered = serde_json::to_value(&finding).unwrap();
        assert_eq!(rendered["kind"], "invalid_entity_ein");
        assert_eq!(rendered["field"], "metadata.ein");
    }
}
