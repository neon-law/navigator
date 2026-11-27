//! The `project_reference` relation — a Project naming a Person or an
//! Entity it has verified, with no standing on the matter attached.
//!
//! # A reference is not a participation
//!
//! Every `person_project_role.participation` value grants a lens on the
//! matter. A counterparty's officer, its registered agent, the signatory on
//! the instrument, the counterparty organization itself, or someone who
//! works at a government office must be durable
//! so the Conflict-Check Graph can collide a future matter with them.
//! A reference grants no participation or access to the matter. The row
//! is its own PostgreSQL `project_reference` row, carrying
//! only a `reason`. `store::access` and the embedded Rego policy never
//! read it. The lawyer party roster lists references under "Referenced,
//! no standing on the matter"; the client lens never loads or renders
//! them. Tests cover the access boundary and the roster lenses.
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
use sqlx::Row;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::PostgresDb;
use crate::postgres_graph::graph_authority;
use crate::relationships::{Endpoint, NewRelationship, Relationship};
pub use model::project_references::{is_reason, REASONS};
pub use model::project_references::{
    REASON_ADVERSE_PARTY, REASON_COUNTERPARTY_ENTITY, REASON_COUNTERPARTY_OFFICER,
    REASON_GOVERNMENT_OFFICIAL, REASON_REGISTERED_AGENT, REASON_SIGNATORY,
};

const PROJECT_TABLE: &str = "project";

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

    /// The PostgreSQL column that names this party. A literal, never input.
    const fn column(self) -> &'static str {
        match self.kind {
            Endpoint::Person => "person_id",
            Endpoint::Entity => "entity_id",
        }
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

const PG_SELECT: &str = "project_id, person_id, entity_id, reason, inserted_at, updated_at";

fn from_pg(row: &sqlx::postgres::PgRow) -> Result<ProjectReference, ProjectReferenceError> {
    let person_id: Option<Uuid> = row.try_get("person_id")?;
    let entity_id: Option<Uuid> = row.try_get("entity_id")?;
    let party = match (person_id, entity_id) {
        (Some(id), None) => Party::person(id),
        (None, Some(id)) => Party::entity(id),
        // The table's CHECK makes exactly one party column set.
        _ => return Err(ProjectReferenceError::WriteReturnedNothing),
    };
    Ok(ProjectReference {
        project_id: row.try_get("project_id")?,
        party,
        reason: row.try_get("reason")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

async fn authority() -> Result<PostgresDb, ProjectReferenceError> {
    graph_authority()
        .await
        .map_err(ProjectReferenceError::Engine)
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectReferenceError {
    /// A reason outside [`REASONS`]. Refused here so the caller reads a
    /// named error rather than the engine's ASSERT text.
    #[error("unknown project reference reason {reason:?}; expected one of {expected}", reason = .0, expected = REASONS.join(", "))]
    UnknownReason(String),
    /// A write reported success but returned no row, or one this module
    /// could not read back.
    #[error("writing a project reference returned no usable row")]
    WriteReturnedNothing,
    /// The authoritative PostgreSQL store refused the read or write.
    #[error("PostgreSQL: {0}")]
    Postgres(#[from] sqlx::Error),
    /// PostgreSQL graph authority is unavailable.
    #[error("{0}")]
    Engine(String),
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
    _db: &ApplicationStore,
    project_id: Uuid,
    party: Party,
    reason: &str,
) -> Result<ProjectReference, ProjectReferenceError> {
    let reason = reason.trim();
    if !is_reason(reason) {
        return Err(ProjectReferenceError::UnknownReason(reason.to_string()));
    }
    let postgres = authority().await?;
    // A concurrent writer's row wins the partial unique index; either
    // way the one row is read back.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO project_reference (project_id, {}, reason, inserted_at, updated_at) \
         VALUES ($1, $2, $3, now(), now()) ON CONFLICT DO NOTHING",
        party.column()
    )))
    .bind(project_id)
    .bind(party.id)
    .bind(reason)
    .execute(&postgres.0)
    .await?;
    find_in_postgres(&postgres, project_id, party, reason)
        .await?
        .ok_or(ProjectReferenceError::WriteReturnedNothing)
}

/// The one reference matching all three parts of the natural key, if any.
///
/// # Errors
///
/// [`ProjectReferenceError::Db`] if the lookup fails.
pub async fn find(
    _db: &ApplicationStore,
    project_id: Uuid,
    party: Party,
    reason: &str,
) -> Result<Option<ProjectReference>, ProjectReferenceError> {
    let postgres = authority().await?;
    find_in_postgres(&postgres, project_id, party, reason).await
}

async fn find_in_postgres(
    postgres: &PostgresDb,
    project_id: Uuid,
    party: Party,
    reason: &str,
) -> Result<Option<ProjectReference>, ProjectReferenceError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {PG_SELECT} FROM project_reference \
         WHERE project_id = $1 AND {} = $2 AND reason = $3",
        party.column()
    )))
    .bind(project_id)
    .bind(party.id)
    .bind(reason.trim())
    .fetch_optional(&postgres.0)
    .await?
    .as_ref()
    .map(from_pg)
    .transpose()
}

/// Whether `project_id` references `party` for any reason.
///
/// # Errors
///
/// [`ProjectReferenceError::Db`] if the lookup fails.
pub async fn is_referenced(
    _db: &ApplicationStore,
    project_id: Uuid,
    party: Party,
) -> Result<bool, ProjectReferenceError> {
    let postgres = authority().await?;
    let found = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT 1 FROM project_reference WHERE project_id = $1 AND {} = $2 LIMIT 1",
        party.column()
    )))
    .bind(project_id)
    .bind(party.id)
    .fetch_optional(&postgres.0)
    .await?;
    Ok(found.is_some())
}

/// Every party one Project references, oldest first.
///
/// # Errors
///
/// [`ProjectReferenceError::Db`] if the lookup fails.
pub async fn for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<ProjectReference>, ProjectReferenceError> {
    let postgres = authority().await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {PG_SELECT} FROM project_reference WHERE project_id = $1 \
         ORDER BY inserted_at ASC, id ASC"
    )))
    .bind(project_id)
    .fetch_all(&postgres.0)
    .await?
    .iter()
    .map(from_pg)
    .collect()
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
    db: &ApplicationStore,
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
    let log_input = crate::relationship_logs::NewRelationshipLog {
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
    };
    let mut input = NewRelationship {
        from: from.kind,
        from_id: from.id,
        to: command.party.kind,
        to_id: command.party.id,
        kind: crate::relationships::KIND_ADVERSE_TO.to_string(),
        confidence_pct: 100,
        source_kind: crate::relationships::SOURCE_MANUAL.to_string(),
        source_id: None,
        detail: Some(format!("recorded on project {}", project.code)),
    };
    let postgres = authority().await?;
    record_adversity_on(db, &postgres, &log_input, &mut input).await
}

async fn record_adversity_on(
    db: &ApplicationStore,
    postgres: &PostgresDb,
    log_input: &crate::relationship_logs::NewRelationshipLog,
    input: &mut NewRelationship,
) -> Result<Relationship, AdversityError> {
    let from_name = crate::relationships::endpoint_name(db, input.from, input.from_id).await?;
    let to_name = crate::relationships::endpoint_name(db, input.to, input.to_id).await?;
    let mut transaction = postgres
        .pool()
        .begin()
        .await
        .map_err(crate::relationships::RelationshipError::from)?;
    let log = crate::relationship_logs::record_on(&mut *transaction, Uuid::now_v7(), log_input)
        .await
        .map_err(crate::relationship_logs::RelationshipLogError::from)?;
    input.source_id = Some(log.id);
    let relationship =
        crate::relationships::record_on(&mut transaction, input, from_name, to_name).await?;
    transaction
        .commit()
        .await
        .map_err(crate::relationships::RelationshipError::from)?;
    Ok(relationship)
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
    use crate::test_support::{self, seed_entity, seed_project};

    async fn counterparty(
        application_store: &crate::application_store::ApplicationStore,
    ) -> uuid::Uuid {
        crate::persons::create(
            application_store,
            &NewPerson::with_role("Agent Example", "agent@example.com", Role::Client),
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn a_reference_is_idempotent_and_grants_no_participation() {
        test_support::with_required_postgres(async {
            let application_store = crate::application_store::ApplicationStore::uninitialized();
            test_support::seed_compiled_brands_postgres(&application_store).await;
            let project = seed_project(&application_store, "Widget matter").await;
            let agent = counterparty(&application_store).await;

            let first = reference(
                &application_store,
                project.id,
                Party::person(agent),
                REASON_REGISTERED_AGENT,
            )
            .await
            .unwrap();
            let again = reference(
                &application_store,
                project.id,
                Party::person(agent),
                REASON_REGISTERED_AGENT,
            )
            .await
            .unwrap();
            assert_eq!(first, again, "the second write finds the first row");
            assert_eq!(
                for_project(&application_store, project.id)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert!(
                is_referenced(&application_store, project.id, Party::person(agent))
                    .await
                    .unwrap()
            );
            assert!(
                find(
                    &application_store,
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
                crate::projects::participation_for_person(&application_store, agent, project.id)
                    .await
                    .unwrap()
                    .is_none(),
                "a reference writes no participation row"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn every_schema_reason_is_writable_and_an_unknown_one_is_refused() {
        test_support::with_required_postgres(async {
        let application_store = crate::application_store::ApplicationStore::uninitialized();
        test_support::seed_compiled_brands_postgres(&application_store).await;
        let project = seed_project(&application_store, "Widget matter").await;
        let party = Party::entity(seed_entity(&application_store).await);
        for reason in REASONS {
            reference(&application_store, project.id, party, reason)
                .await
                .unwrap_or_else(|error| panic!("{reason}: {error}"));
        }
        let refused = reference(&application_store, project.id, party, "co_counsel")
            .await
            .expect_err("a participation-shaped word is not a reference reason");
        assert!(
            matches!(refused, ProjectReferenceError::UnknownReason(word) if word == "co_counsel")
        );
            }).await;
    }

    #[tokio::test]
    async fn adversity_requires_a_reference_and_then_lands_a_sourced_manual_edge() {
        test_support::with_required_postgres(async {
            let application_store = crate::application_store::ApplicationStore::uninitialized();
            test_support::seed_compiled_brands_postgres(&application_store).await;
            let project = seed_project(&application_store, "Widget matter").await;
            let opponent = Party::entity(seed_entity(&application_store).await);
            let command = RecordAdversityCommand {
                project_id: project.id,
                party: opponent,
                subject: AdversitySubject::Entity,
                actor_person_id: None,
            };

            let refused = record_adversity(&application_store, &command)
                .await
                .expect_err("adversity is asserted against a referenced party only");
            assert!(matches!(refused, AdversityError::PartyNotReferenced));

            reference(
                &application_store,
                project.id,
                opponent,
                REASON_COUNTERPARTY_ENTITY,
            )
            .await
            .unwrap();
            let edge = record_adversity(&application_store, &command)
                .await
                .unwrap();
            assert_eq!(edge.from, Endpoint::Entity);
            assert_eq!(edge.from_id, project.entity_id);
            assert_eq!(edge.to_id, opponent.id);
            assert_eq!(edge.kind, KIND_ADVERSE_TO);
            assert_eq!(edge.confidence_pct, 100);
            assert_eq!(edge.source_kind, SOURCE_MANUAL);

            let log = crate::relationship_logs::for_project(&application_store, project.id)
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
        })
        .await;
    }

    #[tokio::test]
    async fn adversity_from_the_client_dri_needs_one_designated() {
        test_support::with_required_postgres(async {
            let application_store = crate::application_store::ApplicationStore::uninitialized();
            test_support::seed_compiled_brands_postgres(&application_store).await;
            let project = seed_project(&application_store, "Widget matter").await;
            let opponent = Party::person(counterparty(&application_store).await);
            reference(
                &application_store,
                project.id,
                opponent,
                REASON_ADVERSE_PARTY,
            )
            .await
            .unwrap();
            let command = RecordAdversityCommand {
                project_id: project.id,
                party: opponent,
                subject: AdversitySubject::ClientDri,
                actor_person_id: None,
            };
            let refused = record_adversity(&application_store, &command)
                .await
                .expect_err("no client DRI yet");
            assert!(matches!(refused, AdversityError::NoClientDri));

            let client = crate::persons::create(
                &application_store,
                &NewPerson::with_role("Client Example", "client@example.com", Role::Client),
            )
            .await
            .unwrap()
            .id;
            crate::projects::designate_dri(
                &application_store,
                project.id,
                client,
                crate::projects::DriSide::Client,
            )
            .await
            .unwrap();
            let edge = record_adversity(&application_store, &command)
                .await
                .unwrap();
            assert_eq!(edge.from, Endpoint::Person);
            assert_eq!(edge.from_id, client);
        })
        .await;
    }

    #[tokio::test]
    async fn adversity_on_an_unknown_project_is_refused() {
        test_support::with_required_postgres(async {
            let application_store = crate::application_store::ApplicationStore::uninitialized();
            let refused = record_adversity(
                &application_store,
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
        })
        .await;
    }
}
