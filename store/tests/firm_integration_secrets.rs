use cloud::{FakeKms, KmsContext, RuntimeKms};
use store::firm_secrets::{IntegrationSecretKind, ALL_KINDS};
use uuid::Uuid;

/// `ALL_KINDS` is what a settings view builds its "add a secret" choices
/// from; a kind added to the enum but not here would be silently
/// unreachable from any write door.
#[test]
fn all_kinds_covers_the_closed_vocabulary_with_no_duplicates() {
    assert!(ALL_KINDS.contains(&IntegrationSecretKind::DocuSignConnection));
    assert!(ALL_KINDS.contains(&IntegrationSecretKind::StripeNevadaAppKey));
    assert!(ALL_KINDS.contains(&IntegrationSecretKind::StripeOperatingAppKey));
    assert!(ALL_KINDS.contains(&IntegrationSecretKind::PortalLogin));
    assert!(ALL_KINDS.contains(&IntegrationSecretKind::TwilioAuthToken));
    let mut slugs: Vec<&str> = ALL_KINDS.iter().map(|kind| kind.as_str()).collect();
    slugs.sort_unstable();
    slugs.dedup();
    assert_eq!(slugs.len(), ALL_KINDS.len());
}

#[tokio::test]
async fn kms_context_mismatch_and_unavailability_fail_closed() {
    let kms =
        FakeKms::new("projects/fixture/locations/global/keyRings/runtime/cryptoKeys/integration");
    let one = KmsContext::new(Uuid::now_v7(), "notion", "notion_token");
    let two = KmsContext::new(Uuid::now_v7(), "notion", "notion_token");
    let wrapped = kms
        .wrap_data_key(b"data-key", &one)
        .await
        .expect("fake KMS wraps");
    assert!(kms.unwrap_data_key(&wrapped, &two).await.is_err());
    kms.set_available(false);
    assert!(kms.wrap_data_key(b"data-key", &one).await.is_err());
}
