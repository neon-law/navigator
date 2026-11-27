use super::*;
#[tokio::test]
async fn interrupted_recovery_keeps_confirmation_and_binds_the_receipt_digest() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let source = crate::application_store::ApplicationStore::uninitialized();
        let receipt = crate::email_receipts::ensure(
            &source,
            &crate::email_receipts::NewEmailReceipt {
                receiving_mailbox: "summary@example.test",
                deployment: "synthetic",
                firm_id: None,
                provider_account: None,
                sender_authenticated: false,
                raw_digest: "synthetic-digest",
                source_message_id: None,
                archive_key: "inbound/synthetic.eml",
                letter_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap()
        .receipt
        .id;
        crate::email_deliveries::ensure(&source, receipt, "C-SYNTHETIC")
            .await
            .unwrap();
        crate::email_deliveries::mark_confirmed(&source, receipt, "C-SYNTHETIC", "synthetic")
            .await
            .unwrap();
        let operation = Uuid::now_v7();
        assert!(matches!(
            claim(
                &pg,
                receipt,
                operation,
                None,
                "wrong-digest",
                DeliveryRule::PreserveConfirmed
            )
            .await,
            Err(RecoveryError::Conflict)
        ));
        assert_eq!(current_operation(&pg, receipt).await.unwrap(), None);
        assert!(claim(
            &pg,
            receipt,
            operation,
            None,
            "synthetic-digest",
            DeliveryRule::PreserveConfirmed
        )
        .await
        .unwrap());
        assert_eq!(
            crate::email_deliveries::find(&source, receipt)
                .await
                .unwrap()
                .unwrap()
                .state,
            crate::email_deliveries::CONFIRMED
        );
    })
    .await;
}
