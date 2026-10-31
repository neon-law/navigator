use super::*;
use crate::{
    brands::NewBrand,
    firms::NewFirm,
    persons::{NewPerson, Role},
    postgres_mirror::tests::{connect, fresh_schema, synthetic_entity},
    surreal::record_id,
    test_support::mem_surreal,
};
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use std::process::Command;
use surrealdb::types::Datetime;
use uuid::Uuid;

async fn practice(surreal: &SurrealDb, suffix: &str) -> (Uuid, Uuid, String) {
    let person = crate::persons::create(
        surreal,
        &NewPerson::with_role(
            "Synthetic administrator",
            format!("selection-{suffix}@example.test"),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = crate::firms::create(
        surreal,
        &NewFirm {
            name: format!("Synthetic selection firm {suffix}"),
            status: "active".into(),
            entity_id: synthetic_entity(surreal).await,
            admin_dri_person_id: person.id,
        },
    )
    .await
    .unwrap();
    let brand_key = format!("selection-{suffix}");
    crate::brands::create(
        surreal,
        Role::Admin,
        Some(person.id),
        &NewBrand {
            name: format!("Synthetic selection Brand {suffix}"),
            key: brand_key.clone(),
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (firm.id, person.id, brand_key)
}

fn postgres_time(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

#[test]
fn selection_key_keeps_the_source_record_identity() {
    let key = MirrorKey::SocialAccountSelection {
        brand_key: "synthetic-brand".into(),
        provider: "facebook".into(),
    };
    assert_eq!(key.table(), "social_account_selection");
    assert_eq!(
        key.to_string(),
        "social_account_selection:synthetic-brand:facebook"
    );
}

#[test]
fn selection_mirror_is_registered_after_tenancy() {
    let names = super::super::MIRRORS
        .iter()
        .map(|mirror| mirror.name)
        .collect::<Vec<_>>();
    let tenancy = names.iter().position(|name| *name == "tenancy").unwrap();
    let selection = names
        .iter()
        .position(|name| *name == "social_account_selections")
        .unwrap();
    assert!(tenancy < selection);
}

#[tokio::test]
// Keep the one-database lifecycle together so this acceptance covers writes,
// tenancy, disconnects, source deletion, and global reconciliation in order.
#[allow(clippy::too_many_lines)]
async fn selections_replay_update_isolate_and_reconcile_on_real_postgres() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();
    let (firm_id, admin_id, brand_key) = practice(&surreal, "one").await;
    let (_, other_admin, other_brand) = practice(&surreal, "two").await;
    let key = MirrorKey::SocialAccountSelection {
        brand_key: brand_key.clone(),
        provider: "facebook".into(),
    };

    crate::social_account_selections::save(
        &surreal,
        Role::Admin,
        admin_id,
        &brand_key,
        crate::social_accounts::SocialProvider::Facebook,
        "page-first",
    )
    .await
    .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Unchanged
    );
    let global = super::super::parity_all(&surreal, &pg).await.unwrap();
    assert!(global
        .iter()
        .filter(|mirror| mirror.mirror == "social_account_selections")
        .flat_map(|mirror| &mirror.report.tables)
        .any(|table| table.table == "social_account_selection" && table.source_rows == 1));

    let source_id = format!("{brand_key}:facebook");
    let initial: (String, Uuid, DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
        "SELECT id, firm_id, inserted_at, updated_at FROM social_account_selection WHERE id=$1",
    )
    .bind(&source_id)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(initial.0, source_id);
    assert_eq!(initial.1, firm_id);

    let duplicate_identity = sqlx::query("INSERT INTO social_account_selection (id, firm_id, brand_key, provider, provider_account_id, inserted_at, updated_at) VALUES ($1, $2, $3, 'facebook', 'synthetic-duplicate', now(), now())")
        .bind(format!("{brand_key}:facebook:duplicate"))
        .bind(firm_id)
        .bind(&brand_key)
        .execute(&pg.0)
        .await
        .unwrap_err();
    assert_eq!(
        duplicate_identity
            .as_database_error()
            .unwrap()
            .code()
            .as_deref(),
        Some("23505")
    );

    crate::social_account_selections::save(
        &surreal,
        Role::Admin,
        admin_id,
        &brand_key,
        crate::social_accounts::SocialProvider::Facebook,
        "page-updated",
    )
    .await
    .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Unchanged
    );
    let updated: (String, DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
        "SELECT provider_account_id, inserted_at, updated_at FROM social_account_selection WHERE id=$1",
    )
    .bind(&source_id)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(updated.0, "page-updated");
    assert_eq!(updated.1, initial.2);
    assert!(updated.2 >= initial.3);

    crate::social_account_selections::save(
        &surreal,
        Role::Admin,
        other_admin,
        &other_brand,
        crate::social_accounts::SocialProvider::Facebook,
        "other-page",
    )
    .await
    .unwrap();
    let other_key = MirrorKey::SocialAccountSelection {
        brand_key: other_brand.clone(),
        provider: "facebook".into(),
    };
    mirror(&surreal, &pg, &other_key).await.unwrap();

    let denied = crate::social_account_selections::save(
        &surreal,
        Role::Admin,
        other_admin,
        &brand_key,
        crate::social_accounts::SocialProvider::Facebook,
        "retarget-attempt",
    )
    .await;
    assert!(denied.is_err());
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    assert_eq!(
        crate::social_account_selections::for_brand(&surreal, &brand_key)
            .await
            .unwrap()[0]
            .provider_account_id,
        "page-updated"
    );

    sqlx::query(
        "UPDATE social_account_selection SET provider_account_id='synthetic-drift' WHERE id=$1",
    )
    .bind(&source_id)
    .execute(&pg.0)
    .await
    .unwrap();
    let drift = parity(&surreal, &pg).await.unwrap();
    assert_eq!(
        drift.tables[0].divergent.as_slice(),
        std::slice::from_ref(&key)
    );
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());

    // An unrelated authorized connection lifecycle cannot alter the saved destination.
    let account_id = format!("{}_facebook_{}", brand_key, "fixture-id");
    surreal.query("CREATE $id SET firm_id=$firm, brand_key=$brand, provider='facebook', provider_account_id='page-authorized', display_name='Synthetic Page', scopes=['pages_show_list'], expires_at=NONE, ciphertext='synthetic-ciphertext', wrapped_dek='synthetic-dek', kms_key_version='v1', kms_context='synthetic-context', status='connected', connected_by_person_id=$person, revoked_by_person_id=NONE, revoked_at=NONE, inserted_at=time::now(), updated_at=time::now()")
        .bind(("id", surrealdb::types::RecordId::new("social_account", account_id)))
        .bind(("firm", record_id("firm", firm_id)))
        .bind(("brand", brand_key.clone()))
        .bind(("person", record_id("person", admin_id)))
        .await.unwrap().check().unwrap();
    surreal
        .query("DELETE $id")
        .bind((
            "id",
            surrealdb::types::RecordId::new(
                "social_account",
                format!("{}_facebook_{}", brand_key, "fixture-id"),
            ),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT provider_account_id FROM social_account_selection WHERE id=$1"
        )
        .bind(&source_id)
        .fetch_one(&pg.0)
        .await
        .unwrap(),
        "page-updated"
    );

    surreal
        .query("DELETE $id")
        .bind((
            "id",
            surrealdb::types::RecordId::new("social_account_selection", source_id.clone()),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Deleted
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM social_account_selection WHERE id=$1")
            .bind(&source_id)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        0
    );

    // A target-only selection is also removed by the global reconciliation pass.
    sqlx::query("INSERT INTO social_account_selection (id, firm_id, brand_key, provider, provider_account_id, inserted_at, updated_at) VALUES ($1, $2, $3, 'youtube', 'synthetic-orphan', now(), now())")
        .bind(format!("{brand_key}:youtube")).bind(firm_id).bind(&brand_key)
        .execute(&pg.0).await.unwrap();
    let all = crate::postgres_mirror::reconcile_all(&surreal, &pg)
        .await
        .unwrap();
    let selection_report = all
        .iter()
        .flat_map(|mirror| &mirror.report.tables)
        .find(|table| table.table == "social_account_selection")
        .expect("global reconcile includes social account selections");
    assert!(selection_report.divergent.is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM social_account_selection WHERE id=$1")
            .bind(format!("{brand_key}:youtube"))
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        0
    );
    pg.0.close().await;
}

#[tokio::test]
async fn selection_timestamps_keep_source_microseconds_on_real_postgres() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();
    let (firm_id, _, brand_key) = practice(&surreal, "time").await;
    let source_id = format!("{brand_key}:facebook");
    let inserted = postgres_time("2026-10-21T12:00:00.123456789Z");
    let updated = postgres_time("2026-10-21T12:00:01.987654321Z");
    surreal.query("CREATE $id SET firm_id=$firm, brand_key=$brand, provider='facebook', provider_account_id='synthetic-page', inserted_at=$inserted, updated_at=$updated")
        .bind(("id", surrealdb::types::RecordId::new("social_account_selection", source_id.clone())))
        .bind(("firm", record_id("firm", firm_id)))
        .bind(("brand", brand_key.clone()))
        .bind(("inserted", Datetime::from(inserted)))
        .bind(("updated", Datetime::from(updated)))
        .await.unwrap().check().unwrap();
    let key = MirrorKey::SocialAccountSelection {
        brand_key,
        provider: "facebook".into(),
    };
    mirror(&surreal, &pg, &key).await.unwrap();
    let target: (DateTime<Utc>, DateTime<Utc>) =
        sqlx::query_as("SELECT inserted_at, updated_at FROM social_account_selection WHERE id=$1")
            .bind(source_id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(target.0, postgres_time("2026-10-21T12:00:00.123456Z"));
    assert_eq!(target.1, postgres_time("2026-10-21T12:00:01.987654Z"));
    pg.0.close().await;
}

#[tokio::test]
async fn saved_selection_write_hook_uses_the_isolated_postgres_database() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "postgres_mirror::social_account_selections::tests::selection_write_hook_probe",
            "--nocapture",
        ])
        .env("NAVIGATOR_SELECTION_HOOK_TEST_CASE", "mirrored")
        .env(crate::postgres_mirror::MIRROR_ENV, "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", options.get_port().to_string())
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "selection write-hook probe: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn selection_write_hook_probe() {
    if std::env::var("NAVIGATOR_SELECTION_HOOK_TEST_CASE").as_deref() != Ok("mirrored") {
        return;
    }
    let surreal = mem_surreal().await;
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();
    let (_, admin_id, brand_key) = practice(&surreal, "hook").await;
    crate::social_account_selections::save(
        &surreal,
        Role::Admin,
        admin_id,
        &brand_key,
        crate::social_accounts::SocialProvider::Facebook,
        "page-hook",
    )
    .await
    .unwrap();
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(
            std::env::var("NAVIGATOR_POSTGRES_PORT")
                .unwrap()
                .parse()
                .unwrap(),
        )
        .username("postgres")
        .password("synthetic")
        .ssl_mode(PgSslMode::Disable);
    let pg = connect(
        &options,
        &std::env::var("NAVIGATOR_POSTGRES_DATABASE").unwrap(),
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT provider_account_id FROM social_account_selection WHERE id=$1"
        )
        .bind(format!("{brand_key}:facebook"))
        .fetch_one(&pg.0)
        .await
        .unwrap(),
        "page-hook"
    );
    pg.0.close().await;
}
