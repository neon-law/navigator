//! Carry an explicit request pool across the server renderer's task boundary.

use dioxus::prelude::ServerFnError;
use store::postgres::PostgresDb;

pub(crate) async fn with_request_store<T>(
    future: impl std::future::Future<Output = Result<T, ServerFnError>>,
) -> Result<T, ServerFnError> {
    match dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<PostgresDb>, _>().await
    {
        Ok(axum::Extension(pg)) => PostgresDb::with_application_db(pg, future)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?,
        Err(_) => future.await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn request_pool_crosses_spawn_and_damaged_history_refuses_the_callback() {
        store::test_support::with_postgres(async {
            let pg = PostgresDb::application().await.unwrap();
            let expected: String = sqlx::query_scalar("SELECT current_database()")
                .fetch_one(pg.pool())
                .await
                .unwrap();
            let (mut parts, ()) = axum::http::Request::new(()).into_parts();
            parts.extensions.insert(pg.clone());
            let context = dioxus_fullstack_core::FullstackContext::new(parts);
            let request = context.clone();
            let actual = tokio::spawn(async move {
                request
                    .scope(with_request_store(async {
                        let admitted = PostgresDb::application()
                            .await
                            .map_err(|error| ServerFnError::new(error.to_string()))?;
                        Ok(sqlx::query_scalar::<_, String>("SELECT current_database()")
                            .fetch_one(admitted.pool())
                            .await
                            .unwrap())
                    }))
                    .await
            })
            .await
            .unwrap()
            .unwrap();
            assert_eq!(actual, expected);

            sqlx::query("DELETE FROM mirror_backfill_checkpoint")
                .execute(pg.pool())
                .await
                .unwrap();
            let called = Arc::new(AtomicBool::new(false));
            let marker = called.clone();
            let result = context
                .scope(with_request_store(async move {
                    marker.store(true, Ordering::SeqCst);
                    Ok(())
                }))
                .await;
            assert!(result.is_err());
            assert!(!called.load(Ordering::SeqCst));
        })
        .await;
    }
}
