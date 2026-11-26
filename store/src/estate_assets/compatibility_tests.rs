use super::record_id;
use crate::test_support::{mem_surreal, seed_entity};
use uuid::Uuid;

async fn project(surreal: &crate::surreal::SurrealDb, code: &str) -> Uuid {
    crate::projects::create(
        surreal,
        &crate::projects::NewProject {
            code: code.into(),
            name: "Sample estate plan".into(),
            status: "open".into(),
            entity_id: seed_entity(surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn the_engine_refuses_a_funding_state_outside_the_closed_list() {
    let surreal = mem_surreal().await;
    let project_id = project(&surreal, "sample-schedule-engine").await;
    let result = surreal
        .query(
            "CREATE estate_asset SET project_id = $project_id, category = 'account', \
             description = 'Checking', passes_by = 'trust', funding_state = 'maybe', \
             inserted_at = '', updated_at = ''",
        )
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check);
    assert!(result.is_err());
}
