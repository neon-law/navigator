//! Forgejo push webhook journal tests, in the established
//! `store/src/postgres/*_tests.rs` pattern: a fresh canonical schema per
//! test, skipped silently when no local PostgreSQL is running. The status
//! machine is the slice's contract — journal first, consume second,
//! delivered and skipped terminal, failed re-consumable.

use serde_json::json;

use super::forgejo_push_webhook_journal as journal;
use super::forgejo_push_webhook_journal::{NewPushReceipt, ReceiptStatus};
use super::PostgresDb;
use crate::postgres_mirror::tests::{connect, fresh_schema};

/// A `push/<change-id>` bookmark update, the shape a jj push produces.
fn ref_updates() -> serde_json::Value {
    json!([
        {
            "ref": "refs/heads/push/qvrpxyq",
            "before": "0000000000000000000000000000000000000000",
            "after": "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1"
        }
    ])
}

fn delivery() -> NewPushReceipt {
    NewPushReceipt {
        delivered_at: chrono::Utc::now(),
        repo_full_name: "firm-slug/project-code".to_string(),
        ref_updates: ref_updates(),
        payload: r#"{"ref":"refs/heads/push/qvrpxyq","before":"0000000000000000000000000000000000000000","after":"9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1","pusher":{"login":"tbot"}}"#.to_string(),
    }
}

fn analysis() -> journal::PushAnalysis {
    journal::PushAnalysis {
        change_id: "qvrpxyq".to_string(),
        author: "T-bot (tbot@neonlaw.org)".to_string(),
        description_summary: "Amend the engagement letter's fee schedule".to_string(),
        stats: json!({"insertions": 12, "deletions": 3, "files": 1}),
        conflict_signal: false,
    }
}

/// A fresh canonical schema per test; none skips when no PostgreSQL lane is
/// configured.
async fn postgres() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

/// Journaling a verified push is the insert half of the round trip: the
/// receipt reads back field for field, pending, and is the replay sweep's
/// work.
#[tokio::test]
async fn a_journaled_push_round_trips_through_the_journal() {
    let Some(pg) = postgres().await else { return };
    let journaled = journal::journal_verified_push(&pg, &delivery())
        .await
        .expect("journal the verified push");

    assert_eq!(journaled.status, ReceiptStatus::Pending);
    assert_eq!(journaled.repo_full_name, "firm-slug/project-code");
    assert_eq!(journaled.ref_updates, ref_updates());
    assert_eq!(
        journaled.change_id, None,
        "analysis fills these, not the receipt"
    );
    assert_eq!(journaled.failure_detail, None);

    let read = journal::by_event_key(&pg, &journaled.event_key)
        .await
        .expect("read the receipt back")
        .expect("the journaled event resolves by its key");
    assert_eq!(read, journaled);

    let pending = journal::reconsumable_receipts(&pg, 10)
        .await
        .expect("read the sweep's work list");
    assert_eq!(pending.len(), 1, "a fresh receipt is re-consumable");
    assert_eq!(pending[0].id, journaled.id);
}

/// A redelivery of the same push — same body, new delivery id — resolves to
/// the journaled row instead of double-journaling, and deduplication is
/// content-based: a different push journals separately.
#[tokio::test]
async fn a_redelivery_resolves_to_the_journaled_row() {
    let Some(pg) = postgres().await else { return };
    let first = journal::journal_verified_push(&pg, &delivery())
        .await
        .expect("journal the push");

    let redelivered = journal::journal_verified_push(&pg, &delivery())
        .await
        .expect("the redelivery resolves instead of failing");
    assert_eq!(redelivered.id, first.id, "same body, same row");

    let rows = journal::reconsumable_receipts(&pg, 10)
        .await
        .expect("read the journal");
    assert_eq!(rows.len(), 1, "a redelivery never double-journals");

    let other = NewPushReceipt {
        repo_full_name: "firm-slug/other-project".to_string(),
        ..delivery()
    };
    let second = journal::journal_verified_push(&pg, &other)
        .await
        .expect("a different push journals separately");
    assert_ne!(second.id, first.id);
}

/// The guarded transitions: analysis fills its columns, delivery closes the
/// receipt, and a terminal receipt answers every further transition with
/// `None` — a redelivery re-runs nothing.
#[tokio::test]
async fn the_status_machine_constrains_transitions() {
    let Some(pg) = postgres().await else { return };
    let receipt = journal::journal_verified_push(&pg, &delivery())
        .await
        .expect("journal the push");

    let analyzed = journal::record_analysis(&pg, receipt.id, &analysis())
        .await
        .expect("record the analysis")
        .expect("a pending receipt takes the analyzed transition");
    assert_eq!(analyzed.status, ReceiptStatus::Analyzed);
    assert_eq!(analyzed.change_id.as_deref(), Some("qvrpxyq"));
    assert_eq!(analyzed.conflict_signal, Some(false));
    assert_eq!(analyzed.stats, Some(analysis().stats));

    let delivered = journal::mark_delivered(&pg, receipt.id)
        .await
        .expect("mark delivered")
        .expect("an analyzed receipt delivers");
    assert_eq!(delivered.status, ReceiptStatus::Delivered);
    assert_eq!(delivered.failure_detail, None);

    assert!(journal::record_failure(&pg, receipt.id, "late failure")
        .await
        .expect("the terminal write ran")
        .is_none());
    assert_eq!(
        journal::by_event_key(&pg, &receipt.event_key)
            .await
            .expect("read the row")
            .expect("the row survives")
            .status,
        ReceiptStatus::Delivered,
        "delivered is terminal: a failure never reopens it"
    );
}

/// `skipped` is the other terminal: a verified payload with no `push/*` ref
/// update is observable and never analyzed, and a redelivery re-runs
/// nothing.
#[tokio::test]
async fn a_skipped_receipt_is_terminal() {
    let Some(pg) = postgres().await else { return };
    let receipt = journal::journal_verified_push(&pg, &delivery())
        .await
        .expect("journal the push");

    let skipped = journal::mark_skipped(&pg, receipt.id)
        .await
        .expect("mark skipped")
        .expect("a pending receipt skips");
    assert_eq!(skipped.status, ReceiptStatus::Skipped);

    assert!(journal::record_analysis(&pg, receipt.id, &analysis())
        .await
        .expect("the terminal write ran")
        .is_none());
    assert!(
        journal::reconsumable_receipts(&pg, 10)
            .await
            .expect("read the journal")
            .is_empty(),
        "a skipped receipt never re-runs"
    );
}

/// A failed consumption names itself and stays on the sweep's work list; the
/// retry path delivers through the failure — `failed` is re-consumable,
/// never lost.
#[tokio::test]
async fn a_failed_receipt_stays_reconsumable() {
    let Some(pg) = postgres().await else { return };
    let receipt = journal::journal_verified_push(&pg, &delivery())
        .await
        .expect("journal the push");

    let failed = journal::record_failure(&pg, receipt.id, "slack unavailable")
        .await
        .expect("record the failure")
        .expect("a pending receipt fails");
    assert_eq!(failed.status, ReceiptStatus::Failed);
    assert_eq!(failed.failure_detail.as_deref(), Some("slack unavailable"));

    let reconsumable = journal::reconsumable_receipts(&pg, 10)
        .await
        .expect("read the sweep's work list");
    assert_eq!(reconsumable.len(), 1, "the failed receipt is re-consumable");
    assert_eq!(reconsumable[0].id, receipt.id);

    let delivered = journal::mark_delivered(&pg, receipt.id)
        .await
        .expect("mark delivered")
        .expect("the retry delivers through the failure");
    assert_eq!(delivered.status, ReceiptStatus::Delivered);
    assert_eq!(
        delivered.failure_detail, None,
        "a successful transition clears the failure it retried"
    );
}

/// The event key is stable for the same body and different across
/// repositories and ref updates — the redelivery lookup's whole contract.
#[test]
fn the_event_key_identifies_a_push_by_content() {
    let key = journal::push_event_key("firm-slug/project-code", &ref_updates());
    assert_eq!(
        journal::push_event_key("firm-slug/project-code", &ref_updates()),
        key,
        "the same body derives the same key"
    );
    assert_ne!(
        journal::push_event_key("firm-slug/other-project", &ref_updates()),
        key,
        "a different repository is a different event"
    );
    let moved = json!([{ "ref": "refs/heads/push/zzzzzzz", "after": "9f3ba2c7" }]);
    assert_ne!(
        journal::push_event_key("firm-slug/project-code", &moved),
        key,
        "a different ref update is a different event"
    );
}

/// The status strings are the SQL `CHECK` constraint's closed set, and
/// terminality marks exactly the statuses a redelivery re-runs nothing for.
#[test]
fn the_status_vocabulary_matches_the_check_constraint() {
    for (status, name) in [
        (ReceiptStatus::Pending, "pending"),
        (ReceiptStatus::Analyzed, "analyzed"),
        (ReceiptStatus::Delivered, "delivered"),
        (ReceiptStatus::Failed, "failed"),
        (ReceiptStatus::Skipped, "skipped"),
    ] {
        assert_eq!(status.as_str(), name);
        assert_eq!(ReceiptStatus::parse(name), Some(status));
    }
    assert_eq!(ReceiptStatus::parse("withdrawn"), None);
    assert!(!ReceiptStatus::Pending.is_terminal());
    assert!(!ReceiptStatus::Failed.is_terminal());
    assert!(ReceiptStatus::Delivered.is_terminal());
    assert!(ReceiptStatus::Skipped.is_terminal());
}

/// The store rejects a status outside the closed set at the read boundary —
/// a row written around this module cannot masquerade as a receipt.
#[tokio::test]
async fn a_row_outside_the_closed_set_is_an_error_not_a_receipt() {
    let Some(pg) = postgres().await else { return };
    let receipt = journal::journal_verified_push(&pg, &delivery())
        .await
        .expect("journal the push");
    sqlx::query("UPDATE forgejo_push_webhook_journal SET status = 'withdrawn' WHERE id = $1")
        .bind(receipt.id)
        .execute(pg.pool())
        .await
        .expect("corrupt the row");

    let error = journal::by_event_key(&pg, &receipt.event_key)
        .await
        .expect_err("the corrupted row is named, not masqueraded");
    // The status names itself somewhere in the error chain — top level or
    // decode source.
    let mut reported = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        reported.push_str("; ");
        reported.push_str(&cause.to_string());
        source = cause.source();
    }
    assert!(
        reported.contains("withdrawn"),
        "the error names the status violation: {reported}"
    );
}
