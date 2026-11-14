use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::mem_surreal;
use std::process::Command;

#[test]
fn keys_are_opaque_and_use_source_table_names() {
    let id = Uuid::nil();
    assert_eq!(MirrorKey::EmailReceipt(id).table(), "email_receipt");
    assert_eq!(MirrorKey::EmailDelivery(id).table(), "email_delivery");
    assert_eq!(
        MirrorKey::EmailDeliveryAttempt(id).table(),
        "email_delivery_attempt"
    );
    assert!(!MirrorKey::EmailReceipt(id).to_string().contains('@'));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn receipt_delivery_attempt_mirror_replays_reconciles_and_cascades() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let ensured = crate::email_receipts::ensure(
        &surreal,
        &crate::email_receipts::NewEmailReceipt {
            receiving_mailbox: "synthetic@example.test",
            deployment: "test",
            firm_id: None,
            provider_account: None,
            sender_authenticated: false,
            raw_digest: "synthetic-digest",
            source_message_id: Some("synthetic-message-id"),
            archive_key: "synthetic/archive.eml",
            letter_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    let receipt_id = ensured.receipt.id;
    let delivery = crate::email_deliveries::ensure(&surreal, receipt_id, "synthetic-channel")
        .await
        .unwrap();
    let admission =
        crate::email_deliveries::admit_attempt(&surreal, receipt_id, "synthetic-correlation")
            .await
            .unwrap();
    let attempt_id = admission.attempt_id.unwrap();
    let keys = [
        MirrorKey::EmailReceipt(receipt_id),
        MirrorKey::EmailDelivery(delivery.id),
        MirrorKey::EmailDeliveryAttempt(attempt_id),
    ];
    for key in &keys {
        mirror(&surreal, &pg, key).await.unwrap();
    }
    for key in &keys {
        assert_eq!(
            mirror(&surreal, &pg, key).await.unwrap(),
            Mirrored::Unchanged
        );
    }
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());

    // A target-side parent loss cascades; reconciliation restores parents
    // before children, including the attempt state.
    sqlx::query("DELETE FROM email_receipt WHERE id=$1")
        .bind(receipt_id)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM email_delivery_attempt WHERE id=$1")
            .bind(attempt_id)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        1
    );

    crate::email_deliveries::mark_confirmed(
        &surreal,
        receipt_id,
        "synthetic-channel",
        "1700000000.000001",
    )
    .await
    .unwrap();
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM email_delivery_attempt WHERE id=$1")
            .bind(attempt_id)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        "confirmed"
    );

    surreal
        .query("DELETE email_delivery_attempt WHERE id=$id")
        .bind((
            "id",
            crate::surreal::record_id("email_delivery_attempt", attempt_id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    surreal
        .query("DELETE email_delivery WHERE id=$id")
        .bind((
            "id",
            crate::surreal::record_id("email_delivery", delivery.id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    surreal
        .query("DELETE email_receipt WHERE id=$id")
        .bind(("id", crate::surreal::record_id("email_receipt", receipt_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &MirrorKey::EmailReceipt(receipt_id))
            .await
            .unwrap(),
        Mirrored::Deleted
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}

/// The writers read process-global PostgreSQL configuration, so run each case
/// in a subprocess with a private environment as the broader mirror suite does.
/// Without a host they write SurrealDB; with one, PostgreSQL holds the rows and
/// SurrealDB none, whether or not the mirror is on.
#[test]
fn email_writers_follow_the_postgres_host() {
    let Some((options, database)) = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fresh_schema())
    else {
        return;
    };
    for case in ["disabled", "postgres"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "postgres_mirror::email_deliveries::tests::email_write_hook_probe",
                "--nocapture",
            ])
            .env("NAVIGATOR_EMAIL_MIRROR_TEST_CASE", case)
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env(
                "NAVIGATOR_POSTGRES_PORT",
                std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap(),
            )
            .env("NAVIGATOR_POSTGRES_USER", "postgres")
            .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
            .env("NAVIGATOR_POSTGRES_DATABASE", &database)
            .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
            .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT");
        if case == "disabled" {
            child.env_remove("NAVIGATOR_POSTGRES_HOST");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "email writer scenario {case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    drop(options);
}

#[tokio::test]
async fn email_write_hook_probe() {
    let Ok(case) = std::env::var("NAVIGATOR_EMAIL_MIRROR_TEST_CASE") else {
        return;
    };
    let surreal = mem_surreal().await;
    let receipt_id = crate::postgres_mirror::tests::hooked_email(&surreal).await;
    assert!(crate::email_receipts::find_by_id(&surreal, receipt_id)
        .await
        .unwrap()
        .is_some());
    let attempts: Vec<String> = crate::email_deliveries::attempts(&surreal, receipt_id)
        .await
        .unwrap()
        .into_iter()
        .map(|attempt| attempt.state)
        .collect();
    assert_eq!(attempts, ["unknown", "failed", "confirmed"]);
    let surreal_rows: Vec<surrealdb::types::RecordId> = surreal
        .query("SELECT VALUE id FROM email_receipt")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    assert_eq!(surreal_rows.len(), usize::from(case == "disabled"));
    if case == "postgres" {
        let pg = PostgresDb::from_env().await.unwrap();
        assert!(super::parity(&surreal, &pg)
            .await
            .unwrap()
            .tables
            .is_empty());
        assert_eq!(
            mirror(&surreal, &pg, &MirrorKey::EmailReceipt(receipt_id))
                .await
                .unwrap(),
            Mirrored::Unchanged
        );
    }
}
