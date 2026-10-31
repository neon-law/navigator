//! Assignable matter work with audited state transitions, stored relationally
//! in PostgreSQL.
//!
//! A Task is work someone does, with a workflow state. It is deliberately a
//! different noun from a Deadline — a Deadline is a docket obligation the date
//! binds, where a miss is a malpractice event; a Task is assignable work whose
//! state moves todo → in_progress → done. See `docs/glossary/task.md`.
//!
//! The model follows the `project_costs.rs` pattern: PostgreSQL-native, integer
//! identifiers, typed errors, and an event-sourced audit trail whose writes the
//! database itself enforces (a mutation without an actor and event kind is
//! refused; events are immutable; rows are retained — done is a transition,
//! never a delete).
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, Transaction};
use uuid::Uuid;

use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub enum ProjectTaskError {
    #[error("invalid Project task: {0}")]
    Invalid(&'static str),
    #[error("task state cannot move from {from} to {to}")]
    Transition {
        from: &'static str,
        to: &'static str,
    },
    #[error("assignee must be a participant on the Project")]
    AssigneeNotParticipant,
    #[error("Project task was not found")]
    NotFound,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("PostgreSQL is unavailable")]
    Postgres(#[from] crate::postgres::PostgresError),
}

/// The workflow state of a Task. The only legal moves are one step forward;
/// `done` closes the row and is a transition, never a delete. The stored
/// word — `as_str` — is also the wire word: the serde form round-trips
/// through the same vocabulary the transition command speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Todo,
    InProgress,
    Done,
}

impl TaskState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }

    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Todo, Self::InProgress) | (Self::InProgress, Self::Done)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTask {
    pub id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub body: Option<String>,
    pub assignee_id: Option<Uuid>,
    pub due_on: Option<NaiveDate>,
    pub state: TaskState,
    pub inserted_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTaskEvent {
    pub id: Uuid,
    pub actor_person_id: Uuid,
    pub event_kind: String,
    pub before_state: Option<serde_json::Value>,
    pub after_state: serde_json::Value,
    pub occurred_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewProjectTask {
    pub title: String,
    pub body: Option<String>,
    /// A participant on the same Project; refused otherwise.
    pub assignee_id: Option<Uuid>,
    pub due_on: Option<NaiveDate>,
}

fn parse_state(raw: &str) -> Result<TaskState, ProjectTaskError> {
    match raw {
        "todo" => Ok(TaskState::Todo),
        "in_progress" => Ok(TaskState::InProgress),
        "done" => Ok(TaskState::Done),
        _ => Err(ProjectTaskError::Invalid("state")),
    }
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<ProjectTask, ProjectTaskError> {
    Ok(ProjectTask {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        title: row.try_get("title")?,
        body: row.try_get("body")?,
        assignee_id: row.try_get("assignee_id")?,
        due_on: row.try_get("due_on")?,
        state: parse_state(&row.try_get::<String, _>("state")?)?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn validate(input: &NewProjectTask) -> Result<(), ProjectTaskError> {
    if input.title.trim().is_empty() || input.title.trim().len() > 300 {
        return Err(ProjectTaskError::Invalid("title"));
    }
    if input
        .body
        .as_deref()
        .is_some_and(|body| body.trim().is_empty() || body.trim().len() > 2000)
    {
        return Err(ProjectTaskError::Invalid("body"));
    }
    Ok(())
}

async fn set_audit_context(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    actor_id: Uuid,
    kind: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT set_config('navigator.project_task_actor',$1,true), \
         set_config('navigator.project_task_event_kind',$2,true)",
    )
    .bind(actor_id.to_string())
    .bind(kind)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The assignee must already hold a participation row on the Project. The
/// composite foreign key refuses an outsider at the database too; this check
/// gives the caller a typed denial instead of a constraint violation.
async fn require_participant(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    project_id: Uuid,
    person_id: Uuid,
) -> Result<(), ProjectTaskError> {
    sqlx::query("SELECT id FROM person_project_role WHERE project_id=$1 AND person_id=$2")
        .bind(project_id)
        .bind(person_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ProjectTaskError::AssigneeNotParticipant)?;
    Ok(())
}

/// Create a Task on a Project. The Project must already be mirrored to
/// PostgreSQL; the row starts in the `todo` state.
pub async fn create(
    pg: &PostgresDb,
    project_id: Uuid,
    actor_id: Uuid,
    input: &NewProjectTask,
) -> Result<ProjectTask, ProjectTaskError> {
    validate(input)?;
    let id = Uuid::new_v4();
    let now = Utc::now();
    let mut tx = pg.0.begin().await?;
    sqlx::query("SELECT id FROM project WHERE id=$1")
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ProjectTaskError::Invalid(
            "Project is not mirrored to PostgreSQL",
        ))?;
    if let Some(assignee_id) = input.assignee_id {
        require_participant(&mut tx, project_id, assignee_id).await?;
    }
    set_audit_context(&mut tx, actor_id, "created").await?;
    sqlx::query(
        "INSERT INTO project_task(id,project_id,title,body,assignee_id,due_on,state,inserted_at,updated_at) \
         VALUES($1,$2,$3,$4,$5,$6,'todo',$7,$7)",
    )
    .bind(id)
    .bind(project_id)
    .bind(input.title.trim())
    .bind(input.body.as_deref().map(str::trim))
    .bind(input.assignee_id)
    .bind(input.due_on)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    fetch(pg, id).await
}

/// Assign or reassign a Task. The assignee must be a participant on the
/// Project; `None` clears the assignment. Repeating the current assignee is a
/// no-op that records no event.
pub async fn assign(
    pg: &PostgresDb,
    id: Uuid,
    actor_id: Uuid,
    assignee_id: Option<Uuid>,
) -> Result<ProjectTask, ProjectTaskError> {
    let now = Utc::now();
    let mut tx = pg.0.begin().await?;
    let before = fetch_for_update(&mut tx, id).await?;
    if before.assignee_id == assignee_id {
        return Ok(before);
    }
    if let Some(assignee_id) = assignee_id {
        require_participant(&mut tx, before.project_id, assignee_id).await?;
    }
    set_audit_context(&mut tx, actor_id, "assignee_changed").await?;
    sqlx::query("UPDATE project_task SET assignee_id=$2,updated_at=$3 WHERE id=$1")
        .bind(id)
        .bind(assignee_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    fetch(pg, id).await
}

/// Move a Task one step forward along todo → in_progress → done. The row is
/// retained in every state; nothing deletes it.
pub async fn transition(
    pg: &PostgresDb,
    id: Uuid,
    actor_id: Uuid,
    next: TaskState,
) -> Result<ProjectTask, ProjectTaskError> {
    let now = Utc::now();
    let mut tx = pg.0.begin().await?;
    let before = fetch_for_update(&mut tx, id).await?;
    if !before.state.can_transition_to(next) {
        return Err(ProjectTaskError::Transition {
            from: before.state.as_str(),
            to: next.as_str(),
        });
    }
    set_audit_context(&mut tx, actor_id, "state_changed").await?;
    sqlx::query("UPDATE project_task SET state=$2,updated_at=$3 WHERE id=$1")
        .bind(id)
        .bind(next.as_str())
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    fetch(pg, id).await
}

/// A Project's tasks: open work first by due date, then done work last.
pub async fn list_for_project(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Vec<ProjectTask>, ProjectTaskError> {
    sqlx::query(
        "SELECT id,project_id,title,body,assignee_id,due_on,state,inserted_at,updated_at \
         FROM project_task WHERE project_id=$1 \
         ORDER BY (state='done'), due_on NULLS LAST, id",
    )
    .bind(project_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(from_row)
    .collect()
}

pub async fn find_by_id(pg: &PostgresDb, id: Uuid) -> Result<ProjectTask, ProjectTaskError> {
    fetch(pg, id).await
}

/// Read the immutable audit history of one Project task.
pub async fn list_events(
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Vec<ProjectTaskEvent>, ProjectTaskError> {
    let rows = sqlx::query(
        "SELECT id,actor_person_id,event_kind,before_state,after_state,occurred_at \
         FROM project_task_events WHERE project_task_id=$1 ORDER BY occurred_at,id",
    )
    .bind(id)
    .fetch_all(&pg.0)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(ProjectTaskEvent {
                id: row.try_get("id")?,
                actor_person_id: row.try_get("actor_person_id")?,
                event_kind: row.try_get("event_kind")?,
                before_state: row
                    .try_get::<Option<sqlx::types::Json<serde_json::Value>>, _>("before_state")?
                    .map(|json| json.0),
                after_state: row
                    .try_get::<sqlx::types::Json<serde_json::Value>, _>("after_state")?
                    .0,
                occurred_at: row.try_get("occurred_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}

async fn fetch_for_update(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<ProjectTask, ProjectTaskError> {
    sqlx::query(
        "SELECT id,project_id,title,body,assignee_id,due_on,state,inserted_at,updated_at \
         FROM project_task WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .as_ref()
    .map(from_row)
    .transpose()?
    .ok_or(ProjectTaskError::NotFound)
}

async fn fetch(pg: &PostgresDb, id: Uuid) -> Result<ProjectTask, ProjectTaskError> {
    sqlx::query(
        "SELECT id,project_id,title,body,assignee_id,due_on,state,inserted_at,updated_at \
         FROM project_task WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(from_row)
    .transpose()?
    .ok_or(ProjectTaskError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::{parse_state, validate, NewProjectTask, ProjectTaskError, TaskState};

    fn input() -> NewProjectTask {
        NewProjectTask {
            title: "Serve discovery responses".into(),
            body: None,
            assignee_id: None,
            due_on: None,
        }
    }

    #[test]
    fn accepts_a_titled_task() {
        assert!(validate(&input()).is_ok());
    }

    #[test]
    fn refuses_an_untitled_or_oversized_title() {
        let mut value = input();
        value.title = "   ".into();
        assert!(matches!(
            validate(&value),
            Err(ProjectTaskError::Invalid("title"))
        ));
        let mut value = input();
        value.title = "x".repeat(301);
        assert!(matches!(
            validate(&value),
            Err(ProjectTaskError::Invalid("title"))
        ));
    }

    #[test]
    fn refuses_a_blank_or_oversized_body() {
        let mut value = input();
        value.body = Some("   ".into());
        assert!(matches!(
            validate(&value),
            Err(ProjectTaskError::Invalid("body"))
        ));
        let mut value = input();
        value.body = Some("x".repeat(2001));
        assert!(matches!(
            validate(&value),
            Err(ProjectTaskError::Invalid("body"))
        ));
    }

    #[test]
    fn state_moves_one_step_forward_only() {
        assert!(TaskState::Todo.can_transition_to(TaskState::InProgress));
        assert!(TaskState::InProgress.can_transition_to(TaskState::Done));
        assert!(!TaskState::Todo.can_transition_to(TaskState::Done));
        assert!(!TaskState::Done.can_transition_to(TaskState::InProgress));
        assert!(!TaskState::Todo.can_transition_to(TaskState::Todo));
    }

    #[test]
    fn state_round_trips_through_its_stored_word() {
        for state in [TaskState::Todo, TaskState::InProgress, TaskState::Done] {
            assert!(matches!(parse_state(state.as_str()), Ok(parsed) if parsed == state));
            // The stored word is the wire word: serde speaks the same
            // vocabulary the transition command does.
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::Value::String(state.as_str().into())
            );
        }
        assert!(matches!(
            parse_state("archived"),
            Err(ProjectTaskError::Invalid("state"))
        ));
    }
    // The remaining assertions need a live PostgreSQL: participant-only
    // assignment, transition validation, and the trigger-written audit rows.
    // They share the canonical-schema fixture used across the store crate.
    mod database {
        use super::super::{
            assign, create, list_events, transition, NewProjectTask, ProjectTaskError, TaskState,
        };
        use crate::postgres::PostgresDb;
        use crate::postgres_mirror::tests::{connect, fresh_schema};
        use sqlx::Row;
        use uuid::Uuid;

        struct Seed {
            project_id: Uuid,
            assignee: Uuid,
            outsider: Uuid,
        }

        async fn person(pg: &PostgresDb, name: &str) -> Uuid {
            let unique = Uuid::new_v4().simple().to_string();
            let id: Uuid = sqlx::query(
                "INSERT INTO person (id,name,email,role) VALUES ($1,$2,$3,'lawyer') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("Synthetic {name}"))
            .bind(format!("{unique}@example.com"))
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            id
        }

        async fn seed(pg: &PostgresDb) -> Seed {
            let jurisdiction: Uuid = sqlx::query(
                "INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Synthetic Task Jurisdiction','TSK') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let entity_type: Uuid = sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Task Entity Type') RETURNING id")
                .bind(Uuid::new_v4())
                .fetch_one(pg.pool())
                .await
                .unwrap()
                .get(0);
            let entity: Uuid = sqlx::query(
                "INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Task Entity',$2,$3) RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(entity_type)
            .bind(jurisdiction)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let project_id: Uuid = sqlx::query(
                "INSERT INTO project (id,code,name,status,entity_id) VALUES ($1,$2,'Synthetic Task Matter','open',$3) RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("task-{}", Uuid::new_v4().simple()))
            .bind(entity)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let assignee = person(pg, "Assignee").await;
            let outsider = person(pg, "Outsider").await;
            sqlx::query("INSERT INTO person_project_role (person_id,project_id,participation) VALUES ($1,$2,'lawyer')")
                .bind(assignee)
                .bind(project_id)
                .execute(pg.pool())
                .await
                .unwrap();
            Seed {
                project_id,
                assignee,
                outsider,
            }
        }

        fn task_input(assignee: Option<Uuid>) -> NewProjectTask {
            NewProjectTask {
                title: "Serve discovery responses".into(),
                body: None,
                assignee_id: assignee,
                due_on: None,
            }
        }

        #[tokio::test]
        async fn assignment_requires_a_project_participant() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let actor = person(&pg, "Actor").await;

            // Creating with, or assigning later to, a non-participant is denied.
            assert!(matches!(
                create(
                    &pg,
                    seed.project_id,
                    actor,
                    &task_input(Some(seed.outsider))
                )
                .await,
                Err(ProjectTaskError::AssigneeNotParticipant)
            ));
            let created = create(&pg, seed.project_id, actor, &task_input(None))
                .await
                .unwrap();
            assert_eq!(created.state, TaskState::Todo);
            assert!(matches!(
                assign(&pg, created.id, actor, Some(seed.outsider)).await,
                Err(ProjectTaskError::AssigneeNotParticipant)
            ));

            // A participant is acceptable, at create and by later assignment.
            let assigned = create(
                &pg,
                seed.project_id,
                actor,
                &task_input(Some(seed.assignee)),
            )
            .await
            .unwrap();
            assert_eq!(assigned.assignee_id, Some(seed.assignee));
            let reassigned = assign(&pg, created.id, actor, Some(seed.assignee))
                .await
                .unwrap();
            assert_eq!(reassigned.assignee_id, Some(seed.assignee));
        }

        #[tokio::test]
        async fn transitions_move_one_step_forward_and_done_is_retained() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let actor = person(&pg, "Actor").await;
            let created = create(&pg, seed.project_id, actor, &task_input(None))
                .await
                .unwrap();

            // Skipping the middle state is refused, as is any backward move.
            assert!(matches!(
                transition(&pg, created.id, actor, TaskState::Done).await,
                Err(ProjectTaskError::Transition {
                    from: "todo",
                    to: "done"
                })
            ));
            assert!(matches!(
                transition(&pg, created.id, actor, TaskState::Todo).await,
                Err(ProjectTaskError::Transition {
                    from: "todo",
                    to: "todo"
                })
            ));

            let in_progress = transition(&pg, created.id, actor, TaskState::InProgress)
                .await
                .unwrap();
            assert_eq!(in_progress.state, TaskState::InProgress);
            let done = transition(&pg, created.id, actor, TaskState::Done)
                .await
                .unwrap();
            assert_eq!(done.state, TaskState::Done);

            // Done is a transition, never a delete: the row remains readable.
            let retained = crate::tasks::find_by_id(&pg, created.id).await.unwrap();
            assert_eq!(retained.id, created.id);
            assert_eq!(retained.state, TaskState::Done);
            assert!(matches!(
                transition(&pg, created.id, actor, TaskState::InProgress).await,
                Err(ProjectTaskError::Transition {
                    from: "done",
                    to: "in_progress"
                })
            ));
        }

        #[tokio::test]
        async fn every_mutation_appends_an_audited_event() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let actor = person(&pg, "Actor").await;
            let created = create(&pg, seed.project_id, actor, &task_input(None))
                .await
                .unwrap();
            assign(&pg, created.id, actor, Some(seed.assignee))
                .await
                .unwrap();
            transition(&pg, created.id, actor, TaskState::InProgress)
                .await
                .unwrap();

            let events = list_events(&pg, created.id).await.unwrap();
            let kinds: Vec<&str> = events
                .iter()
                .map(|event| event.event_kind.as_str())
                .collect();
            assert_eq!(kinds, ["created", "assignee_changed", "state_changed"]);
            assert!(
                events[0].before_state.is_none(),
                "creation has no prior state"
            );
            assert_eq!(
                events[0].actor_person_id, actor,
                "every event names its actor"
            );
            assert_eq!(
                events[1].before_state.as_ref().unwrap()["assignee_id"],
                serde_json::Value::Null
            );
            assert_eq!(
                events[1].after_state["assignee_id"],
                serde_json::json!(seed.assignee)
            );
            assert_eq!(
                events[2].before_state.as_ref().unwrap()["state"],
                serde_json::json!("todo")
            );
            assert_eq!(
                events[2].after_state["state"],
                serde_json::json!("in_progress")
            );

            // The database itself refuses a mutation that carries no audit
            // context, so the event log cannot be bypassed.
            let bypass = sqlx::query("UPDATE project_task SET title='Silent edit' WHERE id=$1")
                .bind(created.id)
                .execute(pg.pool())
                .await;
            assert!(bypass.is_err(), "an unaudited update must be refused");

            // The audit log itself is immutable.
            let event_id = events[0].id;
            let tamper = sqlx::query("DELETE FROM project_task_events WHERE id=$1")
                .bind(event_id)
                .execute(pg.pool())
                .await;
            assert!(tamper.is_err(), "audit events must be immutable");
        }
    }
}
