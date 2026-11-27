use super::{PostgresDb, PostgresError, APPLICATION_POSTGRES};
use crate::persons::Role;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

async fn name(pg: &PostgresDb) -> String {
    sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(pg.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn concurrent_and_nested_application_pools_keep_owned_databases_isolated() {
    crate::test_support::with_postgres(async {
        let first = PostgresDb::application().await.unwrap();
        crate::test_support::with_postgres(async {
            let second = PostgresDb::application().await.unwrap();
            let first_name = name(&first).await;
            let second_name = name(&second).await;
            assert_ne!(first_name, second_name);
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let mut tasks = Vec::new();
            for pg in [first.clone(), second.clone()] {
                let barrier = Arc::clone(&barrier);
                tasks.push(tokio::spawn(async move {
                    PostgresDb::with_application_db(pg, async move {
                        barrier.wait().await;
                        name(&PostgresDb::application().await.unwrap()).await
                    })
                    .await
                    .unwrap()
                }));
            }
            assert_eq!(tasks.remove(0).await.unwrap(), first_name);
            assert_eq!(tasks.remove(0).await.unwrap(), second_name);
            PostgresDb::with_application_db(first.clone(), async {
                assert_eq!(
                    name(&PostgresDb::application().await.unwrap()).await,
                    first_name
                );
                // Raw resolution keeps the existing fixture/environment pool, not the application override.
                assert_eq!(
                    name(&PostgresDb::from_env().await.unwrap()).await,
                    second_name
                );
                PostgresDb::with_application_db(second.clone(), async {
                    assert_eq!(
                        name(&PostgresDb::application().await.unwrap()).await,
                        second_name
                    );
                })
                .await
                .unwrap();
                assert_eq!(
                    name(&PostgresDb::application().await.unwrap()).await,
                    first_name
                );
                assert!(
                    tokio::spawn(async { APPLICATION_POSTGRES.try_with(|_| ()).is_err() })
                        .await
                        .unwrap(),
                    "a spawned callback must explicitly enter its owning application pool"
                );
            })
            .await
            .unwrap();
            assert_eq!(
                name(&PostgresDb::application().await.unwrap()).await,
                second_name
            );
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn application_scope_rechecks_history_and_never_falls_back_to_another_admitted_pool() {
    crate::test_support::with_postgres(async {
        let admitted = PostgresDb::application().await.unwrap();
        crate::test_support::with_postgres(async {
            let damaged = PostgresDb::application().await.unwrap();
            PostgresDb::with_test_db(admitted.clone(), async {
                PostgresDb::with_application_db(damaged.clone(), async {
                    // An owned negative fixture models an incomplete restored proof after entry.
                    let damaged_schema = sqlx::query("UPDATE public._sqlx_migrations SET success=false WHERE version=(SELECT max(version) FROM public._sqlx_migrations)")
                        .execute(damaged.pool())
                        .await
                        .unwrap();
                    assert_eq!(damaged_schema.rows_affected(), 1, "the owned native schema attestation must actually be damaged");
                    assert!(matches!(
                        PostgresDb::application().await,
                        Err(PostgresError::History)
                    ));
                    let source = crate::application_store::ApplicationStore::uninitialized();
                    assert!(
                        crate::projects::onboarding_status(&source, uuid::Uuid::new_v4(), "navigator")
                            .await
                            .is_err()
                    );
                    assert!(crate::projects::onboarding_official_sets(&source, &[])
                        .await
                        .is_err());
                    assert!(crate::projects::onboarding_esign_pending(&source, &[])
                        .await
                        .is_err());
                    assert!(crate::projects::matter_lifecycle_sets(&source, &[])
                        .await
                        .is_err());
                    assert_eq!(
                        name(&PostgresDb::from_env().await.unwrap()).await,
                        name(&admitted).await
                    );
                })
                .await
                .unwrap();
                let called = Arc::new(AtomicUsize::new(0));
                let observed = Arc::clone(&called);
                let result = PostgresDb::with_application_db(damaged, async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                })
                .await;
                assert!(matches!(result, Err(PostgresError::History)));
                assert_eq!(called.load(Ordering::SeqCst), 0);
                assert_eq!(
                    name(&PostgresDb::application().await.unwrap()).await,
                    name(&admitted).await
                );
            })
            .await;
        })
        .await;
    })
    .await;
}

async fn assert_owned_calendar_and_brand(
    owned: &PostgresDb,
    db: &crate::application_store::ApplicationStore,
    actor: uuid::Uuid,
    firm: uuid::Uuid,
    brand: &str,
) {
    assert!(matches!(
        crate::firms::set_calendar_owner(
            db,
            Role::Admin,
            Some(actor),
            firm,
            "calendar@example.test"
        )
        .await,
        Err(crate::firms::FirmError::NotAuthorized)
    ));
    assert!(matches!(
        crate::firms::set_calendar_owner(db, Role::Owner, None, firm, "invalid-address").await,
        Err(crate::firms::FirmError::InvalidCalendarOwner)
    ));
    crate::firms::set_calendar_owner(db, Role::Owner, None, firm, "calendar@example.test")
        .await
        .unwrap();
    let mailbox: String = sqlx::query_scalar(
        "SELECT google_workspace_owner FROM firm_calendar_settings WHERE firm_id=$1",
    )
    .bind(firm)
    .fetch_one(owned.pool())
    .await
    .unwrap();
    assert_eq!(mailbox, "calendar@example.test");
    assert!(matches!(
        crate::firms::attach_own_brand(db, Role::Lawyer, Some(actor), brand).await,
        Err(crate::firms::FirmError::NotAuthorized)
    ));
    assert_eq!(
        crate::firms::attach_own_brand(db, Role::Admin, Some(actor), brand)
            .await
            .unwrap(),
        firm
    );
    let attached: i64 =
        sqlx::query_scalar("SELECT count(*) FROM firm_brand WHERE firm_id=$1 AND brand_key=$2")
            .bind(firm)
            .bind(brand)
            .fetch_one(owned.pool())
            .await
            .unwrap();
    assert_eq!(attached, 1);
    crate::firms::detach_brand(db, Role::Admin, Some(actor), firm, brand)
        .await
        .unwrap();
    let detached: i64 =
        sqlx::query_scalar("SELECT count(*) FROM firm_brand WHERE firm_id=$1 AND brand_key=$2")
            .bind(firm)
            .bind(brand)
            .fetch_one(owned.pool())
            .await
            .unwrap();
    assert_eq!(detached, 0);
}

#[tokio::test]
async fn calendar_and_brand_management_use_the_owning_application_pool() {
    crate::test_support::with_postgres(async {
        let owned = PostgresDb::application().await.unwrap();
        let (actor, notation, _) = super::fixtures::workflow_fixture(&owned).await;
        let (firm, _) = super::fixtures::firm_for_notation(&owned, notation).await;
        sqlx::query("UPDATE person SET role='admin' WHERE id=$1")
            .bind(actor)
            .execute(owned.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO person_firm_role(person_id,firm_id,membership) VALUES($1,$2,'admin')",
        )
        .bind(actor)
        .bind(firm)
        .execute(owned.pool())
        .await
        .unwrap();
        let brand = format!("synthetic-{}", uuid::Uuid::new_v4().simple());
        sqlx::query("INSERT INTO brand(name,brand_key,firm_id) VALUES($1,$1,$2)")
            .bind(&brand)
            .bind(firm)
            .execute(owned.pool())
            .await
            .unwrap();
        crate::test_support::with_postgres(async {
            let ambient = PostgresDb::application().await.unwrap();
            PostgresDb::with_application_db(owned.clone(), async {
                let db = crate::application_store::ApplicationStore::uninitialized();
                assert_owned_calendar_and_brand(&owned, &db, actor, firm, &brand).await;
                for table in ["firm_calendar_settings", "firm_brand"] {
                    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                        "SELECT count(*) FROM {table}"
                    )))
                    .fetch_one(ambient.pool())
                    .await
                    .unwrap();
                    assert_eq!(count, 0, "ambient {table} must remain untouched");
                }
                let damaged_schema = sqlx::query("UPDATE public._sqlx_migrations SET success=false WHERE version=(SELECT max(version) FROM public._sqlx_migrations)")
                    .execute(owned.pool())
                    .await
                    .unwrap();
                assert_eq!(damaged_schema.rows_affected(), 1, "the owned native schema attestation must actually be damaged");
                assert!(matches!(
                    crate::firms::set_calendar_owner(
                        &db,
                        Role::Owner,
                        None,
                        firm,
                        "changed@example.test"
                    )
                    .await,
                    Err(crate::firms::FirmError::Postgres(
                        crate::postgres::tenancy::TenancyError::Postgres(PostgresError::History)
                    ))
                ));
                let preserved: String = sqlx::query_scalar(
                    "SELECT google_workspace_owner FROM firm_calendar_settings WHERE firm_id=$1",
                )
                .bind(firm)
                .fetch_one(owned.pool())
                .await
                .unwrap();
                assert_eq!(preserved, "calendar@example.test");
            })
            .await
            .unwrap();
        })
        .await;
    })
    .await;
}

async fn assert_voice_assignment(
    db: &crate::application_store::ApplicationStore,
    owned: &PostgresDb,
    ambient: &PostgresDb,
    actor: uuid::Uuid,
    firm: uuid::Uuid,
    project: uuid::Uuid,
    review: bool,
) {
    let sid = format!("CA{}", uuid::Uuid::new_v4().simple());
    let inbound = crate::inbox::IngestInbound {
        firm_id: firm,
        project_id: None,
        brand_key: None,
        interaction_type: None,
        provider_post_id: None,
        parent_provider_message_id: None,
        visibility: None,
        channel: crate::inbox::channel::VOICE,
        provider_account: "synthetic-voice",
        provider_message_id: &sid,
        provider_thread_ref: None,
        external_address: "synthetic-caller",
        external_name: Some("Synthetic caller"),
        subject: None,
        body: "Synthetic call",
        source_page: None,
        source_channel: None,
        matter_consent: true,
        promotional_opt_in: false,
        email_conversation_id: None,
        attachments: Vec::new(),
        occurred_at: "2026-10-30T18:00:00Z",
        raw_storage_key: None,
    };
    let saved = crate::inbox::ingest_inbound(db, &inbound).await.unwrap();
    let input = crate::billed_calls::NewBilledCall {
        call_sid: sid.clone(),
        conversation_id: saved.conversation_id,
        firm_id: firm,
        duration_seconds: 361,
        call_started_at: "2026-10-30T18:00:00Z".parse().unwrap(),
    };
    let before = crate::billed_calls::record(owned, &input).await.unwrap();
    let first = if review {
        crate::inbox::assign_for_triage_review(db, actor, saved.conversation_id, project).await
    } else {
        crate::inbox::assign(db, actor, saved.conversation_id, project).await
    }
    .unwrap();
    let again = if review {
        crate::inbox::assign_for_triage_review(db, actor, saved.conversation_id, project).await
    } else {
        crate::inbox::assign(db, actor, saved.conversation_id, project).await
    }
    .unwrap();
    assert_eq!(first.id, again.id);
    let after = crate::billed_calls::find_by_call_sid(owned, &sid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.project_id, Some(project));
    assert_eq!(
        (after.duration_seconds, after.tenths, after.call_started_at),
        (
            before.duration_seconds,
            before.tenths,
            before.call_started_at
        )
    );
    assert!(crate::billed_calls::find_by_call_sid(ambient, &sid)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn voice_assignments_and_triage_keep_billing_in_the_owning_application_pool() {
    crate::test_support::with_postgres(async {
        let owned = PostgresDb::application().await.unwrap();
        let (actor, notation, _) = super::fixtures::workflow_fixture(&owned).await;
        let (firm, project) = super::fixtures::firm_for_notation(&owned, notation).await;
        let brand = format!("synthetic-{}", uuid::Uuid::new_v4().simple());
        sqlx::query("INSERT INTO brand(name,brand_key,firm_id) VALUES($1,$1,$2)")
            .bind(&brand)
            .bind(firm)
            .execute(owned.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO firm_brand(firm_id,brand_key) VALUES($1,$2)")
            .bind(firm)
            .bind(&brand)
            .execute(owned.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE project SET brand=$1 WHERE id=$2 AND firm_id=$3")
            .bind(&brand)
            .bind(project)
            .bind(firm)
            .execute(owned.pool())
            .await
            .unwrap();

        sqlx::query("UPDATE person SET role='owner' WHERE id=$1")
            .bind(actor)
            .execute(owned.pool())
            .await
            .unwrap();
        crate::test_support::with_postgres(async {
            let ambient = PostgresDb::application().await.unwrap();
            PostgresDb::with_application_db(owned.clone(), async {
                let db = crate::application_store::ApplicationStore::uninitialized();
                for review in [false, true] {
                    assert_voice_assignment(&db, &owned, &ambient, actor, firm, project, review)
                        .await;
                }
            })
            .await
            .unwrap();
        })
        .await;
    })
    .await;
}

mod docusign_completion_tests;
mod firm_esign_tests;
mod wrapper_tests;
