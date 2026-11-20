//! One bound migration intent, with explicit same-operation interruption recovery.
use super::{
    authority_contract, database_name, origin, require_schema_for, schema_attestation,
    schema_contract_for, NativeDatabase, Origin, OriginError, TierBinding,
};
use crate::postgres::{embedded_migrator, PostgresDb};
use sqlx::migrate::Migrator;
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, Row};
use std::collections::BTreeMap;
use uuid::Uuid;

const LOCK: i64 = 0x6f72_6967_696e_7570; // "originup", scoped to this database.

pub(super) struct Intent {
    operation: Uuid,
    predecessor: Uuid,
    from: serde_json::Value,
    to: serde_json::Value,
    completed: bool,
}

pub(super) struct Chain {
    pub schema: serde_json::Value,
    predecessor: Uuid,
    pub pending: Option<Intent>,
}

fn prefix(older: &serde_json::Value, newer: &serde_json::Value) -> bool {
    match (older.as_array(), newer.as_array()) {
        (Some(old), Some(new)) => !old.is_empty() && new.starts_with(old),
        _ => false,
    }
}

pub(super) async fn chain(
    pg: &PostgresDb,
    origin: &Origin,
    target: &serde_json::Value,
) -> Result<Chain, OriginError> {
    if !prefix(&origin.schema, target) {
        return Err(OriginError::Authority);
    }
    let rows=sqlx::query("SELECT upgrade_operation,creation_operation,predecessor_operation,from_schema_contract,to_schema_contract,authority_contract,completed_at IS NOT NULL AS completed FROM public.database_origin_upgrade LIMIT 1025")
        .fetch_all(pg.pool()).await?;
    if rows.len() > 1024 {
        return Err(OriginError::Upgrade);
    }
    let mut steps = BTreeMap::new();
    for row in rows {
        if row.try_get::<Uuid, _>("creation_operation")? != origin.operation
            || row.try_get::<serde_json::Value, _>("authority_contract")? != authority_contract()
        {
            return Err(OriginError::Upgrade);
        }
        let intent = Intent {
            operation: row.try_get("upgrade_operation")?,
            predecessor: row.try_get("predecessor_operation")?,
            from: row.try_get("from_schema_contract")?,
            to: row.try_get("to_schema_contract")?,
            completed: row.try_get("completed")?,
        };
        if steps.insert(intent.predecessor, intent).is_some() {
            return Err(OriginError::Upgrade);
        }
    }
    let mut schema = origin.schema.clone();
    let mut predecessor = origin.operation;
    while let Some(intent) = steps.remove(&predecessor) {
        if intent.operation.is_nil()
            || intent.predecessor.is_nil()
            || intent.operation == origin.operation
            || intent.operation == predecessor
            || intent.from != schema
            || intent.to == intent.from
            || !prefix(&intent.from, &intent.to)
            || !prefix(&intent.to, target)
        {
            return Err(OriginError::Upgrade);
        }
        if !intent.completed {
            if !steps.is_empty() {
                return Err(OriginError::Upgrade);
            }
            return Ok(Chain {
                schema,
                predecessor,
                pending: Some(intent),
            });
        }
        schema = intent.to;
        predecessor = intent.operation;
    }
    if !steps.is_empty() {
        return Err(OriginError::Upgrade);
    }
    Ok(Chain {
        schema,
        predecessor,
        pending: None,
    })
}

/// Upgrade only this verified fresh-native resource. The intent is durable before
/// SQLx begins; a crash after DDL resumes only with the same bound operation.
/// Unknown advanced schemas, changed authority families and foreign resources refuse.
/// The owning provisioner must stop its application writers before invoking DDL.
///
/// # Errors
/// Invalid origin/tier, conflicting intent, checksum drift, failed migration or storage failure.
pub async fn upgrade(
    admin_options: PgConnectOptions,
    creation: Uuid,
    tier: TierBinding,
    operation: Uuid,
) -> Result<NativeDatabase, OriginError> {
    tier.validate()?;
    let pg = PostgresDb::connect(admin_options.database(&database_name(creation)?))
        .await
        .map_err(|_| OriginError::Storage)?;
    let result = upgrade_with(&pg, creation, &tier, operation, &embedded_migrator()).await;
    if let Err(error) = result {
        pg.pool().close().await;
        return Err(error);
    }
    Ok(NativeDatabase {
        postgres: pg,
        operation: creation,
        tier,
    })
}

pub(super) async fn upgrade_with(
    pg: &PostgresDb,
    creation: Uuid,
    tier: &TierBinding,
    operation: Uuid,
    runner: &Migrator,
) -> Result<(), OriginError> {
    if operation.is_nil() || operation == creation {
        return Err(OriginError::Identity);
    }
    let mut lease = pg.pool().acquire().await?;
    lease.close_on_drop();
    let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(LOCK)
        .fetch_one(&mut *lease)
        .await?;
    if !held {
        return Err(OriginError::Busy);
    }
    let origin = origin(pg).await?.ok_or(OriginError::Authority)?;
    if origin.operation != creation || &origin.tier != tier {
        return Err(OriginError::Identity);
    }
    let target = schema_contract_for(runner);
    let history = chain(pg, &origin, &target).await?;
    let intent = if let Some(intent) = history.pending {
        if intent.operation != operation || intent.to != target {
            return Err(OriginError::Upgrade);
        }
        let actual = schema_attestation(pg).await?;
        if !prefix(&intent.from, &actual) || !prefix(&actual, &intent.to) {
            return Err(OriginError::Authority);
        }
        intent
    } else {
        require_schema_for(pg, &history.schema).await?;
        if history.schema == target {
            return Ok(());
        }
        let intent = Intent {
            operation,
            predecessor: history.predecessor,
            from: history.schema,
            to: target.clone(),
            completed: false,
        };
        sqlx::query("INSERT INTO public.database_origin_upgrade(upgrade_operation,creation_operation,predecessor_operation,from_schema_contract,to_schema_contract,authority_contract) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(operation).bind(creation).bind(intent.predecessor).bind(&intent.from).bind(&intent.to).bind(authority_contract())
            .execute(pg.pool()).await?;
        intent
    };
    pg.migrate(runner, None)
        .await
        .map_err(|_| OriginError::Migration)?;
    require_schema_for(pg, &target).await?;
    let current = origin_record(pg, creation, tier).await?;
    let history = chain(pg, &current, &target).await?;
    if history.pending.as_ref().map(|pending| pending.operation) != Some(intent.operation) {
        return Err(OriginError::Upgrade);
    }
    // A cancellation while this update is blocked must not leave an
    // autocommit query that can later publish completion without the caller.
    let mut completion = lease.begin().await?;
    let finished=sqlx::query("UPDATE public.database_origin_upgrade SET completed_at=now() WHERE upgrade_operation=$1 AND completed_at IS NULL")
        .bind(operation).execute(&mut *completion).await?;
    if finished.rows_affected() != 1 {
        return Err(OriginError::Upgrade);
    }
    completion.commit().await?;
    Ok(())
}

async fn origin_record(
    pg: &PostgresDb,
    creation: Uuid,
    tier: &TierBinding,
) -> Result<Origin, OriginError> {
    let current = origin(pg).await?.ok_or(OriginError::Authority)?;
    if current.operation != creation || &current.tier != tier {
        return Err(OriginError::Identity);
    }
    Ok(current)
}
