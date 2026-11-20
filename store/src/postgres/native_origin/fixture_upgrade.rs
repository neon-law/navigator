//! Fixed future-release transport fixture; no caller-selected migration or SQL.
use super::*;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::SqlSafeStr;
use std::time::Duration;

const VERSION: i64 = 20_261_120_000_000;
const MIGRATION_GATE: i64 = 617_250_021;

fn runner() -> Migrator {
    let mut entries = embedded_migrator().iter().cloned().collect::<Vec<_>>();
    entries.push(Migration::new(
        VERSION,
        "fixed origin transport fixture".into(),
        MigrationType::Simple,
        AssertSqlSafe("SELECT pg_advisory_xact_lock(617250021); SELECT 1;").into_sql_str(),
        false,
    ));
    Migrator::with_migrations(entries)
}

async fn observed(pg: &PostgresDb, sql: &'static str) -> Result<(), OriginError> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let ready: bool = sqlx::query_scalar(AssertSqlSafe(sql))
                .fetch_one(pg.pool())
                .await?;
            if ready {
                return Ok::<(), OriginError>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| OriginError::Upgrade)?
}

pub(crate) async fn pending(native: &NativeDatabase) -> Result<Uuid, OriginError> {
    if !admits(native.postgres()).await? {
        return Err(OriginError::Authority);
    }
    let operation = Uuid::new_v4();
    let creation = native.operation();
    let pg = native.postgres().clone();
    let tier = native.tier().clone();
    let mut gate = pg.pool().acquire().await?;
    gate.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_GATE)
        .execute(&mut *gate)
        .await?;
    let worker_pg = pg.clone();
    let mut task = tokio::spawn(async move {
        upgrade::upgrade_with(&worker_pg, creation, &tier, operation, &runner()).await
    });
    let result=async {
        observed(&pg,"SELECT EXISTS(SELECT 1 FROM database_origin_upgrade WHERE completed_at IS NULL)").await?;
        let mut receipt=pg.pool().begin().await?;
        sqlx::query("SELECT upgrade_operation FROM database_origin_upgrade WHERE upgrade_operation=$1 FOR UPDATE")
            .bind(operation).fetch_one(&mut *receipt).await?;
        sqlx::query("SELECT pg_advisory_unlock($1)").bind(MIGRATION_GATE).execute(&mut *gate).await?;
        observed(&pg,"SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version=20261120000000 AND success)").await?;
        let completed:bool=sqlx::query_scalar("SELECT completed_at IS NOT NULL FROM database_origin_upgrade WHERE upgrade_operation=$1")
            .bind(operation).fetch_one(pg.pool()).await?;
        if completed {return Err(OriginError::Upgrade);}
        task.abort();
        if !matches!((&mut task).await,Err(error) if error.is_cancelled()) {return Err(OriginError::Upgrade);}
        receipt.rollback().await?;
        observed(&pg,"SELECT NOT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND classid=1869769063 AND objid=1768846704)").await?;
        Ok(operation)
    }.await;
    // Every early error aborts the owned runner and closes the owned gate;
    // no shared role, database or runtime is stopped or modified.
    task.abort();
    if !task.is_finished() {
        let _ = task.await;
    }
    gate.close().await?;
    result
}

pub(crate) async fn complete(native: &NativeDatabase, operation: Uuid) -> Result<(), OriginError> {
    upgrade::upgrade_with(
        native.postgres(),
        native.operation(),
        native.tier(),
        operation,
        &runner(),
    )
    .await
}
