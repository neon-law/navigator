//! Project Deadline MCP tools.
//!
//! `list_deadlines` stays on the statutory workflow clock. These tools read
//! and write `project_deadlines`. A caller who does not participate in the
//! Project is refused. Each write returns the row the store read back.

use serde::Deserialize;
use serde_json::{json, Value};
use store::persons::Role;
use store::project_deadlines::{Deadline, DeadlineError};
use uuid::Uuid;

use super::ToolError;

#[must_use]
pub fn list_descriptor() -> Value {
    json!({
        "name": "list_project_deadlines",
        "description": "List the deadlines stored on one Project the caller participates in. \
                        Each row includes its title, stored due instant, authority, calendar role, and status.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": { "type": "string", "format": "uuid" }
            },
            "required": ["project_id"],
            "additionalProperties": false
        }
    })
}

#[must_use]
pub fn create_descriptor() -> Value {
    json!({
        "name": "create_deadline",
        "description": "Store a deadline on a Project the caller participates in and return the written row. \
                        due_at is an RFC 3339 timestamp with an offset. A statute or court_rule requires an authority citation. \
                        Dates are stored as given; court days are not computed.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": { "type": "string", "format": "uuid" },
                "description": { "type": "string" },
                "title": { "type": "string", "maxLength": 200, "description": "A short name for what is due, such as the pleading or response." },
                "due_at": { "type": "string" },
                "authority_kind": { "type": "string" },
                "authority": { "type": "string" },
                "source": { "type": "string" },
                "source_document_id": { "type": "string", "format": "uuid" },
                "owner_person_id": { "type": "string", "format": "uuid" },
                "calendar_role": { "type": "string" },
                "internal_lead_days": { "type": "integer" },
                "client_lead_days": { "type": "integer" },
                "replay_key": { "type": "string" }
            },
            "required": ["project_id", "due_at", "authority_kind", "calendar_role"],
            "additionalProperties": false
        }
    })
}

#[must_use]
pub fn update_descriptor() -> Value {
    json!({
        "name": "update_deadline",
        "description": "Change a stored deadline and return the written row. Status changes only through complete_deadline, cancel_deadline, and reopen_deadline.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": { "type": "string", "format": "uuid" },
                "deadline_id": { "type": "string", "format": "uuid" },
                "description": { "type": "string" },
                "title": { "type": "string", "maxLength": 200, "description": "An empty string clears the title." },
                "due_at": { "type": "string" },
                "authority_kind": { "type": "string" },
                "authority": { "type": "string" },
                "source": { "type": "string" },
                "source_document_id": { "type": ["string", "null"], "format": "uuid" },
                "owner_person_id": { "type": ["string", "null"], "format": "uuid" },
                "calendar_role": { "type": "string" },
                "internal_lead_days": { "type": ["integer", "null"] },
                "client_lead_days": { "type": ["integer", "null"] }
            },
            "required": ["project_id", "deadline_id"],
            "additionalProperties": false
        }
    })
}

#[must_use]
pub fn complete_descriptor() -> Value {
    transition_descriptor(
        "complete_deadline",
        "Set a deadline's status to complete and return the stored row. This is not a delete.",
    )
}

#[must_use]
pub fn cancel_descriptor() -> Value {
    transition_descriptor(
        "cancel_deadline",
        "Set a deadline's status to cancelled and return the stored row. This is not a delete. Use reopen_deadline to restore it.",
    )
}

#[must_use]
pub fn reopen_descriptor() -> Value {
    transition_descriptor(
        "reopen_deadline",
        "Restore a cancelled deadline to open and return the same stored row. An open row is unchanged; a complete row is refused.",
    )
}

fn transition_descriptor(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": { "type": "string", "format": "uuid" },
                "deadline_id": { "type": "string", "format": "uuid" }
            },
            "required": ["project_id", "deadline_id"],
            "additionalProperties": false
        }
    })
}

#[derive(Deserialize)]
struct ProjectArgs {
    project_id: Uuid,
}

#[derive(Deserialize)]
struct CreateArgs {
    project_id: Uuid,
    title: Option<String>,
    description: Option<String>,
    due_at: String,
    authority_kind: String,
    #[serde(default)]
    authority: String,
    #[serde(default)]
    source: String,
    source_document_id: Option<Uuid>,
    owner_person_id: Option<Uuid>,
    calendar_role: String,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<String>,
}

#[derive(Deserialize)]
#[allow(clippy::option_option)]
struct UpdateArgs {
    project_id: Uuid,
    deadline_id: Uuid,
    title: Option<String>,
    description: Option<String>,
    due_at: Option<String>,
    authority_kind: Option<String>,
    authority: Option<String>,
    source: Option<String>,
    source_document_id: Option<Option<Uuid>>,
    owner_person_id: Option<Option<Uuid>>,
    calendar_role: Option<String>,
    internal_lead_days: Option<Option<i64>>,
    client_lead_days: Option<Option<i64>>,
}

#[derive(Deserialize)]
struct IdArgs {
    project_id: Uuid,
    deadline_id: Uuid,
}

async fn require_participant(
    surreal: &store::surreal::SurrealDb,
    person_id: Uuid,
    role: Role,
    project_id: Uuid,
) -> Result<(), ToolError> {
    let allowed = store::access::can_see_project(surreal, Some(person_id), role, project_id)
        .await
        .map_err(ToolError::Database)?;
    if allowed {
        Ok(())
    } else {
        Err(ToolError::NotFound(format!("project_id={project_id}")))
    }
}

fn map_deadline(error: DeadlineError) -> ToolError {
    match error {
        DeadlineError::Invalid(message) => ToolError::InvalidArguments(message),
        DeadlineError::NotFound => ToolError::NotFound("deadline".into()),
        other => ToolError::Database(other.to_string()),
    }
}

fn row_result(deadline: &Deadline, summary: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": summary }],
        "structuredContent": { "deadline": deadline }
    })
}

pub async fn list_call(
    surreal: &store::surreal::SurrealDb,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args: ProjectArgs = super::decode_args(arguments)?;
    require_participant(surreal, person_id, role, args.project_id).await?;
    let deadlines = store::project_deadlines::list(surreal, args.project_id)
        .await
        .map_err(map_deadline)?;
    Ok(json!({
        "content": [{
            "type": "text",
            "text": format!("{} deadlines on project {}.", deadlines.len(), args.project_id)
        }],
        "structuredContent": {
            "count": deadlines.len(),
            "deadlines": deadlines
        }
    }))
}

pub async fn create_call(
    surreal: &store::surreal::SurrealDb,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args: CreateArgs = super::decode_args(arguments)?;
    require_participant(surreal, person_id, role, args.project_id).await?;
    let deadline = store::project_deadlines::create(
        surreal,
        &store::project_deadlines::NewDeadline {
            project_id: args.project_id,
            title: args.title,
            description: args.description,
            due_at: args.due_at,
            authority_kind: args.authority_kind,
            authority: args.authority,
            source: args.source,
            source_document_id: args.source_document_id,
            owner_person_id: args.owner_person_id,
            calendar_role: args.calendar_role,
            internal_lead_days: args.internal_lead_days,
            client_lead_days: args.client_lead_days,
            replay_key: args.replay_key,
        },
    )
    .await
    .map_err(map_deadline)?;
    Ok(row_result(
        &deadline,
        &format!("Stored deadline {} due {}.", deadline.id, deadline.due_at),
    ))
}

pub async fn update_call(
    surreal: &store::surreal::SurrealDb,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args: UpdateArgs = super::decode_args(arguments)?;
    require_participant(surreal, person_id, role, args.project_id).await?;
    let deadline = store::project_deadlines::update(
        surreal,
        args.project_id,
        args.deadline_id,
        &store::project_deadlines::DeadlinePatch {
            title: args.title.map(Some),
            description: args.description.map(Some),
            due_at: args.due_at,
            authority_kind: args.authority_kind,
            authority: args.authority,
            source: args.source,
            source_document_id: args.source_document_id,
            owner_person_id: args.owner_person_id,
            calendar_role: args.calendar_role,
            internal_lead_days: args.internal_lead_days,
            client_lead_days: args.client_lead_days,
        },
    )
    .await
    .map_err(map_deadline)?;
    Ok(row_result(
        &deadline,
        &format!("Updated deadline {}.", deadline.id),
    ))
}

pub async fn complete_call(
    surreal: &store::surreal::SurrealDb,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    transition(surreal, person_id, role, arguments, Transition::Complete).await
}

pub async fn cancel_call(
    surreal: &store::surreal::SurrealDb,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    transition(surreal, person_id, role, arguments, Transition::Cancel).await
}

pub async fn reopen_call(
    surreal: &store::surreal::SurrealDb,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
) -> Result<Value, ToolError> {
    transition(surreal, person_id, role, arguments, Transition::Reopen).await
}

#[derive(Clone, Copy)]
enum Transition {
    Complete,
    Cancel,
    Reopen,
}

async fn transition(
    surreal: &store::surreal::SurrealDb,
    person_id: Uuid,
    role: Role,
    arguments: &Value,
    which: Transition,
) -> Result<Value, ToolError> {
    let args: IdArgs = super::decode_args(arguments)?;
    require_participant(surreal, person_id, role, args.project_id).await?;
    let deadline = match which {
        Transition::Complete => {
            store::project_deadlines::complete(surreal, args.project_id, args.deadline_id).await
        }
        Transition::Cancel => {
            store::project_deadlines::cancel(surreal, args.project_id, args.deadline_id).await
        }
        Transition::Reopen => {
            store::project_deadlines::reopen(surreal, args.project_id, args.deadline_id).await
        }
    }
    .map_err(map_deadline)?;
    Ok(row_result(
        &deadline,
        &format!("Deadline {} is {}.", deadline.id, deadline.status),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::persons::{NewPerson, Role};
    use store::test_support::mem_surreal;
    use uuid::Uuid;

    async fn project(surreal: &store::surreal::SurrealDb) -> Uuid {
        store::projects::create(
            surreal,
            &store::projects::NewProject {
                code: format!("deadline-{}", Uuid::now_v7()),
                name: "Deadline matter".into(),
                status: "open".into(),
                entity_id: Uuid::now_v7(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn lawyer(surreal: &store::surreal::SurrealDb, project_id: Option<Uuid>) -> Uuid {
        let person = store::persons::create(
            surreal,
            &NewPerson::with_role(
                "Synthetic Lawyer",
                format!("lawyer-{}@example.com", Uuid::now_v7()),
                Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        if let Some(project_id) = project_id {
            store::participation::add_participant(
                surreal,
                &store::participation::AddParticipantCommand {
                    project_id,
                    person_id: person.id,
                    dri: store::participation::DriRequest::Unchanged,
                    actor: store::participation::DriActor::System,
                },
            )
            .await
            .unwrap();
        }
        person.id
    }

    fn create_args(project_id: Uuid, authority: &str) -> Value {
        json!({
            "project_id": project_id,
            "due_at": "2026-11-02T17:00:00-08:00",
            "authority_kind": "statute",
            "authority": authority,
            "calendar_role": "firm",
            "internal_lead_days": 14,
            "client_lead_days": 7
        })
    }

    #[tokio::test]
    async fn create_list_update_and_complete_round_trip() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let person_id = lawyer(&surreal, Some(project_id)).await;
        let created = create_call(
            &surreal,
            person_id,
            Role::Lawyer,
            &create_args(project_id, "15 U.S.C. § 1"),
        )
        .await
        .unwrap();
        let deadline_id = created["structuredContent"]["deadline"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(created["structuredContent"]["deadline"]["status"], "open");
        let listed = list_call(
            &surreal,
            person_id,
            Role::Lawyer,
            &json!({ "project_id": project_id }),
        )
        .await
        .unwrap();
        assert_eq!(listed["structuredContent"]["count"], 1);
        let updated = update_call(
            &surreal,
            person_id,
            Role::Lawyer,
            &json!({
                "project_id": project_id,
                "deadline_id": deadline_id,
                "calendar_role": "hearing"
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            updated["structuredContent"]["deadline"]["calendar_role"],
            "hearing"
        );
        let completed = complete_call(
            &surreal,
            person_id,
            Role::Lawyer,
            &json!({ "project_id": project_id, "deadline_id": deadline_id }),
        )
        .await
        .unwrap();
        assert_eq!(
            completed["structuredContent"]["deadline"]["status"],
            "complete"
        );
    }

    #[tokio::test]
    async fn a_title_is_stored_and_changed() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let person_id = lawyer(&surreal, Some(project_id)).await;
        let mut args = create_args(project_id, "15 U.S.C. § 1");
        args["title"] = json!("Answer to the complaint");
        let created = create_call(&surreal, person_id, Role::Lawyer, &args)
            .await
            .unwrap();
        let deadline = &created["structuredContent"]["deadline"];
        assert_eq!(deadline["title"], "Answer to the complaint");
        let updated = update_call(
            &surreal,
            person_id,
            Role::Lawyer,
            &json!({
                "project_id": project_id,
                "deadline_id": deadline["id"],
                "title": "Reply brief"
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            updated["structuredContent"]["deadline"]["title"],
            "Reply brief"
        );
    }

    #[tokio::test]
    async fn a_cancelled_deadline_can_be_reopened() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let person_id = lawyer(&surreal, Some(project_id)).await;
        let created = create_call(
            &surreal,
            person_id,
            Role::Lawyer,
            &create_args(project_id, "15 U.S.C. § 1"),
        )
        .await
        .unwrap();
        let deadline_id = created["structuredContent"]["deadline"]["id"].clone();
        let ids = json!({ "project_id": project_id, "deadline_id": deadline_id });
        let cancelled = cancel_call(&surreal, person_id, Role::Lawyer, &ids)
            .await
            .unwrap();
        assert_eq!(
            cancelled["structuredContent"]["deadline"]["status"],
            "cancelled"
        );
        let reopened = reopen_call(&surreal, person_id, Role::Lawyer, &ids)
            .await
            .unwrap();
        assert_eq!(reopened["structuredContent"]["deadline"]["status"], "open");
        assert_eq!(reopened["structuredContent"]["deadline"]["id"], deadline_id);
    }

    #[test]
    fn descriptors_point_cancelled_rows_at_reopen() {
        assert_eq!(reopen_descriptor()["name"], "reopen_deadline");
        for d in [update_descriptor(), cancel_descriptor()] {
            assert!(d["description"]
                .as_str()
                .unwrap()
                .contains("reopen_deadline"));
        }
    }

    #[tokio::test]
    async fn a_statute_without_a_citation_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let person_id = lawyer(&surreal, Some(project_id)).await;
        let error = create_call(
            &surreal,
            person_id,
            Role::Lawyer,
            &create_args(project_id, "  "),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)), "{error}");
    }

    #[tokio::test]
    async fn a_non_participant_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let person_id = lawyer(&surreal, None).await;
        let error = create_call(
            &surreal,
            person_id,
            Role::Lawyer,
            &create_args(project_id, "15 U.S.C. § 1"),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ToolError::NotFound(_)), "{error}");
    }
}
