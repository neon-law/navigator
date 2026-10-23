use super::*;
use crate::postgres_mirror::social_accounts;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::mem_surreal;
use uuid::Uuid;

#[test]
fn social_account_key_contains_only_the_stable_record_key() {
    let key = MirrorKey::SocialAccount("synthetic".into());
    assert_eq!(key.table(), "social_account");
    assert_eq!(key.to_string(), "synthetic");
}

#[test]
fn social_account_mirror_is_in_the_operator_registry_after_its_parents() {
    let names = super::super::MIRRORS
        .iter()
        .map(|mirror| mirror.name)
        .collect::<Vec<_>>();
    let social = names
        .iter()
        .position(|name| *name == "social_accounts")
        .unwrap();
    assert!(names.iter().position(|name| *name == "identity").unwrap() < social);
    assert!(names.iter().position(|name| *name == "tenancy").unwrap() < social);
}

#[test]
fn image_debug_does_not_print_encrypted_credential_fields() {
    let image = SocialAccountImage {
        id: "synthetic".into(),
        firm_id: Uuid::nil(),
        brand_key: "synthetic-brand".into(),
        provider: "facebook".into(),
        provider_account_id: "synthetic-page".into(),
        display_name: None,
        scopes: vec![],
        expires_at: None,
        ciphertext: "synthetic-ciphertext-secret".into(),
        wrapped_dek: "synthetic-wrapped-key-secret".into(),
        kms_key_version: "v1".into(),
        kms_context: "synthetic-kms-context-secret".into(),
        status: "connected".into(),
        connected_by_person_id: Uuid::nil(),
        revoked_by_person_id: None,
        revoked_at: None,
        inserted_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let debug = format!("{image:?}");
    assert!(debug.contains("has_encrypted_bundle: true"));
    for secret in [
        "synthetic-ciphertext-secret",
        "synthetic-wrapped-key-secret",
        "synthetic-kms-context-secret",
    ] {
        assert!(!debug.contains(secret));
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one live lifecycle proves encrypted replay, revocation, and reconciliation"
)]
async fn encrypted_social_account_mirrors_replays_revocation_and_reconcile() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let person = crate::persons::create(
        &surreal,
        &crate::persons::NewPerson::with_role(
            "Synthetic account owner",
            format!("{}@example.test", Uuid::now_v7()),
            crate::persons::Role::Admin,
        ),
    )
    .await
    .unwrap();
    let entity_type = crate::entity_types::create(&surreal, "Synthetic account entity type")
        .await
        .unwrap();
    let jurisdiction = crate::jurisdictions::create(
        &surreal,
        &crate::jurisdictions::NewJurisdiction::new(
            format!("Synthetic account jurisdiction {}", Uuid::now_v7()),
            format!("A{}", &Uuid::now_v7().simple().to_string()[..7]),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity = crate::entities::create(
        &surreal,
        &crate::entities::NewEntity {
            name: format!("Synthetic account entity {}", Uuid::now_v7()),
            entity_type_id: entity_type.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap();
    let firm = crate::firms::create(
        &surreal,
        &crate::firms::NewFirm {
            name: format!("Synthetic account firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id: entity.id,
            admin_dri_person_id: person.id,
        },
    )
    .await
    .unwrap();
    let brand = crate::brands::create(
        &surreal,
        crate::persons::Role::Admin,
        Some(person.id),
        &crate::brands::NewBrand {
            name: format!("Synthetic account brand {}", Uuid::now_v7()),
            key: format!("synthetic-{}", Uuid::now_v7().simple()),
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    crate::postgres_mirror::identity::mirror(&surreal, &pg, &MirrorKey::Person(person.id))
        .await
        .unwrap();
    crate::postgres_mirror::tenancy::mirror(&surreal, &pg, &MirrorKey::Firm(firm.id))
        .await
        .unwrap();
    crate::postgres_mirror::tenancy::mirror(&surreal, &pg, &MirrorKey::Brand(brand.id))
        .await
        .unwrap();

    let source_id = format!("{}_facebook_{}", brand.key, "fixture-id-1");
    let ciphertext = "synthetic-ciphertext-bytes";
    let wrapped = "synthetic-wrapped-key-bytes";
    surreal.query("CREATE $id SET firm_id=$firm, brand_key=$brand, provider='facebook', provider_account_id='fixture-account', display_name='Synthetic Page', scopes=['pages_show_list'], expires_at=NONE, ciphertext=$ciphertext, wrapped_dek=$wrapped, kms_key_version='v1', kms_context='synthetic-context', status='connected', connected_by_person_id=$person, revoked_by_person_id=NONE, revoked_at=NONE, inserted_at=time::now(), updated_at=time::now()")
        .bind(("id", surrealdb::types::RecordId::new("social_account", source_id.clone())))
        .bind(("firm", crate::surreal::record_id("firm", firm.id)))
        .bind(("brand", brand.key.clone()))
        .bind(("person", crate::surreal::record_id("person", person.id)))
        .bind(("ciphertext", ciphertext.to_string())).bind(("wrapped", wrapped.to_string()))
        .await.unwrap().check().unwrap();
    let key = MirrorKey::SocialAccount(source_id.clone());
    assert_eq!(
        super::super::mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        super::super::mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Unchanged
    );
    let stored: (String, String) =
        sqlx::query_as("SELECT ciphertext, wrapped_dek FROM social_account WHERE id=$1")
            .bind(&source_id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(stored, (ciphertext.to_string(), wrapped.to_string()));
    assert!(social_accounts::parity(&surreal, &pg)
        .await
        .unwrap()
        .is_clean());

    surreal.query("UPDATE $id SET status='revoked', ciphertext='', wrapped_dek='', revoked_by_person_id=$person, revoked_at=time::now(), updated_at=time::now()")
        .bind(("id", surrealdb::types::RecordId::new("social_account", source_id.clone())))
        .bind(("person", crate::surreal::record_id("person", person.id)))
        .await.unwrap().check().unwrap();
    assert!(social_accounts::reconcile(&surreal, &pg)
        .await
        .unwrap()
        .is_clean());
    let status: String = sqlx::query_scalar("SELECT status FROM social_account WHERE id=$1")
        .bind(&source_id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(status, "revoked");

    assert!(
        sqlx::query("DELETE FROM brand WHERE brand_key=$1")
            .bind(&brand.key)
            .execute(&pg.0)
            .await
            .is_err(),
        "the Brand cannot be deleted while its account is mirrored"
    );
    surreal
        .query("DELETE $id")
        .bind((
            "id",
            surrealdb::types::RecordId::new("social_account", source_id.clone()),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        super::super::mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Deleted
    );
    assert!(social_accounts::parity(&surreal, &pg)
        .await
        .unwrap()
        .is_clean());
    sqlx::query("DELETE FROM brand WHERE brand_key=$1")
        .bind(&brand.key)
        .execute(&pg.0)
        .await
        .unwrap();
    pg.0.close().await;
}
