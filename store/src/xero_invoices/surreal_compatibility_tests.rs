//! A Source-engine concurrency assertion retained in the explicit compatibility lane.

use tokio::task;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn the_unique_index_alone_does_not_serialize_racers() {
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::test_support::seed_project_surreal(&surreal, "sample-matter").await;

    let racers: Vec<_> = (0..8)
        .map(|_| {
            let surreal = surreal.clone();
            task::spawn(async move {
                surreal
                    .query(
                        "CREATE $id SET project_id = $project, xero_invoice_id = 'xero-1', \\
                         reference = 'race', status = 'AUTHORISED', amount_cents = 333300, \\
                         currency = 'USD', issued_at = time::now()",
                    )
                    .bind((
                        "id",
                        crate::surreal::record_id(crate::xero_invoices::TABLE, Uuid::now_v7()),
                    ))
                    .bind((
                        "project",
                        crate::surreal::record_id(crate::projects::PROJECT_TABLE, project_id),
                    ))
                    .await
                    .and_then(surrealdb::IndexedResults::check)
            })
        })
        .collect();

    let mut landed = 0;
    for racer in racers {
        if racer.await.expect("racer task").is_ok() {
            landed += 1;
        }
    }
    assert!(landed >= 1, "at least one unguarded write must land");
}
