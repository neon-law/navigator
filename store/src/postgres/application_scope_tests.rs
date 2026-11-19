use super::{PostgresDb, PostgresError, APPLICATION_POSTGRES};
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
                    sqlx::query("DELETE FROM mirror_backfill_checkpoint")
                        .execute(damaged.pool())
                        .await
                        .unwrap();
                    assert!(matches!(
                        PostgresDb::application().await,
                        Err(PostgresError::History)
                    ));
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
