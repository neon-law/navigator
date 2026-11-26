use super::*;
use crate::test_support::mem_surreal;

#[tokio::test]
async fn the_engine_refuses_negative_shares() {
    let surreal = mem_surreal().await;
    let project_id = Uuid::now_v7();
    let holder_id = Uuid::now_v7();
    let result = surreal
        .query(
            "CREATE cap_table_holding SET project_id = $project_id, holder_id = $holder, \
             share_class = 'Common', shares = -5, inserted_at = '', updated_at = ''",
        )
        .bind(("project_id", record_id("project", project_id)))
        .bind(("holder", record_id("entity", holder_id)))
        .await
        .and_then(surrealdb::IndexedResults::check);
    assert!(result.is_err());
}
