use super::{
    tests::{ingest_for, world},
    *,
};
use crate::test_support::mem_surreal;

#[tokio::test]
async fn messages_written_before_process_fields_read_as_unprocessed() {
    let db = mem_surreal().await;
    let world = world(&db).await;
    let message_id = ingest_for(&db, world.firm, "process-historical-1").await;
    // A row written before the process fields existed carries none.
    db.query("UPDATE $id UNSET route, processed_at, nef_outcome")
        .bind(("id", record_id(MESSAGE, message_id)))
        .await
        .unwrap()
        .check()
        .unwrap();

    let state = process_state(&db, message_id).await.unwrap().unwrap();
    assert_eq!(state.processed_at, None);
    assert_eq!(state.route, None);
    assert_eq!(state.nef_outcome, None);

    // And the tolerant read still lets a process run record the route.
    mark_processed(&db, message_id, route::OTHER, Utc::now(), None)
        .await
        .unwrap();
    let state = process_state(&db, message_id).await.unwrap().unwrap();
    assert_eq!(state.route.as_deref(), Some(route::OTHER));
}
