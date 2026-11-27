//! `close_project` MCP tool.
//!
//! Closes an existing Project through the shared lifecycle command. The
//! attestation is explicit because closing is a firm-policy decision, and the
//! store derives the coupled `closed_at` from the transition and optional
//! effective time.

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::ToolError;

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "close_project",
        "description": "Close an existing Project (matter) in Neon Law Navigator. This lifecycle transition records why the matter closed and stamps closed_at for retention. The closing attorney must explicitly attest that the matter is ready to close.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": {
                    "type": "string",
                    "format": "uuid",
                    "description": "Uuid of the existing Project to close."
                },
                "attestation": {
                    "type": "boolean",
                    "description": "The closing attorney's explicit attestation that the matter is ready to close. Must be true."
                },
                "reason": {
                    "type": "string",
                    "enum": ["pitch_declined", "pitch_lapsed", "pitch_withdrawn", "pitch_superseded", "engagement_completed", "client_terminated", "firm_withdrew"],
                    "description": "Why the matter closed. The allowed values depend on whether onboarding was on file before the close."
                },
                "effective_at": {
                    "type": "string",
                    "format": "date-time",
                    "description": "Optional RFC 3339 time when the matter actually closed. Must not be in the future or precede matter-open."
                }
            },
            "required": ["project_id", "attestation", "reason"],
            "additionalProperties": false
        }
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    project_id: Uuid,
    #[serde(default)]
    attestation: Option<bool>,
    reason: store::projects::ClosureReason,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn call(
    application_store: &store::application_store::ApplicationStore,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args: Args = super::decode_args(arguments)?;
    if !args.attestation.unwrap_or(false) {
        return Err(ToolError::InvalidArguments(
            "this matter close requires attestation — pass attestation=true to affirm the matter is ready to close".into(),
        ));
    }

    let closed = store::projects::transition_project_with_reason(
        application_store,
        args.project_id,
        store::projects::Transition::Close,
        Some(args.reason),
        args.effective_at,
    )
    .await
    .map_err(|error| match error {
        store::projects::ProjectCommandError::NotFound => {
            ToolError::NotFound(format!("project_id={}", args.project_id))
        }
        store::projects::ProjectCommandError::Invalid(message) => {
            ToolError::InvalidArguments(message.into())
        }
        store::projects::ProjectCommandError::Db(message) => ToolError::Database(message),
        store::projects::ProjectCommandError::Referenced(detail) => ToolError::Internal(detail),
    })?;

    let summary = format!(
        "Closed project id={} ({}, status={}, closed_at={}).",
        closed.id,
        closed.name,
        closed.status,
        closed.closed_at.as_deref().unwrap_or("unknown")
    );
    Ok(json!({
        "content": [{ "type": "text", "text": summary }],
        "structuredContent": {
            "id": closed.id,
            "name": closed.name,
            "status": closed.status,
            "closed_at": closed.closed_at,
            "closure_reason": closed.closure_reason,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{call, descriptor};
    use crate::tools::ToolError;
    use serde_json::json;
    use store::projects::{create, NewProject};
    use store::test_support::seed_entity;
    use uuid::Uuid;

    async fn project(application_store: &store::application_store::ApplicationStore) -> Uuid {
        crate::test_fixtures::seed_brands(application_store).await;
        let id = create(
            application_store,
            &NewProject {
                code: format!("close-{}", Uuid::now_v7().simple()),
                name: "Closeable matter".into(),
                status: "open".into(),
                entity_id: seed_entity(application_store).await,
                ..Default::default()
            },
        )
        .await
        .expect("create project")
        .id;
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        sqlx::query(
            "INSERT INTO asset(id,project_id,storage_key,content_type,byte_size,sha256_hex,kind,visibility) \
             VALUES ($1,$2,$3,'application/pdf',1,$4,'offboarding','internal')",
        )
        .bind(Uuid::new_v4())
        .bind(id)
        .bind(format!("projects/{id}/documents/test"))
        .bind(store::assets::sha256_hex(format!("synthetic-{id}").as_bytes()))
        .execute(pg.pool())
        .await
        .expect("offboarding asset");
        id
    }

    #[test]
    fn descriptor_requires_project_attestation_and_reason() {
        let d = descriptor();
        assert_eq!(d["name"], "close_project");
        assert_eq!(
            d["inputSchema"]["required"],
            json!(["project_id", "attestation", "reason"])
        );
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            d["inputSchema"]["properties"]["effective_at"]["format"],
            "date-time"
        );
    }

    #[tokio::test]
    async fn closes_project_and_stamps_date() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let id = project(&application_store).await;
            let effective_at = chrono::Utc::now();

            let result = call(
                &application_store,
                &json!({
                    "project_id": id,
                    "attestation": true,
                    "reason": "pitch_declined",
                    "effective_at": effective_at,
                }),
            )
            .await
            .expect("close project");

            assert_eq!(result["structuredContent"]["status"], "closed");
            assert_eq!(
                result["structuredContent"]["closure_reason"],
                "pitch_declined"
            );
            let closed_at: chrono::DateTime<chrono::Utc> = result["structuredContent"]["closed_at"]
                .as_str()
                .expect("close time")
                .parse()
                .expect("RFC 3339 close time");
            assert_eq!(closed_at, effective_at);
        })
        .await;
    }

    #[tokio::test]
    async fn requires_attestation_and_rejects_unknown_fields() {
        store::test_support::with_postgres(async {
        let application_store = crate::test_fixtures::handle();
        let id = project(&application_store).await;

        let error = call(&application_store, &json!({"project_id": id}))
            .await
            .expect_err("missing attestation must be rejected");
        assert!(matches!(error, ToolError::InvalidArguments(_)));

        let error = call(
            &application_store,
            &json!({"project_id": id, "attestation": true, "reason": "pitch_declined", "closed_at": "now"}),
        )
        .await
        .expect_err("unknown fields must be rejected");
        assert!(matches!(error, ToolError::InvalidArguments(_)));
    }).await;
    }
}
