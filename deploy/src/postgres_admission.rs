//! Read-only release admission from the PostgreSQL application's completed history.

use std::process::ExitCode;

use store::postgres::{PostgresDb, PostgresError};

async fn verify() -> Result<Vec<&'static str>, PostgresError> {
    PostgresDb::application().await?;
    Ok(store::postgres_backfill::family::Family::all()
        .iter()
        .map(|family| family.scope())
        .collect())
}

pub fn run(json: bool) -> ExitCode {
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "could not start PostgreSQL admission runtime".to_owned())
        .and_then(|runtime| {
            runtime
                .block_on(verify())
                .map_err(|error| error.to_string())
        });
    match result {
        Ok(scopes) => {
            if json {
                println!("{}", serde_json::json!({"ready":true,"scopes":scopes}));
            } else {
                println!("PostgreSQL application history is ready");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("hudson: PostgreSQL application history is unavailable: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn admission_requires_every_current_scope_without_source_access() {
        store::test_support::with_captured_postgres(async {
            assert!(verify().await.is_ok());
            let pg = PostgresDb::from_env().await.unwrap();
            assert!(!store::postgres::native_origin::admits(&pg).await.unwrap());
            sqlx::query("DELETE FROM mirror_backfill_checkpoint WHERE run_id IN (SELECT id FROM mirror_backfill_run WHERE scope='application:1')")
                .execute(pg.pool()).await.unwrap();
            assert!(matches!(verify().await, Err(PostgresError::History)));
        }).await;
    }
}
