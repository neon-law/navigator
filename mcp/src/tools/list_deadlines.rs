//! `list_deadlines` MCP tool.

use serde_json::{json, Value};

use super::{ReadScope, ToolError};

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "list_deadlines",
        "description": "List the deadlines on Projects (matters) the signed-in caller \
                        participates in. Each deadline includes its due date, kind, status, \
                        statute, and source so the date can be traced to its authority and \
                        producing workflow. Takes no arguments.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }
    })
}

pub async fn call(
    application_store: &store::application_store::ApplicationStore,
    scope: &ReadScope,
    _arguments: &Value,
) -> Result<Value, ToolError> {
    let projects = match scope {
        ReadScope::Membership { person_id, role } => super::portal_access()
            .visible_projects(application_store, Some(*person_id), *role)
            .await
            .map_err(ToolError::Database)?,
        // A directory is deliberately metadata-only, and no identity cannot
        // establish a participation boundary. Neither scope may read matter
        // contents through this tool.
        ReadScope::Deployment | ReadScope::Directory { .. } | ReadScope::Unlinked => Vec::new(),
    };

    let mut deadlines = Vec::new();
    for project in projects {
        deadlines.extend(
            store::statutory_deadlines::by_project(application_store, project.id)
                .await
                .map_err(|error| ToolError::Database(error.to_string()))?,
        );
    }

    let rows: Vec<Value> = deadlines
        .iter()
        .map(|deadline| {
            json!({
                "id": deadline.id,
                "project_id": deadline.project_id,
                "kind": deadline.kind,
                "trigger_on": deadline.trigger_on,
                "due_on": deadline.due_on,
                "statute": deadline.statute,
                "source": deadline.source,
                "status": deadline.status,
                "inserted_at": deadline.inserted_at,
                "updated_at": deadline.updated_at,
            })
        })
        .collect();

    let summary = if deadlines.is_empty() {
        "No deadlines you participate in.".to_string()
    } else {
        let listed = deadlines
            .iter()
            .map(|deadline| {
                format!(
                    "project {}: {} due {} (statute: {}; source: {}; status: {})",
                    deadline.project_id,
                    deadline.kind,
                    deadline.due_on,
                    deadline.statute,
                    deadline.source,
                    deadline.status,
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} deadlines: {listed}.", deadlines.len())
    };

    Ok(json!({
        "content": [{ "type": "text", "text": summary }],
        "structuredContent": {
            "count": rows.len(),
            "deadlines": rows,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{call, descriptor};
    use crate::tools::ReadScope;
    use chrono::NaiveDate;
    use serde_json::json;
    use uuid::Uuid;

    async fn project(
        application_store: &store::application_store::ApplicationStore,
        name: &str,
    ) -> Uuid {
        crate::test_fixtures::seed_brands(application_store).await;
        store::projects::create(
            application_store,
            &store::projects::NewProject {
                code: format!("deadline-{}", Uuid::now_v7()),
                name: name.into(),
                status: "open".into(),
                entity_id: store::test_support::seed_entity(application_store).await,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn lawyer(
        application_store: &store::application_store::ApplicationStore,
        project_id: Option<Uuid>,
    ) -> store::persons::Person {
        let person = store::persons::create(
            application_store,
            &store::persons::NewPerson::with_role(
                format!("Lawyer {project_id:?}"),
                format!("lawyer-{}@example.com", Uuid::now_v7()),
                store::persons::Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        if let Some(project_id) = project_id {
            store::participation::add_participant(
                application_store,
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
        person
    }

    async fn deadline(
        application_store: &store::application_store::ApplicationStore,
        project_id: Uuid,
        source: &str,
    ) {
        store::statutory_deadlines::record(
            application_store,
            &store::statutory_deadlines::NewStatutoryDeadline {
                project_id,
                kind: "filing",
                trigger_on: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                due_on: NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
                statute: "15 U.S.C. § 1681i(a)(1)",
                source,
            },
        )
        .await
        .unwrap();
    }

    #[test]
    fn descriptor_names_the_tool_and_takes_no_arguments() {
        let d = descriptor();
        assert_eq!(d["name"], "list_deadlines");
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
        assert!(d["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn participating_lawyer_receives_only_their_deadlines_with_authority_and_source() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let permitted = project(&application_store, "Permitted Matter").await;
            let denied = project(&application_store, "Denied Matter").await;
            deadline(&application_store, permitted, "triage:permitted").await;
            deadline(&application_store, denied, "triage:denied").await;
            let person = lawyer(&application_store, Some(permitted)).await;

            let result = call(
                &application_store,
                &ReadScope::Membership {
                    person_id: person.id,
                    role: person.role,
                },
                &json!({}),
            )
            .await
            .unwrap();

            assert_eq!(result["structuredContent"]["count"], 1);
            let row = &result["structuredContent"]["deadlines"][0];
            assert_eq!(row["project_id"], permitted.to_string());
            assert_eq!(row["statute"], "15 U.S.C. § 1681i(a)(1)");
            assert_eq!(row["source"], "triage:permitted");
            let text = result["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("15 U.S.C. § 1681i(a)(1)"));
            assert!(text.contains("triage:permitted"));
            assert!(!text.contains("triage:denied"));
        })
        .await;
    }

    #[tokio::test]
    async fn lawyer_without_participation_receives_no_deadlines() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let project_id = project(&application_store, "Private Matter").await;
            deadline(&application_store, project_id, "triage:private").await;
            let person = lawyer(&application_store, None).await;

            let result = call(
                &application_store,
                &ReadScope::Membership {
                    person_id: person.id,
                    role: person.role,
                },
                &json!({}),
            )
            .await
            .unwrap();

            assert_eq!(result["structuredContent"]["count"], 0);
            assert!(result["structuredContent"]["deadlines"]
                .as_array()
                .unwrap()
                .is_empty());
            assert_eq!(
                result["content"][0]["text"],
                "No deadlines you participate in."
            );
        })
        .await;
    }
}
