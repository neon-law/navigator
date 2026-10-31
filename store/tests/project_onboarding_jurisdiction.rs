//! `project.onboarding_jurisdiction` — the jurisdiction an onboarding
//! Notation's Clauses bind to.
//!
//! The column replaced the questionnaire-state workaround that made every
//! onboarding template ask its own single-choice "which jurisdiction" question
//! to restate a fact the matter already knows. What these tests pin: a matter
//! opens with none and nothing infers one, a row written before the field
//! existed reads back through the production accessor, and the write path is
//! the same sparse patch every other optional column uses — set, clear, and
//! leave-alone are three different instructions.

use store::projects::{create, find_by_id, update_project, NewProject, UpdateProjectCommand};
use store::surreal::record_id;
use store::test_support::{mem_surreal, seed_entity};
use uuid::Uuid;

async fn project(surreal: &store::surreal::SurrealDb, code: &str) -> store::projects::Project {
    create(
        surreal,
        &NewProject {
            code: code.to_string(),
            name: code.to_string(),
            status: "open".to_string(),
            entity_id: seed_entity(surreal).await,
            ..Default::default()
        },
    )
    .await
    .expect("create matter")
}

/// A matter opens with no onboarding jurisdiction, and nothing invents one.
/// The matter-open command does not write the field (the Firm sets it at or
/// after matter-open, `navigator project set --jurisdiction`), so every row
/// `create` writes predates the value.
#[tokio::test]
async fn a_new_matter_records_no_onboarding_jurisdiction() {
    let surreal = mem_surreal().await;
    let row = project(&surreal, "onboarding-jurisdiction-absent").await;
    assert_eq!(
        row.onboarding_jurisdiction, None,
        "a matter opens before its drafting jurisdiction is chosen"
    );
}

/// A row written before the field existed — a raw `CREATE` that bypasses
/// every command seam, exactly what staging and production hold — reads back
/// through the same accessor production uses, with `None` for the field.
/// An optional field is the skill's tolerant-reader shape: no backfill may
/// run between deploy and read.
#[tokio::test]
async fn a_row_written_without_the_field_reads_as_none() {
    let surreal = mem_surreal().await;
    let entity_id = seed_entity(&surreal).await;
    let now = chrono::Utc::now().to_rfc3339();
    let historical_id = Uuid::now_v7();
    surreal
        .query(
            "CREATE $id SET code = 'jurisdiction-historical', name = 'Historical Row', \
             status = 'open', brand = 'neon', entity_id = $entity_id, \
             inserted_at = $now, updated_at = $now",
        )
        .bind(("id", record_id("project", historical_id)))
        .bind(("entity_id", record_id("entity", entity_id)))
        .bind(("now", now))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .expect("create a project row with no onboarding_jurisdiction field");

    let read = find_by_id(&surreal, historical_id)
        .await
        .expect("read the historical row")
        .expect("the row exists");
    assert_eq!(
        read.onboarding_jurisdiction, None,
        "a historical row has no value and none may be inferred"
    );
}

/// Setting, clearing, and leaving alone are three different instructions,
/// through the same sparse patch every other optional column uses.
#[tokio::test]
async fn the_patch_sets_clears_and_leaves_the_column_alone() {
    let surreal = mem_surreal().await;
    let row = project(&surreal, "onboarding-jurisdiction-patch").await;

    // Absent leaves the column alone — a patch that never mentions the
    // jurisdiction cannot erase one.
    let untouched = update_project(
        &surreal,
        row.id,
        &UpdateProjectCommand {
            description: Some("unrelated edit".into()),
            ..Default::default()
        },
    )
    .await
    .expect("patch without the field");
    assert_eq!(untouched.onboarding_jurisdiction, None);

    let set = update_project(
        &surreal,
        row.id,
        &UpdateProjectCommand {
            onboarding_jurisdiction: Some("Nevada".into()),
            ..Default::default()
        },
    )
    .await
    .expect("set the jurisdiction");
    assert_eq!(set.onboarding_jurisdiction.as_deref(), Some("Nevada"));

    // A blank submission clears the column — the same value an HTML form
    // posts for a text input a person emptied.
    let cleared = update_project(
        &surreal,
        row.id,
        &UpdateProjectCommand {
            onboarding_jurisdiction: Some("   ".into()),
            ..Default::default()
        },
    )
    .await
    .expect("clear the jurisdiction");
    assert_eq!(cleared.onboarding_jurisdiction, None);
}

/// The stored value round-trips verbatim: the render reads it live, so what
/// the Clause binds to is exactly what was written.
#[tokio::test]
async fn the_recorded_value_round_trips_verbatim() {
    let surreal = mem_surreal().await;
    let row = project(&surreal, "onboarding-jurisdiction-roundtrip").await;
    let written = update_project(
        &surreal,
        row.id,
        &UpdateProjectCommand {
            onboarding_jurisdiction: Some("Nevada".into()),
            ..Default::default()
        },
    )
    .await
    .expect("write");
    assert_eq!(written.onboarding_jurisdiction.as_deref(), Some("Nevada"));

    let read = find_by_id(&surreal, row.id)
        .await
        .expect("read back")
        .expect("the row exists");
    assert_eq!(read.onboarding_jurisdiction.as_deref(), Some("Nevada"));
}
