use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema, synthetic_entity};
use crate::test_support::mem_surreal;

#[test]
fn key_and_debug_keep_secret_values_out_of_operator_output() {
    let id = Uuid::now_v7();
    let key = MirrorKey::FirmIntegrationSecrets(id);
    assert_eq!(key.table(), "firm_integration_secret");
    assert_eq!(key.to_string(), format!("firm_integration_secret:{id}"));
    let names = super::super::MIRRORS
        .iter()
        .map(|mirror| mirror.name)
        .collect::<Vec<_>>();
    let firm_secrets = names
        .iter()
        .position(|name| *name == "firm_integration_secrets")
        .unwrap();
    assert!(names.iter().position(|name| *name == "tenancy").unwrap() < firm_secrets);
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
async fn encrypted_secret_versions_replay_reconcile_and_revoke() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let admin = crate::persons::create(
        &surreal,
        &crate::persons::NewPerson::with_role(
            "Synthetic secret owner",
            format!("secret-{}@example.test", Uuid::now_v7()),
            crate::persons::Role::Admin,
        ),
    )
    .await
    .unwrap();
    let entity_id = synthetic_entity(&surreal).await;
    let firm = crate::firms::create(
        &surreal,
        &crate::firms::NewFirm {
            name: format!("Synthetic secret firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    let secret_id = Uuid::now_v7();
    let rid = crate::surreal::record_id("firm_integration_secret", secret_id);
    let firm_rid = crate::surreal::record_id("firm", firm.id);
    let actor_rid = crate::surreal::record_id("person", admin.id);
    let now = Utc::now().to_rfc3339();
    surreal.query("CREATE $id SET firm_id=$firm, provider='xero', kind='xero_client_secret', version=1, ciphertext='synthetic-encrypted-bytes', wrapped_dek='synthetic-wrapped-bytes', kms_key_version='synthetic-key-version', kms_context='synthetic-kms-context', status='active', actor_id=$actor, created_at=$now, updated_at=$now")
        .bind(("id", rid.clone())).bind(("firm", firm_rid)).bind(("actor", actor_rid.clone())).bind(("now", now.clone()))
        .await.unwrap().check().unwrap();
    let key = MirrorKey::FirmIntegrationSecrets(firm.id);
    assert_eq!(
        mirror(&surreal, &pg, firm.id).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, firm.id).await.unwrap(),
        Mirrored::Unchanged
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    let second_id = Uuid::now_v7();
    surreal.query("CREATE $id SET firm_id=$firm, provider='xero', kind='xero_client_secret', version=2, ciphertext='synthetic-encrypted-v2', wrapped_dek='synthetic-wrapped-v2', kms_key_version='synthetic-key-version', kms_context='synthetic-kms-context', status='active', actor_id=$actor, created_at=$now, updated_at=$now")
        .bind(("id", crate::surreal::record_id("firm_integration_secret", second_id)))
        .bind(("firm", crate::surreal::record_id("firm", firm.id))).bind(("actor", actor_rid.clone())).bind(("now", now.clone()))
        .await.unwrap().check().unwrap();
    surreal
        .query("UPDATE $id SET status='revoked', updated_at=$now")
        .bind(("id", rid))
        .bind(("now", now))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(!parity(&surreal, &pg).await.unwrap().is_clean());
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    let rows: Vec<(i64, String, String)> = sqlx::query_as("SELECT version,ciphertext,status FROM firm_integration_secret WHERE firm_id=$1 ORDER BY version")
        .bind(firm.id).fetch_all(&pg.0).await.unwrap();
    assert_eq!(
        rows,
        vec![
            (1, "synthetic-encrypted-bytes".into(), "revoked".into()),
            (2, "synthetic-encrypted-v2".into(), "active".into())
        ]
    );
    surreal
        .query("DELETE $id")
        .bind((
            "id",
            crate::surreal::record_id("firm_integration_secret", second_id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM firm_integration_secret WHERE firm_id=$1")
            .bind(firm.id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(remaining, 1);
    assert_eq!(key.table(), "firm_integration_secret");
    pg.0.close().await;
}

#[tokio::test]
async fn parity_detects_and_reconcile_removes_target_only_firm_secrets() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let admin = crate::persons::create(
        &surreal,
        &crate::persons::NewPerson::with_role(
            "Synthetic secret owner",
            format!("target-only-{}@example.test", Uuid::now_v7()),
            crate::persons::Role::Admin,
        ),
    )
    .await
    .unwrap();
    let entity_id = synthetic_entity(&surreal).await;
    let firm = crate::firms::create(
        &surreal,
        &crate::firms::NewFirm {
            name: format!("Synthetic target-only firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    super::super::identity::mirror(&surreal, &pg, &MirrorKey::Person(admin.id))
        .await
        .unwrap();
    super::super::tenancy::mirror(&surreal, &pg, &MirrorKey::Firm(firm.id))
        .await
        .unwrap();
    let orphan_firm = Uuid::now_v7();
    let orphan_entity = synthetic_entity(&surreal).await;
    super::super::identity::mirror(&surreal, &pg, &MirrorKey::Entity(orphan_entity))
        .await
        .unwrap();
    sqlx::query("INSERT INTO firm (id,name,status,slug,entity_id) VALUES ($1,'Synthetic target-only firm','active',$2,$3)")
        .bind(orphan_firm).bind(format!("t-{}", Uuid::now_v7().simple())).bind(orphan_entity).execute(&pg.0).await.unwrap();
    let orphan_secret = Uuid::now_v7();
    let now = Utc::now();
    sqlx::query("INSERT INTO firm_integration_secret (id,firm_id,provider,kind,version,ciphertext,wrapped_dek,kms_key_version,kms_context,status,actor_id,created_at,updated_at) VALUES ($1,$2,'xero','xero_client_secret',1,'synthetic-ciphertext','synthetic-wrapped','v1','{}','active',$3,$4,$4)")
        .bind(orphan_secret).bind(orphan_firm).bind(admin.id).bind(now).execute(&pg.0).await.unwrap();

    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report
        .divergent()
        .any(|key| key == &MirrorKey::FirmIntegrationSecrets(orphan_firm)));
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM firm_integration_secret WHERE id=$1")
            .bind(orphan_secret)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(remaining, 0);
    pg.0.close().await;
}
