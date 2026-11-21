//! Fresh native test resources use the same creation capability and migrator.
use super::{create_observed, database_name, identity, NativeDatabase, TierBinding};
use crate::postgres::PostgresDb;
use sqlx::postgres::PgConnectOptions;
use sqlx::AssertSqlSafe;
use uuid::Uuid;

/// Create and initialize only this operation's exclusively new fixture.
/// Production creation retains failed resources; this test-only owner cleans
/// up only after CREATE was positively acknowledged by the shared implementation.
pub(crate) async fn create(
    admin: &PostgresDb,
    admin_options: PgConnectOptions,
    fixture_options: PgConnectOptions,
    operation: Uuid,
    tier: TierBinding,
) -> anyhow::Result<NativeDatabase> {
    let name = database_name(operation)?;
    let mut acknowledged = false;
    let mut initialization_pool = None;
    let created = create_observed(
        admin_options,
        operation,
        tier.clone(),
        || acknowledged = true,
        |pg| initialization_pool = Some(pg.clone()),
    )
    .await;
    let mut routed_pool = None;
    let prepared = async {
        let native = created?.initialize().await?;
        let expected = identity(native.postgres()).await?;
        let pg = PostgresDb::connect(fixture_options.database(&name)).await?;
        routed_pool = Some(pg.clone());
        // A separately routed transport must reach the exact resource this
        // capability created, not merely a database with a matching name.
        let actual = identity(&pg).await?;
        if actual != expected || !super::admits(&pg).await? {
            return Err(anyhow::Error::from(super::OriginError::Identity));
        }
        Ok(NativeDatabase {
            postgres: pg,
            operation,
            tier,
        })
    }
    .await;
    // The opaque creation resource may have failed after connecting. Its
    // observed pool is retained solely to drain it before owned cleanup.
    if let Some(pg) = initialization_pool {
        pg.pool().close().await;
    }
    if prepared.is_err() {
        if let Some(pg) = routed_pool {
            pg.pool().close().await;
        }
    }
    match prepared {
        Ok(native) => Ok(native),
        Err(error) if acknowledged => {
            let cleanup = sqlx::query(AssertSqlSafe(format!("DROP DATABASE \"{name}\"")))
                .execute(admin.pool())
                .await;
            match cleanup {
                Ok(_) => Err(error),
                Err(cleanup) => {
                    Err(error.context(format!("owned fixture cleanup also failed: {cleanup}")))
                }
            }
        }
        // An error before acknowledgement is not proof of ownership: a
        // pre-existing database or ambiguous CREATE response is never dropped.
        Err(error) => {
            Err(error.context("fixture creation was not acknowledged; no database adopted"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{identity, OriginError, TierBinding};
    use crate::postgres::PostgresDb;

    #[tokio::test]
    async fn duplicate_existing_native_fixture_never_adopts_or_drops_the_database() {
        crate::test_support::with_postgres(async {
            let pg = PostgresDb::application().await.unwrap();
            let options = pg.pool().connect_options().as_ref().clone();
            let before = identity(&pg).await.unwrap();
            let (operation, tier_id, namespace_uid, cluster_name):
                (uuid::Uuid, uuid::Uuid, uuid::Uuid, String) = sqlx::query_as(
                    "SELECT creation_operation,tier_id,namespace_uid,cluster_name FROM database_origin WHERE singleton",
                ).fetch_one(pg.pool()).await.unwrap();
            let origin: serde_json::Value = sqlx::query_scalar(
                "SELECT to_jsonb(database_origin) FROM database_origin WHERE singleton",
            ).fetch_one(pg.pool()).await.unwrap();
            let (sentinel_id, sentinel): (uuid::Uuid, serde_json::Value) = sqlx::query_as(
                "INSERT INTO person(id,name,email,role) VALUES($1,'Synthetic fixture sentinel','fixture-collision@example.test','client') RETURNING id,to_jsonb(person)",
            ).bind(uuid::Uuid::now_v7()).fetch_one(pg.pool()).await.unwrap();

            let Err(error) = super::create(
                &pg, options.clone(), options, operation,
                TierBinding { tier_id, namespace_uid, cluster_name },
            ).await else {
                panic!("duplicate creation must never adopt the existing native fixture");
            };
            assert!(matches!(error.downcast_ref::<OriginError>(), Some(OriginError::Creation)));
            assert_eq!(identity(&pg).await.unwrap(), before, "the actual database OID and creation marker stay unchanged");
            let retained: serde_json::Value = sqlx::query_scalar(
                "SELECT to_jsonb(database_origin) FROM database_origin WHERE singleton",
            ).fetch_one(pg.pool()).await.unwrap();
            assert_eq!(retained, origin, "duplicate creation must not rewrite admission evidence");
            let retained_person: serde_json::Value = sqlx::query_scalar(
                "SELECT to_jsonb(person) FROM person WHERE id=$1",
            ).bind(sentinel_id).fetch_one(pg.pool()).await.unwrap();
            assert_eq!(retained_person, sentinel, "duplicate creation preserves the exact owned sentinel row");
            assert!(!pg.pool().is_closed(), "the caller's working pool must remain open");
            pg.ping().await.unwrap();
            PostgresDb::application().await.expect("the existing native origin remains admitted");
        }).await;
    }
}
