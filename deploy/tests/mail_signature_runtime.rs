//! Real-binary tests for the draft command's runtime signature source
//! (LAW-280): a draft composed while the deployment's stores are reachable
//! wears the `mail_signature` row the sending Gmail account's Person holds,
//! and stderr prints the provenance line naming the source, Person, and
//! revision. Offline, the `NAVIGATOR_GMAIL_SIGNATURE` env var signs the draft
//! and the fallback is announced, never silent. With neither source set, the
//! draft refuses missing configuration before any Gmail contact.
//!
//! The store-backed cases run the real `navigator` binary as a subprocess
//! against the same dependency tier the store's own fixtures use: the
//! server-mode `SurrealDB` lane (`store::test_support::server_surreal`,
//! skipped on a laptop without `NAVIGATOR_SURREAL_*`) and a disposable
//! PostgreSQL database holding the canonical schema
//! (`NAVIGATOR_TEST_POSTGRES_PORT`, skipped without it, failed when
//! `NAV_REQUIRE_POSTGRES=1` arms the tier). The offline cases run everywhere,
//! because their premise is that no store answers.
//!
//! No case touches Gmail: the binary carries no stored refresh token, so the
//! draft always stops at the connect stage — after signature resolution,
//! before the drafts endpoints. The insertion of whatever resolution
//! returned is pinned by `deploy/src/mail.rs`'s own tests; these prove which
//! block resolution hands that seam.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use store::postgres_schema::canonical_schema;
use store::surreal::DATABASE_ENV;
use store::test_support::server_surreal;
use tempfile::TempDir;

/// The sending account the subprocess is configured with: the seeded
/// attorney's address, the one `Person.email` resolution matches against.
const ATTORNEY_EMAIL: &str = "attorney@vestaestateplanning.com";

/// A second attorney's address, whose row must never sign the first
/// attorney's draft.
const COLLEAGUE_EMAIL: &str = "colleague@vestaestateplanning.com";

const ATTORNEY_SIGNATURE: &str = "Vesta Attorney\nConfidential: intended recipient only.";
const COLLEAGUE_SIGNATURE: &str = "Colleague Block — never another attorney's.";
const ENV_SIGNATURE: &str = "Env Fallback Block\nConfidential: intended recipient only.";

/// The revision the seeded rows carry — the `updated_at` the provenance line
/// names, seeded explicitly so the assertion is deterministic against
/// `to_rfc3339`'s `+00:00` rendering.
const SEEDED_REVISION: &str = "2026-11-05T22:41:19Z";

fn runtime_provenance(person_id: uuid::Uuid) -> String {
    format!("signature: source=runtime person={person_id} revision=2026-11-05T22:41:19+00:00")
}

const ENV_PROVENANCE: &str = "signature: source=env person=n/a revision=n/a";

/// The store-backed environment one test drafts in: a server-mode `SurrealDB`
/// database holding the seeded Person rows, and a disposable PostgreSQL
/// database holding the canonical schema and the seeded signature rows.
/// `None` when the dependency tier is absent (skipped on a laptop).
struct StoreEnv {
    surreal: store::test_support::ServerSurreal,
    postgres_database: String,
    postgres_port: String,
}

impl StoreEnv {
    /// The `SurrealDB` connection environment for the subprocess: the same
    /// coordinates this test process used, re-pointed at the test's own
    /// database (`server_surreal` already made that substitution).
    fn surreal_database_env(&self) -> (&'static str, &str) {
        (DATABASE_ENV, &self.surreal.config.database)
    }
}

/// The disposable PostgreSQL tier, in the shape the letterhead runtime tests
/// use: a fresh database per test, the AGE extension, and the canonical
/// schema. Missing port env skips (or fails when the tier is armed).
async fn fresh_postgres() -> Option<(String, String)> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
            Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        eprintln!("skipping the PostgreSQL lane: NAVIGATOR_TEST_POSTGRES_PORT is unset");
        return None;
    };
    let admin_database =
        std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into());
    let admin = sqlx::PgPool::connect_with(
        sqlx::postgres::PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().expect("test postgres port"))
            .username("postgres")
            .password("synthetic")
            .ssl_mode(sqlx::postgres::PgSslMode::Disable)
            .database(&admin_database),
    )
    .await
    .expect("connect to the test PostgreSQL instance");
    let database = format!("mail_signature_runtime_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .expect("create the disposable mail-signature database");
    admin.close().await;
    let setup = sqlx::PgPool::connect_with(
        sqlx::postgres::PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().expect("test postgres port"))
            .username("postgres")
            .password("synthetic")
            .ssl_mode(sqlx::postgres::PgSslMode::Disable)
            .database(&database),
    )
    .await
    .expect("connect to the disposable mail-signature database");
    // The canonical schema's graph migration needs the AGE extension, which
    // only a superuser creates; these fixtures stand in for the production
    // superuser init path. `LOAD` puts the library in this session the way
    // `session_preload_libraries` does for the application's roles.
    sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS age; LOAD 'age';")
        .execute(&setup)
        .await
        .expect("load the AGE extension");
    sqlx::raw_sql(canonical_schema())
        .execute(&setup)
        .await
        .expect("apply the canonical schema");
    setup.close().await;
    Some((database, port))
}

/// The full store tier for one test. `None` skips the caller.
async fn store_env(database: &str) -> Option<StoreEnv> {
    let surreal = server_surreal(database).await?;
    let (postgres_database, postgres_port) = fresh_postgres().await?;
    Some(StoreEnv {
        surreal,
        postgres_database,
        postgres_port,
    })
}

/// Seed the attorney Person the way production holds one, and return the id
/// the `mail_signature` row keys on.
async fn seed_attorney(stores: &StoreEnv, email: &str, name: &str) -> uuid::Uuid {
    let person = store::persons::create(
        &stores.surreal.db,
        &store::persons::NewPerson::new(name, email),
    )
    .await
    .expect("seed the attorney Person");
    person.id
}

/// Seed the stored signature row for `person_id`, written the way the
/// portal's self-edit save leaves it, with the deterministic fixture
/// revision.
async fn seed_signature_row(pg: &sqlx::PgPool, person_id: uuid::Uuid, body: &str) {
    sqlx::query(
        "INSERT INTO mail_signature (person_id, body, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $3)",
    )
    .bind(person_id)
    .bind(body)
    .bind(
        chrono::DateTime::parse_from_rfc3339(SEEDED_REVISION)
            .expect("the fixture revision is RFC 3339")
            .with_timezone(&chrono::Utc),
    )
    .execute(pg)
    .await
    .expect("seed the mail signature row");
}

/// A PostgreSQL pool on the test's disposable database, for seeding.
async fn signature_pool(stores: &StoreEnv) -> sqlx::PgPool {
    sqlx::PgPool::connect_with(
        sqlx::postgres::PgConnectOptions::new()
            .host("127.0.0.1")
            .port(stores.postgres_port.parse().expect("test postgres port"))
            .username("postgres")
            .password("synthetic")
            .ssl_mode(sqlx::postgres::PgSslMode::Disable)
            .database(&stores.postgres_database),
    )
    .await
    .expect("connect to the disposable mail-signature database")
}

/// The store-backed environment handed to a subprocess draft: the test
/// process's own `SurrealDB` coordinates with the test's database, plus a
/// disposable PostgreSQL the binary migrates nothing against.
fn with_store_env(command: &mut Command, stores: &StoreEnv) {
    let (key, value) = stores.surreal_database_env();
    command.env(key, value);
    command.env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1");
    command.env("NAVIGATOR_POSTGRES_PORT", &stores.postgres_port);
    command.env("NAVIGATOR_POSTGRES_USER", "postgres");
    command.env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic");
    command.env("NAVIGATOR_POSTGRES_DATABASE", &stores.postgres_database);
    command.env("NAVIGATOR_POSTGRES_SSLMODE", "disable");
}

/// Every coordinate a runtime read could come from, removed — the offline
/// cases' premise.
fn without_store_env(command: &mut Command) {
    for key in [
        "NAVIGATOR_SURREAL_ENDPOINT",
        "NAVIGATOR_SURREAL_NAMESPACE",
        "NAVIGATOR_SURREAL_DATABASE",
        "NAVIGATOR_SURREAL_USER",
        "NAVIGATOR_SURREAL_PASSWORD",
        "NAVIGATOR_SURREAL_AUTH_SCOPE",
        "NAVIGATOR_SURREAL_TOKEN",
        "NAVIGATOR_POSTGRES_HOST",
        "NAVIGATOR_POSTGRES_PORT",
        "NAVIGATOR_POSTGRES_USER",
        "NAVIGATOR_POSTGRES_PASSWORD",
        "NAVIGATOR_POSTGRES_DATABASE",
        "NAVIGATOR_POSTGRES_SSLMODE",
        "NAVIGATOR_POSTGRES_ROOT_CERT",
    ] {
        command.env_remove(key);
    }
}

/// Run the real binary's draft command with `account` as the configured
/// sending Gmail address and `signature_env` as `NAVIGATOR_GMAIL_SIGNATURE`.
/// Every case stops at the Gmail-connect stage — after signature resolution.
fn draft_command(
    body_file: &Path,
    account: &str,
    signature_env: Option<&str>,
    stores: Option<&StoreEnv>,
) -> std::process::Output {
    let mut command = Command::new(cargo_bin("hudson"));
    command
        .args(["mail", "draft", "--body-file"])
        .arg(body_file)
        .args(["--to", "client@example.test", "--subject", "Review"])
        .env("NAVIGATOR_GMAIL_ACCOUNT", account)
        .env("NAVIGATOR_GMAIL_CLIENT_ID", "test-client-id");
    if let Some(signature) = signature_env {
        command.env("NAVIGATOR_GMAIL_SIGNATURE", signature);
    } else {
        command.env_remove("NAVIGATOR_GMAIL_SIGNATURE");
    }
    match stores {
        Some(env) => with_store_env(&mut command, env),
        None => without_store_env(&mut command),
    }
    command.output().expect("run hudson mail draft")
}

fn draft_body(work: &TempDir) -> PathBuf {
    let body_file = work.path().join("body.txt");
    std::fs::write(&body_file, "Please review.").expect("write the draft body");
    body_file
}

#[tokio::test]
async fn a_draft_wears_the_sending_account_s_stored_signature_when_the_stores_answer() {
    let Some(stores) = store_env("mail_signature_runtime_stored").await else {
        return;
    };
    let attorney = seed_attorney(&stores, ATTORNEY_EMAIL, "Vesta Attorney").await;
    let colleague = seed_attorney(&stores, COLLEAGUE_EMAIL, "Vesta Colleague").await;
    let pg = signature_pool(&stores).await;
    seed_signature_row(&pg, attorney, ATTORNEY_SIGNATURE).await;
    seed_signature_row(&pg, colleague, COLLEAGUE_SIGNATURE).await;
    pg.close().await;

    let work = TempDir::new().expect("tempdir");
    let body_file = draft_body(&work);
    // The env var is set to a decoy: the runtime row must win over it.
    let result = draft_command(
        &body_file,
        ATTORNEY_EMAIL,
        Some(ENV_SIGNATURE),
        Some(&stores),
    );
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        stderr.contains(&runtime_provenance(attorney)),
        "the provenance line must name the runtime source, the sending account's Person, and the row's revision: {stderr}"
    );
    assert!(
        !stderr.contains(ENV_PROVENANCE) && !stderr.contains("source=env"),
        "a runtime-signed draft must not announce the env fallback: {stderr}"
    );
    assert!(
        !stderr.contains(COLLEAGUE_SIGNATURE),
        "another attorney's block must not appear anywhere in the run: {stderr}"
    );
    // The draft never leaves the machine: the failure is the connect stage's
    // missing refresh token, never a signature refusal.
    assert!(
        !result.status.success(),
        "a machine without a Gmail refresh token cannot save the draft"
    );
    assert!(
        !stderr.contains("signature needs review") && !stderr.contains("MissingConfiguration"),
        "the runtime block must reach the insertion seam unrefused: {stderr}"
    );
}

#[tokio::test]
async fn an_unreachable_deployment_falls_back_to_the_env_signature_announced() {
    let work = TempDir::new().expect("tempdir");
    let body_file = draft_body(&work);
    let result = draft_command(&body_file, ATTORNEY_EMAIL, Some(ENV_SIGNATURE), None);
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        stderr.contains("NAVIGATOR_GMAIL_SIGNATURE fallback"),
        "the env fallback must be announced on stderr, never silent: {stderr}"
    );
    assert!(
        stderr.contains(ENV_PROVENANCE),
        "the provenance line must name the env source: {stderr}"
    );
    assert!(
        !stderr.contains("source=runtime"),
        "an offline draft must not claim the runtime source: {stderr}"
    );
}

#[tokio::test]
async fn a_sending_account_with_no_person_falls_back_to_the_env_signature_announced() {
    let Some(stores) = store_env("mail_signature_runtime_ghost").await else {
        return;
    };
    let work = TempDir::new().expect("tempdir");
    let body_file = draft_body(&work);
    let result = draft_command(
        &body_file,
        "ghost@vestaestateplanning.com",
        Some(ENV_SIGNATURE),
        Some(&stores),
    );
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        stderr.contains("no Person matches the sending account"),
        "a sending account that is no Person must say so on stderr: {stderr}"
    );
    assert!(
        stderr.contains(ENV_PROVENANCE),
        "the provenance line must name the env source: {stderr}"
    );
}

#[tokio::test]
async fn a_person_with_no_row_falls_back_to_the_env_signature_announced() {
    let Some(stores) = store_env("mail_signature_runtime_unset").await else {
        return;
    };
    seed_attorney(&stores, ATTORNEY_EMAIL, "Vesta Attorney").await;
    let work = TempDir::new().expect("tempdir");
    let body_file = draft_body(&work);
    let result = draft_command(
        &body_file,
        ATTORNEY_EMAIL,
        Some(ENV_SIGNATURE),
        Some(&stores),
    );
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        stderr.contains("no stored signature row"),
        "a Person without a row must say so on stderr: {stderr}"
    );
    assert!(
        stderr.contains(ENV_PROVENANCE),
        "the provenance line must name the env source: {stderr}"
    );
}

#[tokio::test]
async fn neither_source_refuses_missing_configuration_before_any_gmail_contact() {
    let work = TempDir::new().expect("tempdir");
    let body_file = draft_body(&work);
    let result = draft_command(&body_file, ATTORNEY_EMAIL, None, None);
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        !result.status.success(),
        "a draft with neither signature source must refuse"
    );
    assert!(
        stderr.contains("NAVIGATOR_GMAIL_SIGNATURE"),
        "the missing-configuration refusal must name the env var: {stderr}"
    );
    assert!(
        !stderr.contains("signature: source="),
        "a refused draft prints no provenance line: {stderr}"
    );
}
