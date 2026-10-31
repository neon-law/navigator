//! Firm-authored Case Stage vocabulary with audited per-matter transitions,
//! stored relationally in PostgreSQL.
//!
//! A Case Stage is where the matter sits in the Firm's own workflow (intake,
//! pleading, discovery, trial prep, …). The vocabulary is Firm data — an
//! ordered list the Firm defines — never a hardcoded enum. A Case Stage is
//! deliberately distinct from two neighbors it must never be conflated with:
//! `project.status` is the lifecycle (open/closed/archived) and `case.status`
//! is the per-forum litigation posture (open/stayed/closed). See
//! `docs/glossary/case-stage.md`.
//!
//! The model follows the `project_costs.rs` pattern: PostgreSQL-native, typed
//! errors, and an append-only history whose writes the database itself
//! polices — a stage event must name a stage from the matter Firm's own
//! vocabulary, must continue the chain from the matter's current stage, and
//! can never be edited or deleted. The current stage is not a mutable column:
//! it is the newest history row, so the record and the history cannot drift.
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub enum CaseStageError {
    #[error("invalid Case Stage definition: {0}")]
    Invalid(&'static str),
    #[error("the stage is not in the matter Firm's own vocabulary")]
    NotInFirmVocabulary,
    #[error("the Project has no Firm, so no Firm-authored stage vocabulary applies")]
    ProjectHasNoFirm,
    #[error("Project or stage definition was not found")]
    NotFound,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("PostgreSQL is unavailable")]
    Postgres(#[from] crate::postgres::PostgresError),
}

/// One entry in the Firm's ordered stage vocabulary. Firm data, not a
/// hardcoded enum: the Firm names its stages and their order; `case_type`
/// narrows a stage to one litigation posture when the vocabulary differs by
/// case kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageDefinition {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub label: String,
    pub position: i32,
    pub case_type: Option<String>,
    pub inserted_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

/// One audited transition of a matter to a stage. The first event on a matter
/// has no origin; every later one continues from the current stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageEvent {
    pub id: Uuid,
    pub project_id: Uuid,
    pub from_stage_id: Option<Uuid>,
    pub to_stage_id: Uuid,
    pub actor_person_id: Uuid,
    pub note: Option<String>,
    pub occurred_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewStageDefinition {
    pub label: String,
    pub position: i32,
    pub case_type: Option<String>,
}

fn from_definition_row(row: &sqlx::postgres::PgRow) -> Result<StageDefinition, CaseStageError> {
    Ok(StageDefinition {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        label: row.try_get("label")?,
        position: row.try_get("position")?,
        case_type: row.try_get("case_type")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn validate_note(note: Option<&String>) -> Result<(), CaseStageError> {
    if note.is_some_and(|note| note.trim().is_empty() || note.trim().len() > 500) {
        return Err(CaseStageError::Invalid("note"));
    }
    Ok(())
}

fn validate(input: &NewStageDefinition) -> Result<(), CaseStageError> {
    if input.label.trim().is_empty() || input.label.trim().len() > 100 {
        return Err(CaseStageError::Invalid("label"));
    }
    if input.position < 0 {
        return Err(CaseStageError::Invalid("position"));
    }
    if input
        .case_type
        .as_deref()
        .is_some_and(|case_type| case_type.trim().is_empty() || case_type.trim().len() > 100)
    {
        return Err(CaseStageError::Invalid("case_type"));
    }
    Ok(())
}

/// Add a stage to a Firm's vocabulary. Positions order the list; a gap is
/// deliberate — it lets the Firm slot a stage between two others later.
pub async fn create_definition(
    pg: &PostgresDb,
    firm_id: Uuid,
    input: &NewStageDefinition,
) -> Result<StageDefinition, CaseStageError> {
    validate(input)?;
    let id = Uuid::new_v4();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO stage_definition(id,firm_id,label,position,case_type,inserted_at,updated_at) \
         VALUES($1,$2,$3,$4,$5,$6,$6)",
    )
    .bind(id)
    .bind(firm_id)
    .bind(input.label.trim())
    .bind(input.position)
    .bind(input.case_type.as_deref().map(str::trim))
    .bind(now)
    .execute(&pg.0)
    .await?;
    definition(pg, id).await
}

/// A Firm's vocabulary, in the order the Firm staged it.
pub async fn list_definitions(
    pg: &PostgresDb,
    firm_id: Uuid,
) -> Result<Vec<StageDefinition>, CaseStageError> {
    sqlx::query(
        "SELECT id,firm_id,label,position,case_type,inserted_at,updated_at \
         FROM stage_definition WHERE firm_id=$1 ORDER BY position, label",
    )
    .bind(firm_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(from_definition_row)
    .collect()
}

pub async fn definition(pg: &PostgresDb, id: Uuid) -> Result<StageDefinition, CaseStageError> {
    sqlx::query(
        "SELECT id,firm_id,label,position,case_type,inserted_at,updated_at \
         FROM stage_definition WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(from_definition_row)
    .transpose()?
    .ok_or(CaseStageError::NotFound)
}

/// Move a matter to a stage from the Firm's own vocabulary, appending the
/// audited history row. Setting the stage the matter already holds is a no-op
/// that records no event. Stages may move backward — a matter can return to
/// discovery — because the Firm's list orders the workflow, not the calendar.
pub async fn set_stage(
    pg: &PostgresDb,
    project_id: Uuid,
    actor_id: Uuid,
    to_stage_id: Uuid,
    note: Option<String>,
) -> Result<StageEvent, CaseStageError> {
    validate_note(note.as_ref())?;
    let mut tx = pg.0.begin().await?;
    // Locking the Project serializes transitions on one matter: the
    // current-stage read and the history append cannot interleave.
    let firm_id: Option<Uuid> = sqlx::query("SELECT firm_id FROM project WHERE id=$1 FOR UPDATE")
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| row.try_get("firm_id"))
        .transpose()?
        .flatten();
    let Some(firm_id) = firm_id else {
        return Err(CaseStageError::ProjectHasNoFirm);
    };
    let stage_firm: Option<Uuid> = sqlx::query("SELECT firm_id FROM stage_definition WHERE id=$1")
        .bind(to_stage_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|row| row.try_get("firm_id"))
        .transpose()?;
    if stage_firm.is_none() {
        return Err(CaseStageError::NotFound);
    }
    if stage_firm != Some(firm_id) {
        return Err(CaseStageError::NotInFirmVocabulary);
    }
    let current: Option<Uuid> = sqlx::query(
        "SELECT to_stage_id FROM stage_event WHERE project_id=$1 \
         ORDER BY occurred_at DESC, id DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?
    .map(|row| row.try_get("to_stage_id"))
    .transpose()?;
    if current == Some(to_stage_id) {
        let event = latest_event(&mut tx, project_id)
            .await?
            .expect("a current stage implies a history row");
        return Ok(event);
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO stage_event(id,project_id,from_stage_id,to_stage_id,actor_person_id,note,occurred_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(project_id)
    .bind(current)
    .bind(to_stage_id)
    .bind(actor_id)
    .bind(note.as_deref().map(str::trim))
    .bind(Utc::now())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    event(pg, id).await
}

/// A matter's stage history, oldest first.
pub async fn history(pg: &PostgresDb, project_id: Uuid) -> Result<Vec<StageEvent>, CaseStageError> {
    sqlx::query(
        "SELECT id,project_id,from_stage_id,to_stage_id,actor_person_id,note,occurred_at \
         FROM stage_event WHERE project_id=$1 ORDER BY occurred_at, id",
    )
    .bind(project_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(from_event_row)
    .collect()
}

/// The stage a matter currently holds, resolved through its newest history
/// row. `None` until the Firm places the matter in its first stage.
pub async fn current_stage(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Option<StageDefinition>, CaseStageError> {
    let mut tx = pg.0.begin().await?;
    let Some(event) = latest_event(&mut tx, project_id).await? else {
        return Ok(None);
    };
    definition(pg, event.to_stage_id).await.map(Some)
}

fn from_event_row(row: &sqlx::postgres::PgRow) -> Result<StageEvent, CaseStageError> {
    Ok(StageEvent {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        from_stage_id: row.try_get("from_stage_id")?,
        to_stage_id: row.try_get("to_stage_id")?,
        actor_person_id: row.try_get("actor_person_id")?,
        note: row.try_get("note")?,
        occurred_at: row.try_get("occurred_at")?,
    })
}

async fn latest_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_id: Uuid,
) -> Result<Option<StageEvent>, CaseStageError> {
    sqlx::query(
        "SELECT id,project_id,from_stage_id,to_stage_id,actor_person_id,note,occurred_at \
         FROM stage_event WHERE project_id=$1 ORDER BY occurred_at DESC, id DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&mut **tx)
    .await?
    .as_ref()
    .map(from_event_row)
    .transpose()
}

async fn event(pg: &PostgresDb, id: Uuid) -> Result<StageEvent, CaseStageError> {
    sqlx::query(
        "SELECT id,project_id,from_stage_id,to_stage_id,actor_person_id,note,occurred_at \
         FROM stage_event WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(from_event_row)
    .transpose()?
    .ok_or(CaseStageError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::{validate, validate_note, CaseStageError, NewStageDefinition};

    fn input() -> NewStageDefinition {
        NewStageDefinition {
            label: "Discovery".into(),
            position: 3,
            case_type: None,
        }
    }

    #[test]
    fn accepts_a_labelled_stage() {
        assert!(validate(&input()).is_ok());
    }

    #[test]
    fn refuses_an_untitled_or_oversized_label() {
        let mut value = input();
        value.label = "   ".into();
        assert!(matches!(
            validate(&value),
            Err(CaseStageError::Invalid("label"))
        ));
        let mut value = input();
        value.label = "x".repeat(101);
        assert!(matches!(
            validate(&value),
            Err(CaseStageError::Invalid("label"))
        ));
    }

    #[test]
    fn refuses_a_negative_position() {
        let mut value = input();
        value.position = -1;
        assert!(matches!(
            validate(&value),
            Err(CaseStageError::Invalid("position"))
        ));
    }

    #[test]
    fn refuses_a_blank_or_oversized_case_type() {
        let mut value = input();
        value.case_type = Some("   ".into());
        assert!(matches!(
            validate(&value),
            Err(CaseStageError::Invalid("case_type"))
        ));
        let mut value = input();
        value.case_type = Some("x".repeat(101));
        assert!(matches!(
            validate(&value),
            Err(CaseStageError::Invalid("case_type"))
        ));
    }

    #[test]
    fn refuses_a_blank_or_oversized_note() {
        assert!(validate_note(None).is_ok());
        let blank = Some("   ".to_string());
        assert!(matches!(
            validate_note(blank.as_ref()),
            Err(CaseStageError::Invalid("note"))
        ));
        let oversized = Some("x".repeat(501));
        assert!(matches!(
            validate_note(oversized.as_ref()),
            Err(CaseStageError::Invalid("note"))
        ));
    }

    // The remaining assertions need a live PostgreSQL: firm-vocabulary
    // validation, the chained history, and the trigger-written guarantees.
    // They share the canonical-schema fixture used across the store crate.
    mod database {
        use super::super::{create_definition, current_stage, history, set_stage, CaseStageError};
        use crate::postgres::PostgresDb;
        use crate::postgres_mirror::tests::{connect, fresh_schema};
        use sqlx::Row;
        use uuid::Uuid;

        struct Vocabulary {
            intake: Uuid,
            discovery: Uuid,
            other_firm_stage: Uuid,
        }

        async fn firm(pg: &PostgresDb) -> Uuid {
            let id: Uuid = sqlx::query("INSERT INTO firm (id,name,status) VALUES ($1,'Synthetic Stage Firm','active') RETURNING id")
                .bind(Uuid::new_v4())
                .fetch_one(pg.pool())
                .await
                .unwrap()
                .get(0);
            id
        }

        async fn seed(pg: &PostgresDb, firm_id: Option<Uuid>) -> Uuid {
            let jurisdiction: Uuid = sqlx::query(
                "INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Synthetic Stage Jurisdiction','STG') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let entity_type: Uuid = sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Stage Entity Type') RETURNING id")
                .bind(Uuid::new_v4())
                .fetch_one(pg.pool())
                .await
                .unwrap()
                .get(0);
            let entity: Uuid = sqlx::query(
                "INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Stage Entity',$2,$3) RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(entity_type)
            .bind(jurisdiction)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let project_id: Uuid = sqlx::query(
                "INSERT INTO project (id,code,name,status,entity_id,firm_id) VALUES ($1,$2,'Synthetic Stage Matter','open',$3,$4) RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("stage-{}", Uuid::new_v4().simple()))
            .bind(entity)
            .bind(firm_id)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            project_id
        }

        async fn vocabulary(pg: &PostgresDb, firm_id: Uuid) -> Vocabulary {
            let intake = create_definition(
                pg,
                firm_id,
                &super::super::NewStageDefinition {
                    label: "Intake".into(),
                    position: 1,
                    case_type: None,
                },
            )
            .await
            .unwrap();
            let discovery = create_definition(
                pg,
                firm_id,
                &super::super::NewStageDefinition {
                    label: "Discovery".into(),
                    position: 2,
                    case_type: None,
                },
            )
            .await
            .unwrap();
            let other_firm = firm(pg).await;
            let other_firm_stage = create_definition(
                pg,
                other_firm,
                &super::super::NewStageDefinition {
                    label: "Another Firm's Stage".into(),
                    position: 1,
                    case_type: None,
                },
            )
            .await
            .unwrap()
            .id;
            Vocabulary {
                intake: intake.id,
                discovery: discovery.id,
                other_firm_stage,
            }
        }

        async fn actor(pg: &PostgresDb) -> Uuid {
            let unique = Uuid::new_v4().simple().to_string();
            let id: Uuid = sqlx::query(
                "INSERT INTO person (id,name,email,role) VALUES ($1,'Synthetic Stage Actor',$2,'lawyer') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("{unique}@example.com"))
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            id
        }

        #[tokio::test]
        async fn set_stage_refuses_a_stage_outside_the_firms_vocabulary() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let firm_id = firm(&pg).await;
            let project_id = seed(&pg, Some(firm_id)).await;
            let vocabulary = vocabulary(&pg, firm_id).await;
            let actor = actor(&pg).await;

            // A stage from a different Firm is refused, and so is a stage id
            // that names nothing.
            assert!(matches!(
                set_stage(&pg, project_id, actor, vocabulary.other_firm_stage, None).await,
                Err(CaseStageError::NotInFirmVocabulary)
            ));
            assert!(matches!(
                set_stage(&pg, project_id, actor, Uuid::new_v4(), None).await,
                Err(CaseStageError::NotFound)
            ));

            // A stage from the matter's own Firm is accepted.
            let placed = set_stage(&pg, project_id, actor, vocabulary.intake, None)
                .await
                .unwrap();
            assert_eq!(placed.to_stage_id, vocabulary.intake);
            assert_eq!(placed.from_stage_id, None, "the first event has no origin");
        }

        #[tokio::test]
        async fn every_transition_appends_history_and_the_chain_stays_honest() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let firm_id = firm(&pg).await;
            let project_id = seed(&pg, Some(firm_id)).await;
            let vocabulary = vocabulary(&pg, firm_id).await;
            let actor = actor(&pg).await;

            assert!(
                current_stage(&pg, project_id).await.unwrap().is_none(),
                "a quiet matter holds no stage yet"
            );

            set_stage(&pg, project_id, actor, vocabulary.intake, None)
                .await
                .unwrap();
            set_stage(
                &pg,
                project_id,
                actor,
                vocabulary.discovery,
                Some("Answers served".into()),
            )
            .await
            .unwrap();
            // A matter can return to an earlier stage: the Firm's list orders
            // the workflow, not the calendar.
            set_stage(&pg, project_id, actor, vocabulary.intake, None)
                .await
                .unwrap();

            let events = history(&pg, project_id).await.unwrap();
            assert_eq!(events.len(), 3, "every transition appends one row");
            assert_eq!(events[0].to_stage_id, vocabulary.intake);
            assert_eq!(events[0].from_stage_id, None);
            assert_eq!(events[1].from_stage_id, Some(vocabulary.intake));
            assert_eq!(events[1].to_stage_id, vocabulary.discovery);
            assert_eq!(events[1].note.as_deref(), Some("Answers served"));
            assert_eq!(events[2].from_stage_id, Some(vocabulary.discovery));
            assert_eq!(events[2].to_stage_id, vocabulary.intake);
            for event in &events {
                assert_eq!(event.actor_person_id, actor, "every event names its actor");
            }

            let current = current_stage(&pg, project_id).await.unwrap().unwrap();
            assert_eq!(current.id, vocabulary.intake);

            // Re-setting the stage the matter already holds is a no-op.
            let unchanged = set_stage(&pg, project_id, actor, vocabulary.intake, None)
                .await
                .unwrap();
            assert_eq!(unchanged.id, events[2].id);
            assert_eq!(history(&pg, project_id).await.unwrap().len(), 3);
        }

        #[tokio::test]
        async fn the_database_itself_refuses_history_tampering_and_cross_firm_writes() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let firm_id = firm(&pg).await;
            let project_id = seed(&pg, Some(firm_id)).await;
            let vocabulary = vocabulary(&pg, firm_id).await;
            let actor = actor(&pg).await;
            let placed = set_stage(&pg, project_id, actor, vocabulary.intake, None)
                .await
                .unwrap();

            // The audit log is immutable.
            let edited = sqlx::query("UPDATE stage_event SET note='Rewritten' WHERE id=$1")
                .bind(placed.id)
                .execute(pg.pool())
                .await;
            assert!(edited.is_err(), "history cannot be edited");
            let removed = sqlx::query("DELETE FROM stage_event WHERE id=$1")
                .bind(placed.id)
                .execute(pg.pool())
                .await;
            assert!(removed.is_err(), "history cannot be deleted");

            // A raw insert bypassing the Firm's vocabulary is refused.
            let cross_firm = sqlx::query(
                "INSERT INTO stage_event(id,project_id,from_stage_id,to_stage_id,actor_person_id,occurred_at) \
                 VALUES($1,$2,$3,$4,$5,now())",
            )
            .bind(Uuid::new_v4())
            .bind(project_id)
            .bind(vocabulary.intake)
            .bind(vocabulary.other_firm_stage)
            .bind(actor)
            .execute(pg.pool())
            .await;
            assert!(
                cross_firm.is_err(),
                "a cross-Firm stage write must be refused"
            );

            // So is one that breaks the chain.
            let broken_chain = sqlx::query(
                "INSERT INTO stage_event(id,project_id,from_stage_id,to_stage_id,actor_person_id,occurred_at) \
                 VALUES($1,$2,$3,$4,$5,now())",
            )
            .bind(Uuid::new_v4())
            .bind(project_id)
            .bind(vocabulary.discovery)
            .bind(vocabulary.discovery)
            .bind(actor)
            .execute(pg.pool())
            .await;
            assert!(
                broken_chain.is_err(),
                "a chain-breaking write must be refused"
            );
        }

        #[tokio::test]
        async fn a_matter_without_a_firm_has_no_vocabulary_to_move_through() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let project_id = seed(&pg, None).await;
            let firm_id = firm(&pg).await;
            let vocabulary = vocabulary(&pg, firm_id).await;
            let actor = actor(&pg).await;

            assert!(matches!(
                set_stage(&pg, project_id, actor, vocabulary.intake, None).await,
                Err(CaseStageError::ProjectHasNoFirm)
            ));
        }
    }
}
