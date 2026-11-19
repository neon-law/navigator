use super::*;

#[tokio::test]
async fn identical_receipt_keys_share_one_record() {
    let db = crate::surreal::test_support::mem().await;
    let first = ensure(
        &db,
        &NewEmailReceipt {
            receiving_mailbox: "support@example.com",
            deployment: "staging",
            firm_id: None,
            provider_account: None,
            sender_authenticated: false,
            raw_digest: "digest-a",
            source_message_id: Some("same@example.com"),
            archive_key: "inbound/digest-a.eml",
            letter_id: uuid::Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    let second = ensure(
        &db,
        &NewEmailReceipt {
            receiving_mailbox: "support@example.com",
            deployment: "staging",
            firm_id: None,
            provider_account: None,
            sender_authenticated: false,
            raw_digest: "digest-a",
            source_message_id: Some("same@example.com"),
            archive_key: "inbound/digest-a.eml",
            letter_id: uuid::Uuid::now_v7(),
        },
    )
    .await
    .unwrap();

    assert!(!first.deduped);
    assert!(second.deduped);
    assert_eq!(first.receipt.id, second.receipt.id);
    assert_eq!(count(&db).await.unwrap(), 1);
}

#[tokio::test]
async fn message_id_reuse_with_new_digest_is_distinct() {
    let db = crate::surreal::test_support::mem().await;
    let first = ensure(
        &db,
        &NewEmailReceipt {
            receiving_mailbox: "support@example.com",
            deployment: "staging",
            firm_id: None,
            provider_account: None,
            sender_authenticated: false,
            raw_digest: "digest-a",
            source_message_id: Some("reused@example.com"),
            archive_key: "inbound/digest-a.eml",
            letter_id: uuid::Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    let second = ensure(
        &db,
        &NewEmailReceipt {
            receiving_mailbox: "support@example.com",
            deployment: "staging",
            firm_id: None,
            provider_account: None,
            sender_authenticated: false,
            raw_digest: "digest-b",
            source_message_id: Some("reused@example.com"),
            archive_key: "inbound/digest-b.eml",
            letter_id: uuid::Uuid::now_v7(),
        },
    )
    .await
    .unwrap();

    assert_ne!(first.receipt.id, second.receipt.id);
    assert_eq!(count(&db).await.unwrap(), 2);
}

#[tokio::test]
async fn concurrent_identical_admissions_create_one_receipt() {
    let db = crate::surreal::test_support::mem().await;
    let new = NewEmailReceipt {
        receiving_mailbox: "support@example.com",
        deployment: "staging",
        firm_id: None,
        provider_account: None,
        sender_authenticated: false,
        raw_digest: "digest-race",
        source_message_id: None,
        archive_key: "inbound/digest-race.eml",
        letter_id: uuid::Uuid::now_v7(),
    };
    let (first, second) = tokio::join!(ensure(&db, &new), ensure(&db, &new));
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.receipt.id, second.receipt.id);
    assert_eq!(count(&db).await.unwrap(), 1);
    assert_ne!(first.deduped, second.deduped);
}
