//! `delete_closed_repository` MCP tool.
//!
//! Deletes a closed Project's forge repository once its
//! [`rules::kind::Kind::ClosedRepository`] archive is filed and verified
//! current. This is the deliberate, separate step that follows a matter's
//! close and its archive: it refuses unless the Project is closed, a
//! `closed_repository` document exists for it, that document's recorded
//! commit matches the forge's live HEAD, and an operator has confirmed the
//! provider handoff. Deleting the forge's copy is the one part of this
//! sequence that cannot be undone, so nothing here happens on an assumption.

use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::ToolError;

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "delete_closed_repository",
        "description": "Delete a closed Project's forge repository. Refuses unless the Project is closed, a closed_repository document is filed for it, that document's recorded commit matches the forge's live HEAD, and the operator confirms the provider handoff.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "project_id": {
                    "type": "string",
                    "format": "uuid",
                    "description": "Uuid of the closed Project whose forge repository is being decommissioned."
                },
                "confirm_handoff": {
                    "type": "boolean",
                    "description": "The operator's explicit confirmation that the forge repository is no longer needed and may be permanently deleted. Must be true."
                }
            },
            "required": ["project_id", "confirm_handoff"],
            "additionalProperties": false
        }
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    project_id: Uuid,
    #[serde(default)]
    confirm_handoff: Option<bool>,
}

/// `forge` is `None` on a deployment that has not configured forge
/// credentials — the same optional-dependency shape as
/// [`crate::server::McpState::storage`] and
/// [`crate::server::McpState::email`]. The tool refuses cleanly rather than
/// panicking when it is absent.
pub async fn call(
    application_store: &store::application_store::ApplicationStore,
    forge: Option<&Arc<dyn cloud::ForgeService>>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let pg = store::postgres::PostgresDb::from_env().await.ok();
    call_with(application_store, forge, pg.as_ref(), arguments).await
}

/// [`call`] with the PostgreSQL archive store supplied. Deletion also waits on
/// [`store::project_closeout::readiness`]: a committed recovery generation cut
/// after the close whose verified Git bundle holds the live HEAD, every
/// required handoff acknowledged, and no Legal Hold on the matter.
pub async fn call_with(
    application_store: &store::application_store::ApplicationStore,
    forge: Option<&Arc<dyn cloud::ForgeService>>,
    pg: Option<&store::postgres::PostgresDb>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args: Args = super::decode_args(arguments)?;
    if !args.confirm_handoff.unwrap_or(false) {
        return Err(ToolError::InvalidArguments(
            "deleting a repository requires confirm_handoff=true to affirm the forge copy is no \
             longer needed"
                .into(),
        ));
    }
    let Some(forge) = forge else {
        return Err(ToolError::Internal(
            "no forge is configured on this deployment".into(),
        ));
    };

    let project = store::projects::find_by_id(application_store, args.project_id)
        .await
        .map_err(|error| ToolError::Database(error.to_string()))?
        .ok_or_else(|| ToolError::NotFound(format!("project_id={}", args.project_id)))?;

    if project.status != "closed" {
        return Err(ToolError::InvalidArguments(format!(
            "project_id={} is {}, not closed — close the matter before deleting its repository",
            args.project_id, project.status
        )));
    }

    let asset = store::assets::current(
        application_store,
        args.project_id,
        rules::kind::Kind::ClosedRepository.as_str(),
        store::assets::Lens::Lawyer,
    )
    .await
    .map_err(|error| ToolError::Database(error.to_string()))?
    .ok_or_else(|| {
        ToolError::InvalidArguments(format!(
            "project_id={} has no filed closed_repository document — archive it first",
            args.project_id
        ))
    })?;

    let recorded_sha = asset
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("commit_sha"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ToolError::Internal("the filed closed_repository document carries no commit_sha".into())
        })?;

    let live_sha = forge
        .head_commit_sha(&project.code)
        .await
        .map_err(|error| ToolError::Internal(error.to_string()))?
        .ok_or_else(|| {
            ToolError::NotFound(format!(
                "no live repository found for project_id={}",
                args.project_id
            ))
        })?;

    if recorded_sha != live_sha {
        return Err(ToolError::InvalidArguments(format!(
            "the filed archive records commit {recorded_sha}, but the live repository's HEAD is \
             {live_sha} — the repository moved since the archive was filed; file a fresh \
             closed_repository document before deleting"
        )));
    }

    let readiness = store::project_closeout::readiness(
        application_store,
        pg,
        args.project_id,
        store::project_closeout::Closeout::DeleteRepository {
            live_head: &live_sha,
        },
    )
    .await
    .map_err(ToolError::Database)?;
    if !readiness.is_ready() {
        return Err(ToolError::InvalidArguments(format!(
            "the repository cannot be deleted yet: {}",
            readiness.blockers.join("; ")
        )));
    }

    forge
        .delete_repository(&project.code)
        .await
        .map_err(|error| ToolError::Internal(error.to_string()))?;

    store::projects::set_repository_url(application_store, args.project_id, None)
        .await
        .map_err(|error| ToolError::Internal(error.to_string()))?;

    let summary = format!(
        "Deleted the forge repository for project_id={} ({}) at verified commit {live_sha}.",
        project.id, project.code
    );
    Ok(json!({
        "content": [{ "type": "text", "text": summary }],
        "structuredContent": {
            "id": project.id,
            "code": project.code,
            "deleted_commit_sha": live_sha,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::{call, call_with, descriptor};
    use crate::tools::ToolError;
    use cloud::forge::FakeForge;
    use cloud::{ForgeService, StorageService};
    use serde_json::json;
    use std::sync::Arc;
    use store::documents::{DocumentIdentity, IngestArgs};
    use store::projects::{
        create, transition_project_with_reason, ClosureReason, NewProject, Transition,
    };
    use store::test_support::{dri_person, seed_entity, seed_signed_onboarding};
    use uuid::Uuid;

    async fn storage() -> Arc<dyn StorageService> {
        let dir = std::env::temp_dir().join(format!(
            "navigator-mcp-delete-closed-repo-{}",
            Uuid::now_v7()
        ));
        Arc::new(cloud::FsStorage::new(dir).await.unwrap())
    }

    async fn closed_project(
        application_store: &store::application_store::ApplicationStore,
    ) -> (Uuid, String) {
        let code = format!("closed-repo-{}", Uuid::now_v7().simple());
        crate::test_fixtures::seed_brands(application_store).await;
        let created = create(
            application_store,
            &NewProject {
                code: code.clone(),
                name: "Decommissionable matter".into(),
                status: "open".into(),
                entity_id: seed_entity(application_store).await,
                ..Default::default()
            },
        )
        .await
        .expect("create project");
        seed_signed_onboarding(
            application_store,
            created.id,
            dri_person(application_store).await,
        )
        .await;
        transition_project_with_reason(
            application_store,
            created.id,
            Transition::Close,
            Some(ClosureReason::EngagementCompleted),
            None,
        )
        .await
        .expect("close project");
        (created.id, code)
    }

    async fn file_archive(
        application_store: &store::application_store::ApplicationStore,
        storage: &Arc<dyn StorageService>,
        project_id: Uuid,
        commit_sha: &str,
    ) {
        let kind = rules::kind::Kind::ClosedRepository.as_str();
        store::documents::ingest_bytes_as(
            application_store,
            storage,
            &IngestArgs {
                project_id,
                source: "generated",
                filename: "repository.zip",
                kind,
                content_type: "application/zip",
                description: None,
                secondary_storage_key: None,
                visibility: store::documents::visibility::INTERNAL,
            },
            &DocumentIdentity {
                slug: Some(kind),
                published_at: None,
                metadata: Some(json!({ "commit_sha": commit_sha })),
                derived_from: None,
                transcript_quality: None,
                signing: None,
                notarization: Vec::new(),
                evidence: None,
                folder: None,
            },
            b"a zip archive's bytes",
        )
        .await
        .expect("file closed_repository asset");
    }

    #[test]
    fn descriptor_requires_project_and_confirmation() {
        let d = descriptor();
        assert_eq!(d["name"], "delete_closed_repository");
        assert_eq!(
            d["inputSchema"]["required"],
            json!(["project_id", "confirm_handoff"])
        );
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
    }

    #[tokio::test]
    async fn a_matching_archive_without_a_verified_recovery_record_is_not_enough() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let (project_id, code) = closed_project(&application_store).await;
            file_archive(
                &application_store,
                &storage().await,
                project_id,
                "deadbeefcafe",
            )
            .await;

            let forge = FakeForge::new();
            forge.ensure_repository(&code).await.unwrap();
            forge.set_head_commit_sha(&code, "deadbeefcafe");
            let forge: Arc<dyn ForgeService> = Arc::new(forge);

            let error = call_with(
                &application_store,
                Some(&forge),
                None,
                &json!({ "project_id": project_id, "confirm_handoff": true }),
            )
            .await
            .expect_err("deletion waits on the recovery archive store");
            let ToolError::InvalidArguments(message) = error else {
                panic!("expected a readiness refusal, got {error:?}");
            };
            assert!(message.contains("recovery archive store"), "{message}");
            assert!(forge.find_repository(&code).await.unwrap().is_some());
        })
        .await;
    }

    #[tokio::test]
    async fn refuses_when_the_recorded_commit_does_not_match_live_head() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let (project_id, code) = closed_project(&application_store).await;
            file_archive(
                &application_store,
                &storage().await,
                project_id,
                "deadbeefcafe",
            )
            .await;

            let forge = FakeForge::new();
            forge.ensure_repository(&code).await.unwrap();
            forge.set_head_commit_sha(&code, "0000000000000");
            let forge: Arc<dyn ForgeService> = Arc::new(forge);

            let error = call(
                &application_store,
                Some(&forge),
                &json!({ "project_id": project_id, "confirm_handoff": true }),
            )
            .await
            .expect_err("mismatched commit must be refused");
            assert!(matches!(error, ToolError::InvalidArguments(_)));
            assert!(forge.find_repository(&code).await.unwrap().is_some());
        })
        .await;
    }

    #[tokio::test]
    async fn refuses_an_open_project() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let code = format!("open-repo-{}", Uuid::now_v7().simple());
            crate::test_fixtures::seed_brands(&application_store).await;
            let created = create(
                &application_store,
                &NewProject {
                    code: code.clone(),
                    name: "Still-open matter".into(),
                    status: "open".into(),
                    entity_id: seed_entity(&application_store).await,
                    ..Default::default()
                },
            )
            .await
            .expect("create project");

            let forge: Arc<dyn ForgeService> = Arc::new(FakeForge::new());
            let error = call(
                &application_store,
                Some(&forge),
                &json!({ "project_id": created.id, "confirm_handoff": true }),
            )
            .await
            .expect_err("open project must be refused");
            assert!(matches!(error, ToolError::InvalidArguments(_)));
        })
        .await;
    }

    #[tokio::test]
    async fn refuses_without_a_filed_archive() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let (project_id, _code) = closed_project(&application_store).await;

            let forge: Arc<dyn ForgeService> = Arc::new(FakeForge::new());
            let error = call(
                &application_store,
                Some(&forge),
                &json!({ "project_id": project_id, "confirm_handoff": true }),
            )
            .await
            .expect_err("missing archive must be refused");
            assert!(matches!(error, ToolError::InvalidArguments(_)));
        })
        .await;
    }

    #[tokio::test]
    async fn requires_confirmation_and_a_configured_forge() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let (project_id, _code) = closed_project(&application_store).await;

            let error = call(&application_store, None, &json!({"project_id": project_id}))
                .await
                .expect_err("missing confirm_handoff must be rejected");
            assert!(matches!(error, ToolError::InvalidArguments(_)));

            let error = call(
                &application_store,
                None,
                &json!({"project_id": project_id, "confirm_handoff": true}),
            )
            .await
            .expect_err("an unconfigured forge must be rejected");
            assert!(matches!(error, ToolError::Internal(_)));
        })
        .await;
    }
}
