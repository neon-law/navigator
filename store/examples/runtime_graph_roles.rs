//! Exercise genuine native fixtures with the actual runtime role initializer.
use anyhow::{bail, Context, Result};
use sqlx::postgres::PgConnectOptions;
use std::path::PathBuf;
use std::process::Command;
use store::{postgres::PostgresDb, test_support::PostgresFixture};

#[path = "../tests/support/person_merge_graph.rs"]
mod merge_fixture;
#[path = "../tests/support/native_project.rs"]
mod native_project;

fn psql(database: &str, app: bool, file: &str) -> Result<()> {
    let container = std::env::var("GRAPH_ROLE_CONTAINER")?;
    let password = if app { "app" } else { "admin" };
    let user = if app { "navigator_app" } else { "postgres" };
    let status = Command::new("docker")
        .args(["exec", "--user", "0", "-e"])
        .arg(format!(
            "PGPASSFILE=/etc/navigator/postgres/probe/{password}.pgpass"
        ))
        .arg(container)
        .args([
            "psql",
            "-X",
            "--no-password",
            "-h",
            "127.0.0.1",
            "-U",
            user,
            "-d",
            database,
            "-v",
            "ON_ERROR_STOP=1",
            "-v",
        ])
        .arg(format!("navigator_database={database}"))
        .args(["-f", file])
        .status()
        .context("execute actual role SQL")?;
    if !status.success() {
        bail!("role SQL failed: {file}");
    }
    Ok(())
}

fn secret(name: &str) -> Result<String> {
    let root =
        PathBuf::from(std::env::var_os("GRAPH_ROLE_SECRET_DIR").context("secret directory")?);
    Ok(std::fs::read_to_string(root.join(name).join("password"))?
        .trim()
        .to_owned())
}

async fn admitted(database: &str, options: PgConnectOptions) -> Result<()> {
    let app = PostgresDb::connect(
        options
            .database(database)
            .username("navigator_app")
            .password(&secret("app")?),
    )
    .await?;
    let result = PostgresDb::with_application_db(app.clone(), async {
        let scoped = PostgresDb::application().await?;
        let current: String = sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(scoped.pool())
            .await?;
        anyhow::ensure!(current == "navigator_app", "admission used wrong login");
        Ok::<(), anyhow::Error>(())
    })
    .await;
    app.pool().close().await;
    result??;
    Ok(())
}

async fn admitted_merge(database: &str, options: PgConnectOptions) -> Result<()> {
    let app = PostgresDb::connect(
        options
            .database(database)
            .username("navigator_app")
            .password(&secret("app")?),
    )
    .await?;
    let worker = app.clone();
    // Catch assertion panics as JoinError so the caller still closes its owned fixture.
    let joined = tokio::spawn(async move {
        PostgresDb::with_application_db(worker, async {
            let pg = PostgresDb::application().await?;
            let current: String = sqlx::query_scalar("SELECT current_user::text")
                .fetch_one(pg.pool())
                .await?;
            anyhow::ensure!(current == "navigator_app", "merge used wrong login");
            let db = store::surreal::SurrealDb::uninitialized();
            let parent =
                native_project::create(&format!("synthetic-app-merge-{}", uuid::Uuid::new_v4()))
                    .await;
            let scenario = merge_fixture::Scenario::seed(&db, &pg, &parent).await;
            let before = merge_fixture::snapshot(&pg).await;
            let refused = sqlx::query("SELECT public.graph_merge_person($1,$2)")
                .bind(scenario.kept)
                .bind(scenario.dropped)
                .execute(pg.pool())
                .await
                .expect_err("projector must refuse an incomplete relational merge");
            anyhow::ensure!(
                refused
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code)
                    .as_deref()
                    == Some("23514"),
                "wrong incomplete-merge refusal"
            );
            anyhow::ensure!(
                merge_fixture::snapshot(&pg).await == before,
                "incomplete projection changed the 21-table sample"
            );
            store::person_merge::merge(&db, scenario.kept, scenario.dropped).await?;
            scenario.assert_merged(&db, &pg).await;
            Ok::<(), anyhow::Error>(())
        })
        .await
    })
    .await;
    app.pool().close().await;
    let admitted = joined.context("owned App merge assertion task failed")?;
    admitted??;
    println!("actual App public merge, canonical references/participation/provenance/AGE PASS");
    Ok(())
}

// Bootstrap introspection only: this database has not run its role initializer yet.
async fn migration_grants(fixture: &PostgresFixture) -> Result<()> {
    let public: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS(SELECT FROM unnest(ARRAY[
         'public.graph_ensure_vertices(text,text)', 'public.graph_load_edges(text,text)',
         'public.graph_merge_person(uuid,uuid)',
         'public.graph_view_seed(text)', 'public.graph_project_attach(text)',
         'public.graph_project_expand(text)', 'public.graph_lawyer_expand(text)',
         'public.graph_firm_view(text)', 'public.graph_conflict_frontier(text)',
         'public.graph_conflict_edges(text)']) AS signatures(signature)
         WHERE NOT has_function_privilege('navigator_app',signature,'EXECUTE'))",
    )
    .fetch_one(fixture.postgres().pool())
    .await?;
    anyhow::ensure!(
        public,
        "existing App lacks migration-time wrapper/reader grants"
    );
    let private: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS(SELECT FROM unnest(ARRAY[
         'public.graph_load_vertices(text,text)', 'public.graph_load_edges_raw(text,text)',
         'public.graph_ensure_vertices_raw(text,text)', 'public.graph_clear_navigator()',
         'public.graph_canonical_vertex(text,jsonb)', 'public.graph_canonical_edge(text,jsonb)',
         'public.graph_require_unfiltered_sources()', 'public.graph_insert_canonical_edge(text,jsonb)'
         ]) AS signatures(signature)
         WHERE has_function_privilege('navigator_app',signature,'EXECUTE'))",
    )
    .fetch_one(fixture.postgres().pool())
    .await?;
    anyhow::ensure!(private, "migration exposed a private graph helper to App");
    println!("pre-initializer migration ACLs PASS (bootstrap introspection only)");
    Ok(())
}

async fn controls(fixture: &PostgresFixture, options: PgConnectOptions) -> Result<()> {
    let database = fixture.database();
    psql(database, false, "/probe/k8s/postgres/access/roles.sql")?;
    admitted(database, options.clone()).await?;
    admitted_merge(database, options.clone()).await?;
    psql(
        database,
        true,
        "/probe/images/postgres/tests/runtime-graph-app-controls.sql",
    )?;
    sqlx::query("ALTER TABLE public.relationship ENABLE ROW LEVEL SECURITY")
        .execute(fixture.postgres().pool())
        .await?;
    let rls = psql(
        database,
        true,
        "/probe/images/postgres/tests/runtime-graph-app-rls-refusal.sql",
    );
    let restore = sqlx::query("ALTER TABLE public.relationship DISABLE ROW LEVEL SECURITY")
        .execute(fixture.postgres().pool())
        .await;
    restore?;
    rls?;
    psql(database, false, "/probe/k8s/postgres/access/roles.sql")?;
    admitted(database, options.clone()).await?;
    admitted_merge(database, options).await?;
    psql(
        database,
        true,
        "/probe/images/postgres/tests/runtime-graph-app-controls.sql",
    )?;
    Ok(())
}

async fn run() -> Result<()> {
    anyhow::ensure!(
        std::env::var("NAVIGATOR_FORBID_SURREAL").as_deref() == Ok("1"),
        "Source refusal missing"
    );
    let port = std::env::var("GRAPH_ROLE_PORT")?.parse()?;
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password(&secret("bootstrap")?)
        .database("postgres");
    let admin = PostgresDb::connect(options.clone()).await?;
    let absent: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS(SELECT 1 FROM pg_roles WHERE rolname='navigator_app')",
    )
    .fetch_one(admin.pool())
    .await?;
    admin.pool().close().await;
    anyhow::ensure!(absent, "cluster must be exclusively new and role-less");
    for order in ["migrations-before-roles", "roles-before-migrations"] {
        let fixture = PostgresFixture::create(options.clone()).await?;
        let result = async {
            if order == "roles-before-migrations" {
                migration_grants(&fixture).await?;
            }
            controls(&fixture, options.clone()).await
        }
        .await
        .context(order);
        let cleanup = fixture.close().await;
        cleanup.context("close owned native fixture")?;
        result?;
        println!("runtime graph roles PASS: {order}");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    run().await
}
