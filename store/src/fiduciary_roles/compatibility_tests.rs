use super::*;
use crate::test_support::mem_surreal;

#[tokio::test]
async fn the_engine_refuses_a_role_outside_the_closed_list() {
    let surreal = mem_surreal().await;
    let project_id = Uuid::now_v7();
    let person_id = Uuid::now_v7();
    let result = surreal
        .query(
            "CREATE fiduciary_role SET project_id = $project_id, role = 'beneficiary', \
             fiduciary_id = $person, successor_order = 1, inserted_at = '', updated_at = ''",
        )
        .bind(("project_id", record_id("project", project_id)))
        .bind(("person", record_id("person", person_id)))
        .await
        .and_then(surrealdb::IndexedResults::check);
    assert!(result.is_err());
}
