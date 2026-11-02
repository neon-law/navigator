//! A pending Notation Transition owns its Notation until the body completes.
use crate::postgres::PostgresDb;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("Notation has an unfinished operation")]
    Busy,
    #[error("operation identity belongs to different input or outcome")]
    Conflict,
    #[error("operation storage: {0}")]
    Database(#[from] sqlx::Error),
}
#[derive(Debug, PartialEq)]
pub enum Admission {
    Pending { started: bool },
    Completed(Value),
}

/// Admit the same immutable operation or refuse another unfinished owner.
/// # Errors
/// Refuses identity reuse, pending owners, and database failures.
pub async fn admit(
    pg: &PostgresDb,
    id: Uuid,
    notation: Uuid,
    fingerprint: &str,
) -> Result<Admission, OperationError> {
    let mut tx = pg.pool().begin().await?;
    let inserted=sqlx::query("INSERT INTO notation_transition(id,notation_id,request_fingerprint) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING")
        .bind(id).bind(notation).bind(fingerprint).execute(&mut *tx).await;
    if let Err(error) = inserted {
        let busy = error.as_database_error().is_some_and(|error| {
            error.code().as_deref() == Some("23505")
                && error.constraint() == Some("notation_transition_pending")
        });
        tx.rollback().await?;
        return Err(if busy {
            OperationError::Busy
        } else {
            OperationError::Database(error)
        });
    }
    let row=sqlx::query("SELECT notation_id,request_fingerprint,started,result FROM notation_transition WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_one(&mut *tx).await?;
    if row.try_get::<Uuid, _>("notation_id")? != notation
        || row.try_get::<String, _>("request_fingerprint")? != fingerprint
    {
        return Err(OperationError::Conflict);
    }
    let result = row.try_get::<Option<Value>, _>("result")?;
    let admission = match result {
        Some(value) => Admission::Completed(value),
        None => Admission::Pending {
            started: row.try_get("started")?,
        },
    };
    tx.commit().await?;
    Ok(admission)
}

/// Mark before the first checkpointed state read or effect; failures keep ownership.
/// # Errors
/// Refuses completed/missing operations and database failures.
pub async fn mark_started(pg: &PostgresDb, id: Uuid) -> Result<(), OperationError> {
    let changed =
        sqlx::query("UPDATE notation_transition SET started=true WHERE id=$1 AND result IS NULL")
            .bind(id)
            .execute(pg.pool())
            .await?
            .rows_affected();
    if changed != 1 {
        return Err(OperationError::Conflict);
    }
    Ok(())
}

/// Release only fresh preflight failures, before any checkpointed state or effect.
/// # Errors
/// Database failures; a started operation is deliberately retained.
pub async fn discard_unstarted(pg: &PostgresDb, id: Uuid) -> Result<(), OperationError> {
    sqlx::query("DELETE FROM notation_transition WHERE id=$1 AND NOT started AND result IS NULL")
        .bind(id)
        .execute(pg.pool())
        .await?;
    Ok(())
}

/// Completion receipt and release of pending uniqueness commit in one statement.
/// # Errors
/// Refuses missing operations, changed outcomes, and database failures.
pub async fn complete(pg: &PostgresDb, id: Uuid, result: &Value) -> Result<(), OperationError> {
    let mut tx = pg.pool().begin().await?;
    let old: Option<Value> =
        sqlx::query_scalar("SELECT result FROM notation_transition WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if let Some(old) = old {
        if old != *result {
            return Err(OperationError::Conflict);
        }
    } else {
        sqlx::query("UPDATE notation_transition SET result=$2,completed_at=now() WHERE id=$1")
            .bind(id)
            .bind(result)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
