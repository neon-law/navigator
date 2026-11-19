use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::mem_surreal;

#[test]
fn debug_keeps_secret_values_out_of_operator_output() {
    let id = Uuid::now_v7();
    let image = Image {
        id,
        firm_id: Uuid::nil(),
        provider: "xero".into(),
        kind: "xero_client_secret".into(),
        version: 1,
        ciphertext: "synthetic-ciphertext-canary".into(),
        wrapped_dek: "synthetic-wrapped-key-canary".into(),
        kms_key_version: "v1".into(),
        kms_context: "synthetic-context-canary".into(),
        status: "active".into(),
        actor_id: Uuid::nil(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let debug = format!("{image:?}");
    assert!(debug.contains("has_ciphertext: true"));
    for value in [
        "synthetic-ciphertext-canary",
        "synthetic-wrapped-key-canary",
        "synthetic-context-canary",
    ] {
        assert!(!debug.contains(value));
    }
}

#[test]
fn unreadable_source_timestamps_are_rejected_without_substitution() {
    assert!(matches!(
        super::super::tenancy::timestamp("invalid synthetic timestamp"),
        Err(MirrorError::SourceTimestamp)
    ));
}

#[tokio::test]
async fn encrypted_secret_pending_copy_preserves_native_versions_and_rejects_drift() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (actor, notation, _) = crate::postgres::fixtures::workflow_fixture(&pg).await;
    let (firm, _) = crate::postgres::fixtures::firm_for_notation(&pg, notation).await;
    let source = super::super::from_source(mem_surreal()).await;
    let id = Uuid::now_v7();
    let now = "2026-11-18T12:00:00.123456Z";
    source.query("CREATE $id SET firm_id=$firm, provider='xero', kind='xero_client_secret', version=1, ciphertext='synthetic-encrypted-bytes', wrapped_dek='synthetic-wrapped-bytes', kms_key_version='synthetic-key-version', kms_context='synthetic-exact-context', status='active', actor_id=$actor, created_at=$now, updated_at=$now")
        .bind(("id", crate::surreal::record_id("firm_integration_secret", id)))
        .bind(("firm", crate::surreal::record_id("firm", firm)))
        .bind(("actor", crate::surreal::record_id("person", actor)))
        .bind(("now", now)).await.unwrap().check().unwrap();
    assert_eq!(
        super::super::during_initial_copy(mirror(&source, &pg, firm))
            .await
            .unwrap(),
        Mirrored::Written
    );
    let original = target(&pg, firm).await.unwrap();
    assert_eq!(original[&id].kms_context, "synthetic-exact-context");
    assert!(parity(&source, &pg).await.unwrap().is_clean());
    assert_eq!(
        super::super::during_initial_copy(mirror(&source, &pg, firm))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    source
        .query("UPDATE $id SET status='revoked', ciphertext='synthetic-stale-replacement'")
        .bind((
            "id",
            crate::surreal::record_id("firm_integration_secret", id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        super::super::during_initial_copy(mirror(&source, &pg, firm))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    assert!(!super::super::during_initial_copy(reconcile(&source, &pg))
        .await
        .unwrap()
        .is_clean());
    assert_eq!(target(&pg, firm).await.unwrap(), original);
    let second = Uuid::now_v7();
    source.query("CREATE $id SET firm_id=$firm, provider='xero', kind='xero_client_secret', version=2, ciphertext='synthetic-second-version', wrapped_dek='synthetic-second-wrap', kms_key_version='synthetic-key-version', kms_context='synthetic-second-context', status='active', actor_id=$actor, created_at=$now, updated_at=$now")
        .bind(("id", crate::surreal::record_id("firm_integration_secret", second)))
        .bind(("firm", crate::surreal::record_id("firm", firm)))
        .bind(("actor", crate::surreal::record_id("person", actor)))
        .bind(("now", now)).await.unwrap().check().unwrap();
    assert_eq!(
        super::super::during_initial_copy(mirror(&source, &pg, firm))
            .await
            .unwrap(),
        Mirrored::Written
    );
    let with_second = target(&pg, firm).await.unwrap();
    assert_eq!(with_second[&id], original[&id]);
    assert_eq!(with_second[&second].version, 2);
    assert!(!parity(&source, &pg).await.unwrap().is_clean());
    source
        .query("DELETE $id")
        .bind((
            "id",
            crate::surreal::record_id("firm_integration_secret", id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(!super::super::during_initial_copy(reconcile(&source, &pg))
        .await
        .unwrap()
        .is_clean());
    assert_eq!(target(&pg, firm).await.unwrap(), with_second);
    pg.pool().close().await;
}
