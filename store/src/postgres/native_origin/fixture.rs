//! Fresh native test resources use the same creation capability and migrator.
use super::{
    create_database_on, database_name, identity, marker, stamp_creation, CreatedDatabase,
    NativeDatabase, OriginError, TierBinding,
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
    tier.validate()?;
    let name = database_name(operation)?;
    // Keep the session borrowed across the administrative steps, including cleanup.
    let mut connection = admin.pool().acquire().await?;
    let mut acknowledged = false;
    let mut data_pool = None;
    let prepared = async {
        create_database_on(&mut connection, &name, || acknowledged = true).await?;
        let oid: i64 =
            sqlx::query_scalar("SELECT oid::bigint FROM pg_catalog.pg_database WHERE datname = $1")
                .bind(&name)
                .fetch_one(&mut *connection)
                .await?;
        stamp_creation(&mut *connection, &name, operation).await?;
        let pg = PostgresDb::connect(fixture_options.database(&name)).await?;
        data_pool = Some(pg.clone());
        // Validate the separate transport before letting it modify the schema.
        if identity(&pg).await? != (name.clone(), oid, Some(marker(operation))) {
            return Err(anyhow::Error::from(OriginError::Identity));
        }
        let native = CreatedDatabase {
            postgres: pg,
            operation,
            tier,
            admin_options,
        }
        .initialize()
        .await?;
        if !super::admits(native.postgres()).await? {
            return Err(anyhow::Error::from(OriginError::Identity));
        }
        Ok(native)
    }
    .await;
    if prepared.is_err() {
        if let Some(pg) = data_pool {
            pg.pool().close().await;
        }
    }
    match prepared {
        Ok(native) => Ok(native),
        Err(error) if acknowledged => {
            let cleanup = sqlx::query(AssertSqlSafe(format!("DROP DATABASE \"{name}\"")))
                .execute(&mut *connection)
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
    async fn a_single_admin_session_can_create_and_clean_up_through_separate_routes() {
        use crate::test_support::PostgresFixture;
        use tokio::net::{TcpListener, TcpStream};

        crate::test_support::with_postgres(async {
            let direct = PostgresDb::application().await.unwrap();
            let options = direct.pool().connect_options().as_ref().clone();
            let upstream = (options.get_host().to_owned(), options.get_port());
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let forward = tokio::spawn(async move {
                let (mut inbound, _) = listener.accept().await.unwrap();
                // A second administrative session cannot use this transport.
                drop(listener);
                let mut database = TcpStream::connect(upstream).await.unwrap();
                tokio::io::copy_bidirectional(&mut inbound, &mut database)
                    .await
                    .unwrap();
            });

            let fixture = PostgresFixture::create_with_connections(
                options.clone().host("127.0.0.1").port(port),
                options,
            )
            .await
            .unwrap();
            assert_eq!(
                identity(fixture.postgres()).await.unwrap().0,
                fixture.database()
            );
            fixture
                .postgres()
                .require_application_history()
                .await
                .unwrap();
            let database = fixture.database().to_owned();
            fixture.close().await.unwrap();
            forward.await.unwrap();
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = $1)",
            )
            .bind(database)
            .fetch_one(direct.pool())
            .await
            .unwrap();
            assert!(!exists);
        })
        .await;
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
