//! Fresh native test resources use the same creation capability and migrator.
use super::{
    create_resource_on, database_name, identity, mark_created_on, CreatedDatabase, NativeDatabase,
    TierBinding,
};
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
    let prepared: anyhow::Result<NativeDatabase> = async {
        tier.validate()?;
        super::creation_phase("fixture.borrowed_admin.begin");
        let created = create_resource_on(admin, &name, || acknowledged = true).await;
        super::creation_outcome("fixture.create.end", created.is_ok());
        super::creation_outcome("fixture.creation.acknowledged", acknowledged);
        created?;
        super::creation_phase("fixture.borrowed_admin.retained");
        let expected = mark_created_on(admin, &name, operation).await?;
        super::creation_phase("fixture.data_connect.begin");
        let connected = PostgresDb::connect(fixture_options.database(&name)).await;
        super::creation_outcome("fixture.data_connect.end", connected.is_ok());
        let pg = connected?;
        initialization_pool = Some(pg.clone());
        super::creation_phase("fixture.data_identity.begin");
        let identity = require_created_identity(&pg, &expected).await;
        super::creation_outcome("fixture.data_identity.end", identity.is_ok());
        identity?;
        super::creation_phase("fixture.initialize.begin");
        let native = CreatedDatabase {
            postgres: pg,
            operation,
            tier,
            admin_options,
        }
        .initialize()
        .await?;
        super::creation_phase("fixture.initialize.end");
        if !super::admits(native.postgres()).await? {
            return Err(anyhow::Error::from(super::OriginError::Identity));
        }
        super::creation_phase("fixture.data_identity_admission.end");
        Ok(native)
    }
    .await;
    if prepared.is_err() {
        if let Some(pg) = initialization_pool {
            super::creation_phase("fixture.failed_data_pool_close.begin");
            pg.pool().close().await;
            super::creation_phase("fixture.failed_data_pool_close.end");
        }
    }
    match prepared {
        Ok(native) => Ok(native),
        Err(error) if acknowledged => {
            super::creation_phase("fixture.owned_drop.begin");
            let cleanup = sqlx::query(AssertSqlSafe(format!("DROP DATABASE \"{name}\"")))
                .execute(admin.pool())
                .await;
            super::creation_outcome("fixture.owned_drop.end", cleanup.is_ok());
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

/// Name/OID alone can collide across clusters; the owner's fresh nonce must match too.
async fn require_created_identity(
    pg: &PostgresDb,
    expected: &(String, i64, Option<String>),
) -> Result<(), super::OriginError> {
    if &identity(pg).await? != expected {
        return Err(super::OriginError::Identity);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{identity, OriginError, TierBinding};
    use crate::postgres::PostgresDb;

    #[tokio::test]
    async fn a_colliding_name_and_oid_without_the_owners_nonce_never_admits_or_changes_the_target()
    {
        crate::test_support::with_postgres(async {
            let pg = PostgresDb::application().await.unwrap();
            let before = identity(&pg).await.unwrap();
            let origin: serde_json::Value =
                sqlx::query_scalar("SELECT to_jsonb(o) FROM public.database_origin o")
                    .fetch_one(pg.pool())
                    .await
                    .unwrap();
            let expected = (
                before.0.clone(),
                before.1,
                Some(super::super::marker(uuid::Uuid::new_v4())),
            );
            assert!(
                matches!(
                    super::require_created_identity(&pg, &expected).await,
                    Err(OriginError::Identity)
                ),
                "same name/OID is not ownership of a foreign nonce"
            );
            assert_eq!(
                identity(&pg).await.unwrap(),
                before,
                "the data target is never restamped"
            );
            let retained: serde_json::Value =
                sqlx::query_scalar("SELECT to_jsonb(o) FROM public.database_origin o")
                    .fetch_one(pg.pool())
                    .await
                    .unwrap();
            assert_eq!(retained, origin);
            super::require_created_identity(&pg, &before).await.unwrap();
            assert!(super::super::admits(&pg).await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn borrowed_single_connection_admin_stays_live_and_data_options_initialize_the_owned_fixture(
    ) {
        use futures::FutureExt as _;
        crate::test_support::with_postgres(async {
            let parent = PostgresDb::application().await.unwrap();
            let admin_options = parent.pool().connect_options().as_ref().clone()
                .application_name("synthetic-borrowed-fixture-admin");
            let data_options = admin_options.clone().application_name("synthetic-borrowed-fixture-data");
            let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1)
                .connect_with(admin_options.clone()).await.unwrap();
            let admin = PostgresDb(pool);
            let backend: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(admin.pool()).await.unwrap();
            let operation = uuid::Uuid::now_v7();
            let name = super::database_name(operation).unwrap();
            let native = super::create(
                &admin, admin_options, data_options, operation,
                TierBinding { tier_id: uuid::Uuid::new_v4(), namespace_uid: uuid::Uuid::new_v4(), cluster_name: "synthetic-borrowed-fixture".into() },
            ).await.unwrap();
            let result = std::panic::AssertUnwindSafe(async {
                assert!(!admin.pool().is_closed());
                let retained: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(admin.pool()).await.unwrap();
                assert_eq!(retained, backend, "caller-owned administrative session remains retained");
                let route: String = sqlx::query_scalar("SELECT current_setting('application_name')")
                    .fetch_one(native.postgres().pool()).await.unwrap();
                assert_eq!(route, "synthetic-borrowed-fixture-data");
                let expected: (String, i64, Option<String>) = sqlx::query_as("SELECT datname::text,oid::bigint,shobj_description(oid,'pg_database') FROM pg_database WHERE datname=$1")
                    .bind(&name).fetch_one(admin.pool()).await.unwrap();
                assert_eq!(identity(native.postgres()).await.unwrap(), expected);
                assert_eq!(expected.2, Some(super::super::marker(operation)));
                assert!(super::super::admits(native.postgres()).await.unwrap());
            }).catch_unwind().await;
            native.postgres().pool().close().await;
            sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE \"{name}\"")))
                .execute(admin.pool()).await.unwrap();
            admin.ping().await.unwrap();
            admin.pool().close().await;
            if let Err(panic) = result { std::panic::resume_unwind(panic); }
        }).await;
    }

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
