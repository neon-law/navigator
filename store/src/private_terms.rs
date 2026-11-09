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

use crate::entities;
use crate::persons;
use crate::project_references;
use crate::relationships::Endpoint;
use crate::projects;
use crate::surreal::SurrealDb;

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
    db: &SurrealDb,
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
    use super::{resolve, EntityField, Finding, PersonAttachment};
    use crate::entities::{self, NewEntity};
    use crate::participation::{AddParticipantCommand, DriActor, DriRequest};
    use crate::persons::{self, NewPerson, Role};
    use crate::project_references::{
        self, Party, REASON_ADVERSE_PARTY, REASON_COUNTERPARTY_ENTITY,
    };
    use crate::projects::{self, NewProject};
    use crate::surreal::test_support::mem;
    use crate::surreal::{record_id, SurrealDb};
    use uuid::Uuid;

    /// One client Entity and a matter opened against it, with the lawyer who
    /// will hold its participation and the person a reference will name.
    struct Matter {
        db: SurrealDb,
        project: projects::Project,
        client: entities::Entity,
        participant: persons::Person,
        referenced: persons::Person,
    }

    async fn matter(code: &str) -> Matter {
        let db = mem().await;
        let client = entities::create(
            &db,
            &NewEntity {
                name: format!("{code} client LLC"),
                entity_type_id: Uuid::now_v7(),
                jurisdiction_id: Uuid::now_v7(),
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap();
        let project = projects::create(
            &db,
            &NewProject {
                code: code.into(),
                name: format!("Matter {code}"),
                status: "open".into(),
                entity_id: client.id,
                ..NewProject::default()
            },
        )
        .await
        .unwrap();
        let participant = persons::create(
            &db,
            &NewPerson::with_role("Paralegal", "paralegal@example.com", Role::Clerk),
        )
        .await
        .unwrap();
        let referenced = persons::create(
            &db,
            &NewPerson::with_role("Adverse Party", "adverse@example.com", Role::Client),
        )
        .await
        .unwrap();
        Matter {
            db,
            project,
            client,
            participant,
            referenced,
        }
    }

    async fn attach(matter: &Matter) {
        crate::participation::add_participant(
            &matter.db,
            &AddParticipantCommand {
                project_id: matter.project.id,
                person_id: matter.participant.id,
                dri: DriRequest::Unchanged,
                actor: DriActor::System,
            },
        )
        .await
        .unwrap();
        project_references::reference(
            &matter.db,
            matter.project.id,
            Party::person(matter.referenced.id),
            REASON_ADVERSE_PARTY,
        )
        .await
        .unwrap();
    }

    async fn delete_person(db: &SurrealDb, id: Uuid) {
        db.query("DELETE $person")
            .bind(("person", record_id("person", id)))
            .await
            .unwrap()
            .check()
            .unwrap();
    }

    async fn set_metadata(db: &SurrealDb, id: Uuid, metadata: serde_json::Value) {
        db.query("UPDATE $entity SET metadata = $metadata")
            .bind(("entity", record_id("entity", id)))
            .bind(("metadata", metadata))
            .await
            .unwrap()
            .check()
            .unwrap();
    }

    #[tokio::test]
    async fn a_healthy_matter_reports_no_findings() {
        let m = matter("healthy-matter").await;
        attach(&m).await;

        let resolution = resolve(&m.db, &m.project).await.unwrap();
        assert!(resolution.findings.is_empty(), "{:?}", resolution.findings);
        // The Person records a clean resolution builds the terms from are
        // here: the participant, and the referenced person.
        let mut resolved_people = resolution
            .people
            .iter()
            .map(|person| person.id)
            .collect::<Vec<_>>();
        resolved_people.sort();
        let mut expected_people = vec![m.participant.id, m.referenced.id];
        expected_people.sort();
        assert_eq!(resolved_people, expected_people);
        assert_eq!(resolution.entities.len(), 1);
        assert_eq!(
            resolution.participation_ids,
            [m.participant.id].into_iter().collect()
        );
    }

    #[tokio::test]
    async fn a_dangling_participation_is_reported_with_its_attachment() {
        let m = matter("dangling-participation").await;
        crate::participation::add_participant(
            &m.db,
            &AddParticipantCommand {
                project_id: m.project.id,
                person_id: m.participant.id,
                dri: DriRequest::Unchanged,
                actor: DriActor::System,
            },
        )
        .await
        .unwrap();
        delete_person(&m.db, m.participant.id).await;

        let resolution = resolve(&m.db, &m.project).await.unwrap();
        assert_eq!(
            resolution.findings,
            vec![Finding::MissingPerson {
                id: m.participant.id,
                attached_as: PersonAttachment::Participation,
            }]
        );
    }

    #[tokio::test]
    async fn a_dangling_reference_is_reported_as_a_referenced_person() {
        let m = matter("dangling-reference").await;
        // A reference edge to a Person that was never minted — the
        // out-of-band drift the resolver exists to name. (Deleting an
        // attached Person would cascade the edge away with the row,
        // leaving nothing for a report to see.)
        let gone = Uuid::now_v7();
        project_references::reference(
            &m.db,
            m.project.id,
            Party::person(gone),
            REASON_ADVERSE_PARTY,
        )
        .await
        .unwrap();

        let resolution = resolve(&m.db, &m.project).await.unwrap();
        assert_eq!(
            resolution.findings,
            vec![Finding::MissingPerson {
                id: gone,
                attached_as: PersonAttachment::Reference,
            }]
        );
    }

    #[tokio::test]
    async fn a_missing_client_entity_is_reported_as_the_project_client() {
        let m = matter("missing-client").await;
        m.db.query("DELETE $entity")
            .bind(("entity", record_id("entity", m.client.id)))
            .await
            .unwrap()
            .check()
            .unwrap();

        let resolution = resolve(&m.db, &m.project).await.unwrap();
        assert_eq!(
            resolution.findings,
            vec![Finding::MissingEntity {
                id: m.client.id,
                attached_as: super::EntityAttachment::ProjectClient,
            }]
        );
    }

    #[tokio::test]
    async fn a_dangling_referenced_entity_is_reported_with_its_attachment() {
        let m = matter("dangling-entity-reference").await;
        let gone = Uuid::now_v7();
        project_references::reference(
            &m.db,
            m.project.id,
            Party::entity(gone),
            REASON_COUNTERPARTY_ENTITY,
        )
        .await
        .unwrap();

        let resolution = resolve(&m.db, &m.project).await.unwrap();
        assert_eq!(
            resolution.findings,
            vec![Finding::MissingEntity {
                id: gone,
                attached_as: super::EntityAttachment::Reference,
            }]
        );
    }

    #[tokio::test]
    async fn metadata_that_is_not_an_object_is_reported_without_an_ein_finding() {
        let m = matter("metadata-shape").await;
        set_metadata(&m.db, m.client.id, serde_json::json!("not an object")).await;

        let resolution = resolve(&m.db, &m.project).await.unwrap();
        assert_eq!(
            resolution.findings,
            vec![Finding::InvalidEntityMetadata {
                id: m.client.id,
                field: EntityField::Metadata,
            }]
        );
    }

    #[tokio::test]
    async fn an_ein_that_is_not_nn_nnnnnnn_is_reported_and_a_standardized_one_is_not() {
        let m = matter("ein-shape").await;
        // The metadata itself is an object in every case here — a
        // non-object metadata is the InvalidEntityMetadata class, covered
        // above — and the `ein` value inside it is what fails the
        // `NN-NNNNNNN` shape: too short, too long, not numeric, or not a
        // string at all.
        for malformed in [
            serde_json::json!({"ein": "12345"}),
            serde_json::json!({"ein": "12-34567890"}),
            serde_json::json!({"ein": "ab-3456789"}),
            serde_json::json!({"ein": 12_345_678_u64}),
        ] {
            set_metadata(&m.db, m.client.id, malformed.clone()).await;
            let resolution = resolve(&m.db, &m.project).await.unwrap();
            assert_eq!(
                resolution.findings,
                vec![Finding::InvalidEntityEin {
                    id: m.client.id,
                    field: EntityField::MetadataEin,
                }],
                "{malformed}"
            );
        }

        set_metadata(&m.db, m.client.id, serde_json::json!({"ein": "12-3456789"})).await;
        let resolution = resolve(&m.db, &m.project).await.unwrap();
        assert!(resolution.findings.is_empty(), "{:?}", resolution.findings);
    }

    #[tokio::test]
    async fn a_person_hanging_from_two_attachments_reports_both() {
        let m = matter("two-attachments").await;
        // One Person id hanging from a participation and a reference at
        // once, with no Person row behind either: the participation row is
        // the out-of-band write past every accessor's existence check, the
        // edge the never-minted target the graph happily holds.
        let gone = Uuid::now_v7();
        let now = chrono::Utc::now().to_rfc3339();
        m.db
            .query(
                "CREATE $id SET person_id = $person, project_id = $project, \
                 participation = $participation, inserted_at = $now, updated_at = $now",
            )
            .bind(("id", record_id("person_project_role", Uuid::now_v7())))
            .bind(("person", record_id("person", gone)))
            .bind(("project", record_id("project", m.project.id)))
            .bind((
                "participation",
                projects::participation_for_role(Role::Client),
            ))
            .bind(("now", now))
            .await
            .unwrap()
            .check()
            .unwrap();
        project_references::reference(
            &m.db,
            m.project.id,
            Party::person(gone),
            REASON_ADVERSE_PARTY,
        )
        .await
        .unwrap();

        let resolution = resolve(&m.db, &m.project).await.unwrap();
        // Both dangling attachments are named: restoring the Person repairs
        // them at once, and neither attachment reads as intact.
        assert_eq!(
            resolution.findings,
            vec![
                Finding::MissingPerson {
                    id: gone,
                    attached_as: PersonAttachment::Participation,
                },
                Finding::MissingPerson {
                    id: gone,
                    attached_as: PersonAttachment::Reference,
                },
            ]
        );
    }

    #[tokio::test]
    async fn findings_read_in_the_order_the_refusal_reads_them() {
        let m = matter("refusal-order").await;
        attach(&m).await;
        // Three classes on one matter, in the order the refusal reads
        // them: a dangling participation, a dangling referenced entity,
        // and a live client entity whose stored metadata is not an
        // object. (The fourth class — a missing client Entity — cannot
        // share a matter with invalid metadata: a deleted client reports
        // ProjectClient, a live-but-malformed one reports Metadata. The
        // missing-client test covers that attachment.)
        let gone = Uuid::now_v7();
        project_references::reference(
            &m.db,
            m.project.id,
            Party::entity(gone),
            REASON_COUNTERPARTY_ENTITY,
        )
        .await
        .unwrap();
        set_metadata(&m.db, m.client.id, serde_json::json!([1, 2, 3])).await;
        delete_person(&m.db, m.participant.id).await;

        let resolution = resolve(&m.db, &m.project).await.unwrap();
        assert_eq!(
            resolution.findings,
            vec![
                Finding::MissingPerson {
                    id: m.participant.id,
                    attached_as: PersonAttachment::Participation,
                },
                Finding::MissingEntity {
                    id: gone,
                    attached_as: super::EntityAttachment::Reference,
                },
                Finding::InvalidEntityMetadata {
                    id: m.client.id,
                    field: EntityField::Metadata,
                },
            ]
        );
    }

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
