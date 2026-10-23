//! The `project_reference` relation — a Project naming a Person or an
//! Entity it has verified, with no standing on the matter attached.
//!
//! # A reference is not a participation
//!
//! Every `person_project_role.participation` value grants a lens on the
//! matter. A counterparty's officer, its registered agent, the signatory on
//! the instrument, or the counterparty organization itself must be durable
//! so the Conflict-Check Graph can collide a future matter with them, and
//! must never reach the portal. So the row is its own relation,
//! `RELATE project->project_reference->(person|entity)`, carrying only a
//! `reason`. `store::access` and the embedded Rego policy never read it;
//! the portal party list never renders it. Each carries a test that fails
//! if that changes.
//!
//! # Adversity is recorded against a referenced party
//!
//! [`record_adversity`] is the matter's own write path into the graph: a
//! `manual` `adverse_to` edge at full confidence from the client (the
//! Project's Entity, or its client DRI) to a party this Project has first
//! referenced. The edge's `source_id` is the Relationship Log row the
//! write appends, which carries the Project, so the edge's provenance names
//! the matter that lived the adversity.
//!
//! # Idempotence is the contract
//!
//! `store::seed` and the CLI re-run over a live database. The
//! `project_reference_tie` index closes the find-then-insert race and
//! [`reference`] re-reads the winner's row when it loses, the pattern
//! [`crate::entity_roles::grant`] established.

use chrono::{DateTime, Utc};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::relationships::{Endpoint, NewRelationship, Relationship};
use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

const TABLE: &str = "project_reference";
const PROJECT_TABLE: &str = "project";

/// Reason: an officer, member, or manager of the counterparty.
pub const REASON_COUNTERPARTY_OFFICER: &str = "counterparty_officer";
/// Reason: the counterparty's registered agent for service.
pub const REASON_REGISTERED_AGENT: &str = "registered_agent";
/// Reason: the individual who signed the instrument.
pub const REASON_SIGNATORY: &str = "signatory";
/// Reason: the party the client is adverse to.
pub const REASON_ADVERSE_PARTY: &str = "adverse_party";
/// Reason: the counterparty organization itself.
pub const REASON_COUNTERPARTY_ENTITY: &str = "counterparty_entity";

/// Every reason the schema admits, in the schema's order. The `ASSERT` on
/// `project_reference.reason` is the engine's copy of this list.
pub const REASONS: [&str; 5] = [
    REASON_COUNTERPARTY_OFFICER,
    REASON_REGISTERED_AGENT,
    REASON_SIGNATORY,
    REASON_ADVERSE_PARTY,
    REASON_COUNTERPARTY_ENTITY,
];

/// The action a [`record_adversity`] write appends to the Relationship Log.
pub const ADVERSITY_RECORDED_ACTION: &str = "adversity_recorded";

/// One end of a reference or an adversity edge: which table, and which row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Party {
    pub kind: Endpoint,
    pub id: Uuid,
}

impl Party {
    #[must_use]
    pub const fn person(id: Uuid) -> Self {
        Self {
            kind: Endpoint::Person,
            id,
        }
    }

    #[must_use]
    pub const fn entity(id: Uuid) -> Self {
        Self {
            kind: Endpoint::Entity,
            id,
        }
    }

    fn record(self) -> surrealdb::types::RecordId {
        record_id(self.kind.table(), self.id)
    }
}

/// One reference from a Project to a party.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectReference {
    pub project_id: Uuid,
    pub party: Party,
    pub reason: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct ProjectReferenceRow {
    r#in: surrealdb::types::RecordId,
    out: surrealdb::types::RecordId,
    reason: String,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl ProjectReferenceRow {
    fn into_reference(self) -> Option<ProjectReference> {
        Some(ProjectReference {
            project_id: record_uuid(&self.r#in)?,
            party: Party {
                kind: Endpoint::from_table(self.out.table.as_str())?,
                id: record_uuid(&self.out)?,
            },
            reason: self.reason,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

const SELECT: &str = "in, out, reason, inserted_at, updated_at";

#[derive(Debug, thiserror::Error)]
pub enum ProjectReferenceError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// A reason outside [`REASONS`]. Refused here so the caller reads a
    /// named error rather than the engine's ASSERT text.
    #[error("unknown project reference reason {reason:?}; expected one of {expected}", reason = .0, expected = REASONS.join(", "))]
    UnknownReason(String),
    /// A write reported success but returned no row, or one this module
    /// could not read back.
    #[error("writing a project reference returned no usable row")]
    WriteReturnedNothing,
}

fn is_duplicate_tie(error: &surrealdb::Error) -> bool {
    retry::unique_violation(error) == Some("project_reference_tie")
}

fn many(
    mut response: surrealdb::IndexedResults,
) -> Result<Vec<ProjectReference>, ProjectReferenceError> {
    let rows: Vec<ProjectReferenceRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(ProjectReferenceRow::into_reference)
        .collect())
}

/// Whether `reason` is one the schema admits.
#[must_use]
pub fn is_reason(reason: &str) -> bool {
    REASONS.contains(&reason)
}

/// Record that `project_id` references `party` for `reason`, returning the
/// reference whether this call created it or found it already there.
///
/// Grants nothing: no participation row, no lens, no portal.
///
/// # Errors
///
/// [`ProjectReferenceError::UnknownReason`] for a reason outside
/// [`REASONS`]; [`ProjectReferenceError::Db`] if the lookup or the write
/// fails, including a `project_id` or party with no row behind it.
pub async fn reference(
    db: &SurrealDb,
    project_id: Uuid,
    party: Party,
    reason: &str,
) -> Result<ProjectReference, ProjectReferenceError> {
    let reason = reason.trim();
    if !is_reason(reason) {
        return Err(ProjectReferenceError::UnknownReason(reason.to_string()));
    }
    if let Some(existing) = find(db, project_id, party, reason).await? {
        mirror_reference(db, &existing).await;
        return Ok(existing);
    }

    let written = retry::writing(|| {
        db.query(format!(
            "RELATE $project->{TABLE}->$party \
             SET reason = $reason, inserted_at = time::now(), updated_at = time::now() \
             RETURN {SELECT}"
        ))
        .bind(("project", record_id(PROJECT_TABLE, project_id)))
        .bind(("party", party.record()))
        .bind(("reason", reason.to_string()))
    })
    .await;

    match written {
        Ok(mut response) => {
            let rows: Vec<ProjectReferenceRow> = response.take(0)?;
            let reference = rows
                .into_iter()
                .next()
                .and_then(ProjectReferenceRow::into_reference)
                .ok_or(ProjectReferenceError::WriteReturnedNothing)?;
            mirror_reference(db, &reference).await;
            Ok(reference)
        }
        // Another writer minted the same reference between the read above
        // and this write. Its row is the answer.
        Err(error) if is_duplicate_tie(&error) => {
            let reference = find(db, project_id, party, reason)
                .await?
                .ok_or(ProjectReferenceError::WriteReturnedNothing)?;
            mirror_reference(db, &reference).await;
            Ok(reference)
        }
        Err(error) => Err(ProjectReferenceError::Db(error)),
    }
}

async fn mirror_reference(db: &SurrealDb, reference: &ProjectReference) {
    let _ = crate::postgres_mirror::after_write(
        db,
        crate::postgres_mirror::MirrorKey::ProjectReference {
            project_id: reference.project_id,
            party_kind: reference.party.kind.table().to_owned(),
            party_id: reference.party.id,
            reason: reference.reason.clone(),
        },
    )
    .await;
}

/// The one reference matching all three parts of the natural key, if any.
///
/// # Errors
///
/// [`ProjectReferenceError::Db`] if the lookup fails.
pub async fn find(
    db: &SurrealDb,
    project_id: Uuid,
    party: Party,
    reason: &str,
) -> Result<Option<ProjectReference>, ProjectReferenceError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE in = $project AND out = $party AND reason = $reason LIMIT 1"
        ))
        .bind(("project", record_id(PROJECT_TABLE, project_id)))
        .bind(("party", party.record()))
        .bind(("reason", reason.trim().to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<ProjectReferenceRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(ProjectReferenceRow::into_reference))
}

/// Whether `project_id` references `party` for any reason.
///
/// # Errors
///
/// [`ProjectReferenceError::Db`] if the lookup fails.
pub async fn is_referenced(
    db: &SurrealDb,
    project_id: Uuid,
    party: Party,
) -> Result<bool, ProjectReferenceError> {
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM ONLY {TABLE} WHERE in = $project AND out = $party LIMIT 1"
        ))
        .bind(("project", record_id(PROJECT_TABLE, project_id)))
        .bind(("party", party.record()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<surrealdb::types::RecordId> = response.take(0)?;
    Ok(row.is_some())
}

/// Every party one Project references, oldest first.
///
/// # Errors
///
/// [`ProjectReferenceError::Db`] if the lookup fails.
pub async fn for_project(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<ProjectReference>, ProjectReferenceError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE in = $project ORDER BY inserted_at ASC"
        ))
        .bind(("project", record_id(PROJECT_TABLE, project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Which side of the client an adversity edge starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AdversitySubject {
    /// The Entity the Project opened against — the usual case, since every
    /// matter opens against one.
    #[default]
    Entity,
    /// The Project's client DRI, for a matter whose adversity is personal
    /// to the natural person rather than to their company.
    ClientDri,
}

/// What [`record_adversity`] writes.
#[derive(Debug, Clone)]
pub struct RecordAdversityCommand {
    pub project_id: Uuid,
    /// The party the client is adverse to. Must already be referenced by
    /// the Project.
    pub party: Party,
    pub subject: AdversitySubject,
    /// The person asserting the adversity, recorded as the Relationship
    /// Log actor. `None` for a session with no person behind it.
    pub actor_person_id: Option<Uuid>,
}

#[derive(Debug, thiserror::Error)]
pub enum AdversityError {
    #[error("no such project")]
    ProjectNotFound,
    /// The party is not one this Project references. Reference it first:
    /// adversity is asserted against a party the matter has verified.
    #[error("the project does not reference this party; add a project reference first")]
    PartyNotReferenced,
    /// `subject: client_dri` on a matter with no client DRI designated.
    #[error("the project has no client DRI to record adversity from")]
    NoClientDri,
    #[error(transparent)]
    Project(#[from] crate::projects::ProjectStoreError),
    #[error(transparent)]
    Reference(#[from] ProjectReferenceError),
    #[error(transparent)]
    Log(#[from] crate::relationship_logs::RelationshipLogError),
    #[error(transparent)]
    Relationship(#[from] crate::relationships::RelationshipError),
}

/// Record that the Project's client is adverse to a party the Project
/// references: a `manual` `adverse_to` edge at full confidence, sourced to
/// the Relationship Log row this write appends on the Project.
///
/// Full confidence is right here: a lawyer asserting adversity on their own
/// matter is the human-asserted case the `confidence_pct` scale reserves 100
/// for. `store::conflicts::check_new_matter` raises the edge on the next
/// matter that reaches either end of it.
///
/// # Errors
///
/// [`AdversityError::ProjectNotFound`], [`AdversityError::PartyNotReferenced`],
/// or [`AdversityError::NoClientDri`] for a command the caller can correct;
/// the wrapped store errors if a read or write fails.
pub async fn record_adversity(
    db: &SurrealDb,
    command: &RecordAdversityCommand,
) -> Result<Relationship, AdversityError> {
    let project = crate::projects::find_by_id(db, command.project_id)
        .await?
        .ok_or(AdversityError::ProjectNotFound)?;
    if !is_referenced(db, project.id, command.party).await? {
        return Err(AdversityError::PartyNotReferenced);
    }
    let from = match command.subject {
        AdversitySubject::Entity => Party::entity(project.entity_id),
        AdversitySubject::ClientDri => crate::projects::participations_for_project(db, project.id)
            .await?
            .into_iter()
            .find(|row| row.is_client_dri)
            .map(|row| Party::person(row.person_id))
            .ok_or(AdversityError::NoClientDri)?,
    };
    let log = crate::relationship_logs::record(
        db,
        &crate::relationship_logs::NewRelationshipLog {
            actor_person_id: command.actor_person_id,
            subject_type: PROJECT_TABLE.to_string(),
            subject_id: project.id,
            action: ADVERSITY_RECORDED_ACTION.to_string(),
            detail: format!(
                "{}:{} adverse_to {}:{}",
                from.kind.table(),
                from.id,
                command.party.kind.table(),
                command.party.id
            ),
            project_id: Some(project.id),
        },
    )
    .await?;
    Ok(crate::relationships::record(
        db,
        &NewRelationship {
            from: from.kind,
            from_id: from.id,
            to: command.party.kind,
            to_id: command.party.id,
            kind: crate::relationships::KIND_ADVERSE_TO.to_string(),
            confidence_pct: 100,
            source_kind: crate::relationships::SOURCE_MANUAL.to_string(),
            source_id: Some(log.id),
            detail: Some(format!("recorded on project {}", project.code)),
        },
    )
    .await?)
}

#[cfg(test)]
mod tests {
    use super::{
        find, for_project, is_referenced, record_adversity, reference, AdversityError,
        AdversitySubject, Party, ProjectReferenceError, RecordAdversityCommand,
        ADVERSITY_RECORDED_ACTION, REASONS, REASON_ADVERSE_PARTY, REASON_COUNTERPARTY_ENTITY,
        REASON_REGISTERED_AGENT,
    };
    use crate::persons::{NewPerson, Role};
    use crate::relationships::{Endpoint, KIND_ADVERSE_TO, SOURCE_MANUAL};
    use crate::test_support::{mem_surreal, seed_entity, seed_project};

    async fn counterparty(surreal: &crate::surreal::SurrealDb) -> uuid::Uuid {
        crate::persons::create(
            surreal,
            &NewPerson::with_role("Agent Example", "agent@example.com", Role::Client),
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn a_reference_is_idempotent_and_grants_no_participation() {
        let surreal = mem_surreal().await;
        let project = seed_project(&surreal, "Widget matter").await;
        let agent = counterparty(&surreal).await;

        let first = reference(
            &surreal,
            project.id,
            Party::person(agent),
            REASON_REGISTERED_AGENT,
        )
        .await
        .unwrap();
        let again = reference(
            &surreal,
            project.id,
            Party::person(agent),
            REASON_REGISTERED_AGENT,
        )
        .await
        .unwrap();
        assert_eq!(first, again, "the second write finds the first row");
        assert_eq!(for_project(&surreal, project.id).await.unwrap().len(), 1);
        assert!(is_referenced(&surreal, project.id, Party::person(agent))
            .await
            .unwrap());
        assert!(
            find(
                &surreal,
                project.id,
                Party::person(agent),
                REASON_ADVERSE_PARTY
            )
            .await
            .unwrap()
            .is_none(),
            "a reference is keyed by its reason too"
        );
        assert!(
            crate::projects::participation_for_person(&surreal, agent, project.id)
                .await
                .unwrap()
                .is_none(),
            "a reference writes no participation row"
        );
    }

    #[tokio::test]
    async fn every_schema_reason_is_writable_and_an_unknown_one_is_refused() {
        let surreal = mem_surreal().await;
        let project = seed_project(&surreal, "Widget matter").await;
        let party = Party::entity(seed_entity(&surreal).await);
        for reason in REASONS {
            reference(&surreal, project.id, party, reason)
                .await
                .unwrap_or_else(|error| panic!("{reason}: {error}"));
        }
        let refused = reference(&surreal, project.id, party, "co_counsel")
            .await
            .expect_err("a participation-shaped word is not a reference reason");
        assert!(
            matches!(refused, ProjectReferenceError::UnknownReason(word) if word == "co_counsel")
        );
    }

    #[tokio::test]
    async fn adversity_requires_a_reference_and_then_lands_a_sourced_manual_edge() {
        let surreal = mem_surreal().await;
        let project = seed_project(&surreal, "Widget matter").await;
        let opponent = Party::entity(seed_entity(&surreal).await);
        let command = RecordAdversityCommand {
            project_id: project.id,
            party: opponent,
            subject: AdversitySubject::Entity,
            actor_person_id: None,
        };

        let refused = record_adversity(&surreal, &command)
            .await
            .expect_err("adversity is asserted against a referenced party only");
        assert!(matches!(refused, AdversityError::PartyNotReferenced));

        reference(&surreal, project.id, opponent, REASON_COUNTERPARTY_ENTITY)
            .await
            .unwrap();
        let edge = record_adversity(&surreal, &command).await.unwrap();
        assert_eq!(edge.from, Endpoint::Entity);
        assert_eq!(edge.from_id, project.entity_id);
        assert_eq!(edge.to_id, opponent.id);
        assert_eq!(edge.kind, KIND_ADVERSE_TO);
        assert_eq!(edge.confidence_pct, 100);
        assert_eq!(edge.source_kind, SOURCE_MANUAL);

        let log = crate::relationship_logs::for_project(&surreal, project.id)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.action == ADVERSITY_RECORDED_ACTION)
            .expect("the write appends a log row on the project");
        assert_eq!(
            edge.source_id,
            Some(log.id),
            "the edge is sourced to the matter's log row"
        );
        assert_eq!(log.project_id, Some(project.id));
    }

    #[tokio::test]
    async fn adversity_from_the_client_dri_needs_one_designated() {
        let surreal = mem_surreal().await;
        let project = seed_project(&surreal, "Widget matter").await;
        let opponent = Party::person(counterparty(&surreal).await);
        reference(&surreal, project.id, opponent, REASON_ADVERSE_PARTY)
            .await
            .unwrap();
        let command = RecordAdversityCommand {
            project_id: project.id,
            party: opponent,
            subject: AdversitySubject::ClientDri,
            actor_person_id: None,
        };
        let refused = record_adversity(&surreal, &command)
            .await
            .expect_err("no client DRI yet");
        assert!(matches!(refused, AdversityError::NoClientDri));

        let client = crate::persons::create(
            &surreal,
            &NewPerson::with_role("Client Example", "client@example.com", Role::Client),
        )
        .await
        .unwrap()
        .id;
        crate::projects::designate_dri_in_surreal(
            &surreal,
            project.id,
            client,
            crate::projects::DriSide::Client,
        )
        .await
        .unwrap();
        let edge = record_adversity(&surreal, &command).await.unwrap();
        assert_eq!(edge.from, Endpoint::Person);
        assert_eq!(edge.from_id, client);
    }

    #[tokio::test]
    async fn adversity_on_an_unknown_project_is_refused() {
        let surreal = mem_surreal().await;
        let refused = record_adversity(
            &surreal,
            &RecordAdversityCommand {
                project_id: uuid::Uuid::now_v7(),
                party: Party::person(uuid::Uuid::now_v7()),
                subject: AdversitySubject::Entity,
                actor_person_id: None,
            },
        )
        .await
        .expect_err("no project");
        assert!(matches!(refused, AdversityError::ProjectNotFound));
    }
}
