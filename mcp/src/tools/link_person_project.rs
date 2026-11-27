//! `link_person_project` MCP tool.
//!
//! Binds a Person to a Project. The tool names no role: participation
//! follows the person's `persons.role`, so there is no word for a model to
//! invent or get wrong. The `(person_id, project_id)` pair is the assignment
//! key, and the call is idempotent — linking someone already on the matter
//! returns the existing row.
//!
//! See [`Person–Project Role`].
//!
//! [`Person–Project Role`]: ../../../docs/glossary/personproject-role.md

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::ToolError;

#[must_use]
pub fn descriptor() -> Value {
    json!({
        "name": "link_person_project",
        "description":
            "Bind a Person to a Project. The matter-side participation follows the person's \
             system tier and is not an input. Idempotent: re-linking someone already on the \
             matter returns the existing assignment. Returns the link id.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "person_id": {
                    "type": "string",
                    "format": "uuid",
                    "description": "Existing persons.id."
                },
                "project_id": {
                    "type": "string",
                    "format": "uuid",
                    "description": "Existing projects.id."
                }
            },
            "required": ["person_id", "project_id"],
            "additionalProperties": false
        }
    })
}

#[derive(Debug, Deserialize)]
struct Args {
    person_id: Uuid,
    project_id: Uuid,
}

/// An authenticated Admin may assign participants to matters in the Firms they administer.
pub(super) async fn require_admin_scope(
    application_store: &store::application_store::ApplicationStore,
    identity: super::Identity,
    arguments: &Value,
) -> Result<(), ToolError> {
    if identity.role != store::persons::Role::Admin {
        return Ok(());
    }
    let args: Args = super::decode_args(arguments)?;
    let project = store::projects::find_by_id(application_store, args.project_id)
        .await
        .map_err(|error| ToolError::Database(error.to_string()))?
        .ok_or_else(|| ToolError::NotFound("project".into()))?;
    if !store::projects::can_administer_project(
        application_store,
        Some(identity.person_id),
        identity.role,
        &project,
    )
    .await
    .map_err(|error| ToolError::Database(error.to_string()))?
    {
        return Err(ToolError::NotFound("project".into()));
    }
    Ok(())
}

pub async fn call(
    application_store: &store::application_store::ApplicationStore,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let args: Args = super::decode_args(arguments)?;

    if store::persons::find_by_id(application_store, args.person_id)
        .await?
        .is_none()
    {
        return Err(ToolError::NotFound(format!("person_id={}", args.person_id)));
    }
    if store::projects::find_by_id(application_store, args.project_id)
        .await
        .map_err(|error| ToolError::Database(error.to_string()))?
        .is_none()
    {
        return Err(ToolError::NotFound(format!(
            "project_id={}",
            args.project_id
        )));
    }

    let existing = store::projects::participation_for_person(
        application_store,
        args.person_id,
        args.project_id,
    )
    .await
    .map_err(|error| ToolError::Database(error.to_string()))?;

    // Route the write through the shared participation commands so this tool
    // honours the same invariants as the REST door and the lawyer form — one
    // row per (person, project), the derived participation, and the
    // lawyer-DRI-lockout guard — instead of writing `person_project_role`
    // directly. The command boundary is the point of #355.
    let (row, created) = if let Some(row) = existing {
        (row, false)
    } else {
        let inserted = store::participation::add_participant(
            application_store,
            &store::participation::AddParticipantCommand {
                project_id: args.project_id,
                person_id: args.person_id,
                dri: store::participation::DriRequest::Unchanged,
                // Linking a person never moves accountability, so this tool
                // names no DRI actor — there is no marker change to attribute.
                actor: store::participation::DriActor::System,
            },
        )
        .await
        .map_err(map_add_error)?;
        (inserted, true)
    };

    // `participation` and the two DRI markers are three separate fields
    // because they are three separate facts: the participation is the
    // person's tier on the matter, and a DRI marker is accountability
    // riding the same row. Flattening them into one enum would teach the
    // model a vocabulary `person_project_roles` does not have.
    let verb = if created { "Linked" } else { "Already linked" };
    let dri = match (row.is_lawyer_dri, row.is_client_dri) {
        (true, true) => ", DRI on both sides",
        (true, false) => ", lawyer DRI",
        (false, true) => ", client DRI",
        (false, false) => "",
    };
    Ok(json!({
        "content": [{
            "type": "text",
            "text": format!(
                "{verb} person={} → project={} as {}{dri} (link id={}).",
                args.person_id, args.project_id, row.participation, row.id
            )
        }],
        "structuredContent": {
            "id": row.id,
            "person_id": args.person_id,
            "project_id": args.project_id,
            "participation": row.participation,
            "is_lawyer_dri": row.is_lawyer_dri,
            "is_client_dri": row.is_client_dri,
            "created": created,
        }
    }))
}

/// Map an [`store::participation::AddParticipantError`] to a `ToolError`. The
/// tool has already checked the person and project exist and that no row is
/// present, so the not-found and duplicate arms are defensive.
fn map_add_error(e: store::participation::AddParticipantError) -> ToolError {
    use store::participation::AddParticipantError as E;
    match e {
        E::ProjectNotFound => ToolError::NotFound("project".into()),
        E::PersonNotFound => ToolError::NotFound("person".into()),
        E::Duplicate => {
            ToolError::InvalidArguments("that person is already linked to this project".into())
        }
        // The tool links a person and never designates a DRI, so the command
        // carries `DriRequest::Unchanged` and this arm is unreachable in
        // practice — reported rather than swallowed if that ever changes.
        E::Dri(e) => ToolError::InvalidArguments(e.to_string()),
        E::Db(e) => ToolError::Database(e.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::{call, descriptor};
    use crate::tools::ToolError;
    use serde_json::json;
    use uuid::Uuid;

    async fn seed(
        application_store: &store::application_store::ApplicationStore,
        role: store::persons::Role,
    ) -> (Uuid, Uuid) {
        let p = store::persons::create(
            application_store,
            &store::persons::NewPerson::with_role(
                crate::test_fixtures::person_name(),
                crate::test_fixtures::email(),
                role,
            ),
        )
        .await
        .unwrap();
        crate::test_fixtures::seed_brands(application_store).await;
        let proj = store::projects::create(
            application_store,
            &store::projects::NewProject {
                code: format!("synthetic-{}", Uuid::now_v7().simple()),
                name: crate::test_fixtures::MATTER.clone(),
                status: "open".into(),
                entity_id: store::test_support::seed_entity(application_store).await,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        (p.id, proj.id)
    }

    /// The tool takes two ids and no role word. A model cannot name a
    /// participation because there is no field to name it in.
    #[test]
    fn descriptor_takes_two_ids_and_offers_no_role_field() {
        let d = descriptor();
        assert_eq!(d["name"], "link_person_project");
        let required: Vec<&str> = d["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(required, vec!["person_id", "project_id"]);
        assert!(
            d["inputSchema"]["properties"]["role"].is_null(),
            "the tool must not advertise a role input: {d}"
        );
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
    }

    /// The written participation is the person's tier, not anything the caller
    /// supplied — a `role` in the arguments is surplus and simply unread.
    #[tokio::test]
    async fn the_link_takes_its_participation_from_the_person_tier() {
        store::test_support::with_postgres(async {
            for (tier, expected) in [
                (store::persons::Role::Lawyer, "lawyer"),
                (store::persons::Role::Client, "client"),
            ] {
                let application_store = crate::test_fixtures::handle();
                let (pid, projid) = seed(&application_store, tier).await;
                let r = call(
                    &application_store,
                    &json!({ "person_id": pid, "project_id": projid, "role": "wizard" }),
                )
                .await
                .unwrap();
                assert_eq!(r["structuredContent"]["participation"], expected);
                assert_eq!(r["structuredContent"]["created"], true);
                // Participation and accountability are separate axes, so the
                // response carries all three rather than one flattened enum.
                // Linking never moves a DRI marker, so both are false here.
                assert_eq!(r["structuredContent"]["is_lawyer_dri"], false);
                assert_eq!(r["structuredContent"]["is_client_dri"], false);
                assert!(
                    r["structuredContent"]["role"].is_null(),
                    "`role` is the person's tier, not the matter-side word: {r}"
                );
                let all = store::projects::participations_for_project(&application_store, projid)
                    .await
                    .unwrap();
                assert_eq!(all.len(), 1);
                assert_eq!(all[0].participation, expected);
            }
        })
        .await;
    }

    #[tokio::test]
    async fn re_linking_the_same_pair_is_idempotent() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let (pid, projid) = seed(&application_store, store::persons::Role::Client).await;
            let args = json!({ "person_id": pid, "project_id": projid });
            let first = call(&application_store, &args).await.unwrap();
            let second = call(&application_store, &args).await.unwrap();
            assert_eq!(
                first["structuredContent"]["id"],
                second["structuredContent"]["id"]
            );
            assert_eq!(second["structuredContent"]["created"], false);
            let all = store::projects::participations_for_project(&application_store, projid)
                .await
                .unwrap();
            assert_eq!(all.len(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_person_id_returns_not_found() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let (_, projid) = seed(&application_store, store::persons::Role::Client).await;
            let err = call(
                &application_store,
                &json!({ "person_id": Uuid::now_v7(), "project_id": projid }),
            )
            .await
            .unwrap_err();
            assert!(matches!(err, ToolError::NotFound(_)));
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_project_id_returns_not_found() {
        store::test_support::with_postgres(async {
            let application_store = crate::test_fixtures::handle();
            let (pid, _) = seed(&application_store, store::persons::Role::Client).await;
            let err = call(
                &application_store,
                &json!({ "person_id": pid, "project_id": Uuid::now_v7() }),
            )
            .await
            .unwrap_err();
            assert!(matches!(err, ToolError::NotFound(_)));
        })
        .await;
    }
}
