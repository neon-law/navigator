use super::*;

async fn claim(
    pg: &PostgresDb,
    receipt: Uuid,
    operation: Uuid,
    terminal_owner: Option<Uuid>,
    actor: Option<&str>,
) -> Result<bool, RecoveryError> {
    let digest: Option<String> =
        sqlx::query_scalar("SELECT raw_digest FROM email_receipt WHERE id=$1")
            .bind(receipt)
            .fetch_optional(pg.pool())
            .await
            .unwrap();
    super::claim(
        pg,
        receipt,
        operation,
        terminal_owner,
        digest.as_deref().unwrap_or("missing"),
        DeliveryRule::RetryFailureNotice(actor),
    )
    .await
}

async fn receipt(pg: &PostgresDb) -> Uuid {
    let source = crate::application_store::ApplicationStore::uninitialized();
    let id = crate::email_receipts::ensure(
        &source,
        &crate::email_receipts::NewEmailReceipt {
            receiving_mailbox: "summary@example.test",
            deployment: "synthetic",
            firm_id: None,
            provider_account: None,
            sender_authenticated: false,
            raw_digest: &Uuid::now_v7().to_string(),
            source_message_id: None,
            archive_key: "inbound/synthetic.eml",
            letter_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap()
    .receipt
    .id;
    crate::email_deliveries::ensure(&source, id, "C-SYNTHETIC")
        .await
        .unwrap();
    assert_eq!(owner(pg, id).await, None);
    id
}

async fn owner(pg: &PostgresDb, receipt: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("SELECT summary_recovery_operation FROM email_receipt WHERE id=$1")
        .bind(receipt)
        .fetch_one(pg.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn concurrent_recovery_has_one_owner_and_same_operation_never_reopens_twice() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let receipt = receipt(&pg).await;
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let (first, second) = tokio::join!(
            claim(&pg, receipt, a, None, None),
            claim(&pg, receipt, b, None, None)
        );
        assert!(matches!((&first, &second), (Ok(true), Err(RecoveryError::Busy { .. })) | (Err(RecoveryError::Busy { .. }), Ok(true))));
        let winner = owner(&pg, receipt).await.unwrap();
        sqlx::query("UPDATE email_delivery SET state='confirmed',slack_timestamp='synthetic' WHERE receipt_id=$1")
            .bind(receipt).execute(pg.pool()).await.unwrap();
        assert!(!claim(&pg, receipt, winner, None, Some("operator")).await.unwrap());
        let state: String = sqlx::query_scalar("SELECT state FROM email_delivery WHERE receipt_id=$1")
            .bind(receipt).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(state,"confirmed");
        assert!(matches!(claim(&pg,receipt,Uuid::now_v7(),None,None).await,Err(RecoveryError::Busy {..})));
        assert_eq!(owner(&pg, receipt).await,Some(winner));
    }).await;
}

#[tokio::test]
async fn recovery_admission_quarantines_uncertain_delivery_and_atomically_audits_reopen() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let receipt = receipt(&pg).await;
        let operation = Uuid::now_v7();
        for state in ["sending","unknown"] {
            sqlx::query("UPDATE email_delivery SET state=$2 WHERE receipt_id=$1")
                .bind(receipt).bind(state).execute(pg.pool()).await.unwrap();
            assert!(matches!(claim(&pg,receipt,operation,None,Some("operator")).await,Err(RecoveryError::Quarantined)));
            assert_eq!(owner(&pg,receipt).await,None);
        }
        sqlx::query("UPDATE email_delivery SET state='confirmed', slack_timestamp='synthetic' WHERE receipt_id=$1")
            .bind(receipt).execute(pg.pool()).await.unwrap();
        assert!(matches!(claim(&pg,receipt,operation,None,None).await,Err(RecoveryError::Confirmed)));
        assert!(matches!(claim(&pg,receipt,operation,None,Some(" ")).await,Err(RecoveryError::Actor)));
        assert_eq!(owner(&pg,receipt).await,None);
        assert!(claim(&pg,receipt,operation,None,Some("operator")).await.unwrap());
        let row: (String,Option<String>,Option<String>,Option<String>) = sqlx::query_as(
            "SELECT state,slack_timestamp,reconciled_by,reconciliation FROM email_delivery WHERE receipt_id=$1")
            .bind(receipt).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(row,("not_attempted".into(),None,Some("operator".into()),Some("redrive_after_failed_result".into())));
        assert_eq!(owner(&pg,receipt).await,Some(operation));
        let next=Uuid::now_v7();
        assert!(matches!(claim(&pg,receipt,next,Some(Uuid::now_v7()),None).await,Err(RecoveryError::Busy { .. })));
        assert!(claim(&pg,receipt,next,Some(operation),None).await.unwrap());
        assert!(matches!(claim(&pg,receipt,Uuid::now_v7(),Some(operation),None).await,Err(RecoveryError::Busy {..})));
        assert_eq!(owner(&pg,receipt).await,Some(next));
        assert!(matches!(claim(&pg,Uuid::now_v7(),Uuid::now_v7(),None,None).await,Err(RecoveryError::NotFound)));
    }).await;
}
