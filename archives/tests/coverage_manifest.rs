//! The committed archive coverage manifest (`k8s/postgres/archive/coverage.json`)
//! against the database this release creates.
//!
//! A deployed Archives run refuses a generation when the live database holds a
//! relation the manifest does not name. The PostgreSQL test builds a fresh
//! database the way production does — every embedded migration, then the DBOS
//! SDK's own schema — and checks the Navigator half against it, so a migration
//! that adds a table fails here instead of at the next scheduled run. On
//! failure it prints a review draft of the observed inventory.
//!
//! Without `NAVIGATOR_TEST_POSTGRES_PORT` the PostgreSQL test skips, unless
//! `NAV_REQUIRE_POSTGRES=1`.

use std::sync::Arc;

use archives::coverage::{draft, inventory, ConfiguredCoverage, CoverageManifest, Rule};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use workflows::dbos_engine;

const COMMITTED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../k8s/postgres/archive/coverage.json"
));

fn committed() -> ConfiguredCoverage {
    serde_json::from_str(COMMITTED).expect("coverage.json is a coverage manifest")
}

fn relations(manifest: &CoverageManifest) -> Vec<archives::coverage::Relation> {
    manifest
        .relations
        .iter()
        .map(|rule| match rule {
            Rule::Export(relation) | Rule::Derived { relation, .. } => relation.clone(),
        })
        .collect()
}

/// Each half is internally valid: no duplicate identity, no excluded stored
/// data, no unreviewed view, and the large-object catalogs are exported.
#[test]
fn the_committed_manifest_is_well_formed() {
    let coverage = committed();
    for (name, manifest) in [
        ("navigator", &coverage.navigator),
        ("forgejo", &coverage.forgejo),
    ] {
        manifest
            .verify(&relations(manifest))
            .unwrap_or_else(|error| panic!("{name} coverage: {error}"));
        for catalog in ["pg_largeobject", "pg_largeobject_metadata"] {
            assert!(
                manifest.relations.iter().any(|rule| matches!(
                    rule,
                    Rule::Export(relation)
                        if relation.schema == "pg_catalog" && relation.name == catalog
                )),
                "{name} coverage must export pg_catalog.{catalog}"
            );
        }
    }
    let schemas = |manifest: &CoverageManifest| {
        relations(manifest)
            .into_iter()
            .map(|relation| relation.schema)
            .collect::<std::collections::BTreeSet<_>>()
    };
    for schema in ["public", "dbos", "ag_catalog", "navigator_graph"] {
        assert!(
            schemas(&coverage.navigator).contains(schema),
            "navigator coverage must name the {schema} schema"
        );
    }
}

/// A fresh database, or `None` when no test PostgreSQL is set.
async fn database() -> Option<(PgConnectOptions, u16, String)> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    let port: u16 = port
        .parse()
        .expect("NAVIGATOR_TEST_POSTGRES_PORT is a port");
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = sqlx::PgPool::connect_with(options.clone())
        .await
        .expect("connect to the test server");
    let name = format!("archive_coverage_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .expect("create the test database");
    admin.close().await;
    Some((options.database(&name), port, name))
}

#[tokio::test]
async fn the_navigator_manifest_names_every_relation_this_release_creates() {
    let Some((options, port, name)) = database().await else {
        return;
    };
    let db = store::postgres::PostgresDb::connect(options.clone())
        .await
        .expect("connect to the fresh database");
    db.migrate_embedded(None)
        .await
        .expect("apply every embedded migration");

    // DBOS creates its `dbos` schema when it launches, as `web` does.
    let settings = dbos_engine::Settings::from_lookup(|key| {
        Some(match key {
            dbos_engine::HOST_ENV => "127.0.0.1".to_owned(),
            dbos_engine::PORT_ENV => port.to_string(),
            dbos_engine::USER_ENV => "postgres".to_owned(),
            dbos_engine::PASSWORD_ENV => "synthetic".to_owned(),
            dbos_engine::DATABASE_ENV => name.clone(),
            dbos_engine::SSLMODE_ENV => "disable".to_owned(),
            dbos_engine::RELEASE_ENV => "test".to_owned(),
            dbos_engine::EXECUTOR_ID_ENV => "archive-coverage-test".to_owned(),
            _ => return None,
        })
    })
    .expect("DBOS settings");
    dbos_engine::Executor::launch(&settings, Arc::new(workflows::CapturingNotifier::new()))
        .await
        .expect("launch DBOS")
        .shutdown()
        .await;

    let pool = sqlx::PgPool::connect_with(options)
        .await
        .expect("connect to the fresh database");
    let mut tx = pool.begin().await.expect("begin");
    let observed = inventory(&mut tx).await.expect("read the inventory");
    tx.rollback().await.expect("rollback");
    if let Err(error) = committed().navigator.verify(&observed) {
        panic!(
            "k8s/postgres/archive/coverage.json no longer matches the migrated database ({error}).\n\
             Review this draft and update the `navigator` half:\n{}",
            serde_json::to_string_pretty(&draft(observed)).expect("draft serializes")
        );
    }
}
