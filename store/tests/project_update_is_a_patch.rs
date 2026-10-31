//! `update_project` is always a patch: an absent field is never written.
//!
//! The rule these tests pin is the one that keeps a partial update from
//! destroying data the caller never mentioned. A client that reads a matter,
//! edits one field, and sends its own narrow body must not be able to erase the
//! columns it does not know about — so absence means *leave alone*, and
//! clearing is always an explicit empty string.
//!
//! `null` and absent are deliberately the same thing. `serde` cannot tell them
//! apart on a plain `Option`, and collapsing them toward "leave alone" is the
//! safe direction: the other choice fails by wiping a column silently.

use store::projects::{
    create, update_project, NewProject, ProjectCommandError, UpdateProjectCommand,
};
use store::test_support::{mem_surreal, seed_entity};
use uuid::Uuid;

const A_REPOSITORY: &str = "https://forge.example/an-organization/a-matter";
const A_SLACK_CHANNEL: &str = "https://slack.example/archives/C0FIXTURE";

async fn matter(surreal: &store::surreal::SurrealDb) -> store::projects::Project {
    let code = format!("patch-{}", Uuid::now_v7().simple());
    let created = create(
        surreal,
        &NewProject {
            code: code.clone(),
            name: "Original Name".to_string(),
            status: "open".to_string(),
            entity_id: seed_entity(surreal).await,
            ..Default::default()
        },
    )
    .await
    .expect("create matter");

    // Give every optional column a value, so a later patch that wrongly wrote
    // an absent field would be visible rather than indistinguishable from a
    // column that was already empty.
    update_project(
        surreal,
        created.id,
        &UpdateProjectCommand {
            name: Some("Original Name".into()),
            description: Some("Original description".into()),
            repository_url: Some(A_REPOSITORY.into()),
            internal_slack_channel_url: Some(A_SLACK_CHANNEL.into()),
            ..Default::default()
        },
    )
    .await
    .expect("seed the optional columns")
}

async fn firm_wearing_neon(surreal: &store::surreal::SurrealDb) -> Uuid {
    let admin = store::persons::create(
        surreal,
        &store::persons::NewPerson::with_role(
            "Firm Admin",
            format!("firm-admin-{}@example.com", Uuid::now_v7()),
            store::persons::Role::Admin,
        ),
    )
    .await
    .expect("firm admin");
    let firm = store::firms::create(
        surreal,
        &store::firms::NewFirm {
            name: format!("Project Update Firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id: seed_entity(surreal).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .expect("firm");
    store::firms::attach_brand(surreal, firm.id, "neon")
        .await
        .expect("firm wears neon");
    firm.id
}

/// The central rule. One field is sent; every other column survives.
#[tokio::test]
async fn a_patch_touching_one_field_leaves_every_other_column_alone() {
    let surreal = mem_surreal().await;
    let before = matter(&surreal).await;

    let after = update_project(
        &surreal,
        before.id,
        &UpdateProjectCommand {
            description: Some("Edited description".into()),
            ..Default::default()
        },
    )
    .await
    .expect("a one-field patch applies");

    assert_eq!(after.description.as_deref(), Some("Edited description"));
    assert_eq!(
        after.name, before.name,
        "an absent name must not be written"
    );
    assert_eq!(
        after.repository_url, before.repository_url,
        "an absent repository_url must not be cleared"
    );
    assert_eq!(
        after.internal_slack_channel_url, before.internal_slack_channel_url,
        "an absent Slack channel must not be cleared"
    );
}

/// A JSON body carrying only `repository_url` deserializes and applies: a
/// caller never resends `name` to change another field, because an update
/// specifies only the fields it changes.
#[tokio::test]
async fn a_json_body_naming_one_field_deserializes_and_applies() {
    let surreal = mem_surreal().await;
    let before = matter(&surreal).await;

    let body = serde_json::json!({
        "repository_url": "https://forge.example/an-organization/moved"
    });
    let command: UpdateProjectCommand =
        serde_json::from_value(body).expect("a one-field JSON patch deserializes");

    let after = update_project(&surreal, before.id, &command)
        .await
        .expect("it applies");

    assert_eq!(
        after.repository_url.as_deref(),
        Some("https://forge.example/an-organization/moved")
    );
    assert_eq!(
        after.name, before.name,
        "the name is untouched by a body that never mentioned it"
    );
}

/// `null` reads as absent, not as a clear. The failure mode of the other
/// choice is a caller wiping a column it did not intend to touch, so the
/// collapse goes toward leaving it alone.
#[tokio::test]
async fn an_explicit_null_leaves_the_column_alone() {
    let surreal = mem_surreal().await;
    let before = matter(&surreal).await;

    let command: UpdateProjectCommand = serde_json::from_value(serde_json::json!({
        "description": "Edited",
        "repository_url": null,
    }))
    .expect("null deserializes");

    let after = update_project(&surreal, before.id, &command)
        .await
        .expect("it applies");

    assert_eq!(
        after.repository_url, before.repository_url,
        "an explicit null must read as absent, never as a clear"
    );
}

/// Clearing is explicit, and it is the empty string — the same value an HTML
/// form posts for a text input a person emptied, so the form caller and the
/// JSON caller converge on one rule.
#[tokio::test]
async fn an_empty_string_is_how_a_column_is_cleared() {
    let surreal = mem_surreal().await;
    let before = matter(&surreal).await;
    assert!(before.repository_url.is_some(), "the fixture set one");

    let after = update_project(
        &surreal,
        before.id,
        &UpdateProjectCommand {
            repository_url: Some(String::new()),
            ..Default::default()
        },
    )
    .await
    .expect("an explicit clear applies");

    assert_eq!(after.repository_url, None);
    assert_eq!(
        after.description, before.description,
        "clearing one column does not disturb another"
    );
}

/// The one field that refuses to be blanked. A matter with no name is not a
/// state a patch may produce, so `""` is an error rather than a clear —
/// while omitting it is still just "leave it alone".
#[tokio::test]
async fn a_blank_name_is_refused_but_an_absent_one_is_fine() {
    let surreal = mem_surreal().await;
    let before = matter(&surreal).await;

    let blank = update_project(
        &surreal,
        before.id,
        &UpdateProjectCommand {
            name: Some("   ".into()),
            ..Default::default()
        },
    )
    .await;
    assert!(
        matches!(blank, Err(ProjectCommandError::Invalid(_))),
        "a blank name is refused, not applied: {blank:?}"
    );

    let absent = update_project(&surreal, before.id, &UpdateProjectCommand::default())
        .await
        .expect("an empty patch is a no-op, not an error");
    assert_eq!(absent.name, before.name);
}

/// `status` is not a column this patch may touch — moving a matter through
/// its lifecycle belongs to `POST /app/api/projects/{id}/lifecycle`
/// (`transition_project`). A body naming it must fail to deserialize rather
/// than being silently dropped, so `{"status": "zzz-not-a-status"}` can never
/// apply as a no-op `200` a caller could mistake for success.
#[test]
fn a_status_field_fails_to_deserialize_rather_than_being_dropped() {
    let err = serde_json::from_value::<UpdateProjectCommand>(serde_json::json!({
        "status": "zzz-not-a-status",
    }))
    .expect_err("status is not a field on this patch");
    assert!(
        err.to_string().contains("status"),
        "the error should name the offending field: {err}"
    );
}

/// `closed_at` is derived from the transition inside `transition_project`
/// and must never be independently settable — not even to a value that
/// looks legitimate. Same failure mode as `status` above.
#[test]
fn a_closed_at_field_fails_to_deserialize_rather_than_being_dropped() {
    let err = serde_json::from_value::<UpdateProjectCommand>(serde_json::json!({
        "closed_at": "2020-01-01T00:00:00Z",
    }))
    .expect_err("closed_at is not a field on this patch");
    assert!(
        err.to_string().contains("closed_at"),
        "the error should name the offending field: {err}"
    );
}

/// Any other unrecognized field is rejected the same way — the point is a
/// no-op write can never pass for a successful one, not that these two
/// fields specifically are blocklisted.
#[test]
fn an_unknown_field_fails_to_deserialize_rather_than_being_dropped() {
    let err = serde_json::from_value::<UpdateProjectCommand>(serde_json::json!({
        "made_up_field": "anything",
    }))
    .expect_err("an unrecognized field must not silently deserialize");
    assert!(
        err.to_string().contains("made_up_field"),
        "the error should name the offending field: {err}"
    );
}

#[tokio::test]
async fn a_firm_and_brand_update_must_name_both_fields() {
    let surreal = mem_surreal().await;
    let before = matter(&surreal).await;
    let command = UpdateProjectCommand {
        firm_id: Some(Uuid::now_v7()),
        ..Default::default()
    };

    let error = update_project(&surreal, before.id, &command)
        .await
        .expect_err("a Firm without its brand must be refused");

    assert_eq!(error.to_string(), "Firm and brand must be set together.");
}

#[tokio::test]
async fn a_firm_and_brand_update_writes_a_matching_pair() {
    let surreal = mem_surreal().await;
    let before = matter(&surreal).await;
    let firm_id = firm_wearing_neon(&surreal).await;

    let after = update_project(
        &surreal,
        before.id,
        &UpdateProjectCommand {
            firm_id: Some(firm_id),
            brand: Some("neon".into()),
            ..Default::default()
        },
    )
    .await
    .expect("a matching pair applies");

    assert_eq!(after.firm_id, Some(firm_id));
    assert_eq!(after.brand, "neon");
}

#[tokio::test]
async fn a_firm_that_does_not_wear_the_selected_brand_is_refused_by_name() {
    let surreal = mem_surreal().await;
    let before = matter(&surreal).await;

    let error = update_project(
        &surreal,
        before.id,
        &UpdateProjectCommand {
            firm_id: Some(Uuid::now_v7()),
            brand: Some("neon".into()),
            ..Default::default()
        },
    )
    .await
    .expect_err("the pair must agree with firm_brand");

    assert_eq!(
        error.to_string(),
        "That Firm does not wear the selected brand."
    );
}

/// LAW-213: the Drive ingest folder id joins the sparse patch — one field
/// sent, applied, and clearable by the blank string like every other column.
#[tokio::test]
async fn a_drive_folder_patch_applies_and_a_blank_one_clears() {
    let surreal = mem_surreal().await;
    let before = matter(&surreal).await;

    let after = update_project(
        &surreal,
        before.id,
        &UpdateProjectCommand {
            drive_folder_id: Some("1QaBcD_2-efG".into()),
            ..Default::default()
        },
    )
    .await
    .expect("a drive-folder patch applies");
    assert_eq!(after.drive_folder_id.as_deref(), Some("1QaBcD_2-efG"));
    assert_eq!(
        after.calendar_id, before.calendar_id,
        "an absent calendar_id must not be cleared by a folder patch"
    );

    let cleared = update_project(
        &surreal,
        before.id,
        &UpdateProjectCommand {
            drive_folder_id: Some(String::new()),
            ..Default::default()
        },
    )
    .await
    .expect("a blank folder clears");
    assert_eq!(cleared.drive_folder_id, None);
}

/// A Drive ingest folder belongs to exactly one matter — the importer scopes
/// every fetch by the folder link — so a patch recording a folder another
/// matter already owns is refused rather than silently shared.
#[tokio::test]
async fn a_drive_folder_recorded_on_another_matter_is_refused() {
    let surreal = mem_surreal().await;
    let first = matter(&surreal).await;
    let second_code = format!("patch-{}", Uuid::now_v7().simple());
    let second = create(
        &surreal,
        &NewProject {
            code: second_code,
            name: "Second matter".to_string(),
            status: "open".to_string(),
            entity_id: seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .expect("create the second matter");

    update_project(
        &surreal,
        first.id,
        &UpdateProjectCommand {
            drive_folder_id: Some("1QaBcD_2-efG".into()),
            ..Default::default()
        },
    )
    .await
    .expect("the first matter records the folder");

    let error = update_project(
        &surreal,
        second.id,
        &UpdateProjectCommand {
            drive_folder_id: Some("1QaBcD_2-efG".into()),
            ..Default::default()
        },
    )
    .await
    .expect_err("the second matter must be refused the same folder");
    assert!(
        error.to_string().contains("another matter"),
        "the error names the conflict: {error}"
    );

    // Re-recording the folder the matter already has is idempotent, not a
    // self-conflict.
    update_project(
        &surreal,
        first.id,
        &UpdateProjectCommand {
            drive_folder_id: Some("1QaBcD_2-efG".into()),
            ..Default::default()
        },
    )
    .await
    .expect("the owning matter may re-record its own folder");
}

/// The folder id is placed in a Drive API path, so anything that could
/// change that path — a link, a query, a comma-separated list — is refused
/// at the command boundary, not escaped at the provider.
#[tokio::test]
async fn a_malformed_drive_folder_id_is_refused() {
    let surreal = mem_surreal().await;
    let before = matter(&surreal).await;

    for bad in [
        "https://drive.google.com/drive/folders/1QaBcD",
        "1QaBcD?resourcekey=x",
        "1QaBcD_2-efG,1AbCdE",
    ] {
        let error = update_project(
            &surreal,
            before.id,
            &UpdateProjectCommand {
                drive_folder_id: Some(bad.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("a malformed folder id must be refused");
        assert!(
            error.to_string().contains("Drive folder id"),
            "the error says what shape is wanted: {error}"
        );
    }
}
