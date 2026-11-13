use super::postgres;
use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

#[tokio::test]
async fn a_redelivery_lands_on_the_journaled_row_and_consumption_drains_it() {
    let Some(pg) = database().await else { return };
    let provider = SignatureProvider::DocuSign;
    let first =
        postgres::journal_verified_receipt(&pg, provider, "synthetic-envelope", "completed", "{}")
            .await
            .unwrap();
    let again = postgres::journal_verified_receipt(
        &pg,
        provider,
        "synthetic-envelope",
        "completed",
        "{\"redelivered\":true}",
    )
    .await
    .unwrap();
    assert_eq!(again, first, "a redelivery re-reads the journaled row");
    assert_eq!(first.attempts, 1);

    postgres::record_attempt_failure(&pg, first.id, "synthetic failure")
        .await
        .unwrap();
    let pending = postgres::pending_receipts(&pg, 10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, 2);
    assert_eq!(pending[0].last_error.as_deref(), Some("synthetic failure"));

    postgres::mark_consumed(&pg, first.id).await.unwrap();
    assert!(postgres::pending_receipts(&pg, 10)
        .await
        .unwrap()
        .is_empty());
    let consumed = postgres::by_event(&pg, provider, "synthetic-envelope", "completed")
        .await
        .unwrap()
        .unwrap();
    assert!(consumed.consumed_at.is_some());
    assert_eq!(consumed.last_error, None);
    pg.0.close().await;
}

#[tokio::test]
async fn surreal_receipts_copy_once_with_their_identity() {
    let Some(pg) = database().await else { return };
    let surreal = crate::surreal::test_support::mem().await;
    let receipt = journal_verified_receipt(
        &surreal,
        SignatureProvider::DocuSign,
        "synthetic-envelope",
        "completed",
        "{}",
    )
    .await
    .unwrap();

    let copied = postgres::copy_from_surreal(&surreal, &pg).await.unwrap();
    assert_eq!((copied.applied, copied.failures.len()), (1, 0));
    let rerun = postgres::copy_from_surreal(&surreal, &pg).await.unwrap();
    assert_eq!(rerun.applied, 0, "a rerun writes nothing");

    let pending = postgres::pending_receipts(&pg, 10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, receipt.id);
    assert_eq!(pending[0].payload, receipt.payload);
    pg.0.close().await;
}
