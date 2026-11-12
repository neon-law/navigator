//! Real-binary tests for the runtime letterhead source (LAW-266): a letter
//! rendered inside a Project whose `navigator.yaml` host selects a
//! non-default registered Brand wears the admin-editable `brand_letterhead`
//! row the deployment stores hold, and the render says so on stderr.
//! Offline, the compiled registry frames the letter and the fallback is
//! announced, never silent.
//!
//! The store-backed cases run the real `navigator` binary as a subprocess
//! against the same dependency tier the store's own fixtures use: the
//! server-mode `SurrealDB` lane (`store::test_support::server_surreal`,
//! skipped on a laptop without `NAVIGATOR_SURREAL_*`) and a disposable
//! PostgreSQL database holding the canonical schema
//! (`NAVIGATOR_TEST_POSTGRES_PORT`, skipped without it, failed when
//! `NAV_REQUIRE_POSTGRES=1` arms the tier). The offline-fallback case runs
//! everywhere, because its premise is that no store answers.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use store::postgres_schema::canonical_schema;
use store::surreal::record_id;
use store::surreal::DATABASE_ENV;
use store::test_support::server_surreal;
use tempfile::TempDir;
use views::brand::{BrandKey, DEFAULT_BRANDING};

const VESTA_HOST: &str = "www.vestaestateplanning.com";
const VESTA_WORDMARK: &str = "Vesta Estate Planning";
const VESTA_ENTITY: &str = "Vesta Estate Planning PC";
const VESTA_PHONE: &str = "+1 555 010 0001";
const VESTA_EMAIL: &str = "letters@vestaestateplanning.com";
const VESTA_SITE: &str = "https://www.vestaestateplanning.com";
const VESTA_POSTAL: &str = "1 Main Street\nSuite 200\nSpringfield, California 90000";

/// The Firm the fixture Brand belongs to. `seed_vesta_brand` derives the
/// Firm's slug from this name, so the two cannot drift.
const FIXTURE_FIRM_NAME: &str = "Vesta Fixture Firm";

/// The revision the fixture letterhead row carries — the `updated_at` the
/// provenance line names. Seeded explicitly so the assertion is
/// deterministic against `to_rfc3339`'s `+00:00` rendering.
const FIXTURE_REVISION_SEEDED: &str = "2026-11-05T22:41:19Z";

/// The provenance line the seeded row's render prints: the runtime source,
/// the Brand, the row's `updated_at` revision (`DateTime::to_rfc3339` renders
/// the whole-second UTC stamp with a `+00:00` offset), and the mark it drew.
const RUNTIME_PROVENANCE: &str =
    "letterhead: source=runtime brand=vesta revision=2026-11-05T22:41:19+00:00 mark=wordmark";

/// The default Brand's compiled wordmark as the PDF renders it — authored
/// mixed-case. (The Word renderer sets its name run in caps; nothing here
/// asserts that form against the default Brand.) This is the frame a letter
/// must NOT wear when the host selected Vesta.
const DEFAULT_WORDMARK: &str = "Neon Law";

const VALID: &str = "\
---
kind: letter
title: Test Demand
respondent_type: entity
confidential: true
output: letter
questionnaire:
  BEGIN:
    _: END
  END: {}
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: END
    rejected: END
  END: {}
---

# Demand

Pay the sum of `{{amount}}` without delay.
";

/// The store-backed environment one test renders in: a server-mode `SurrealDB`
/// database holding the seeded Brand row, and a disposable PostgreSQL
/// database holding the canonical schema and the seeded letterhead row.
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

/// The disposable PostgreSQL tier, in the shape `postgres_mirror`'s fixtures
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
    let database = format!("letterhead_runtime_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .expect("create the disposable letterhead database");
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
    .expect("connect to the disposable letterhead database");
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

/// The full store tier for one test, with the Brand row seeded so
/// `for_brand_key` resolves Vesta. `None` skips the caller.
async fn store_env(database: &str) -> Option<StoreEnv> {
    let surreal = server_surreal(database).await?;
    let (postgres_database, postgres_port) = fresh_postgres().await?;
    Some(StoreEnv {
        surreal,
        postgres_database,
        postgres_port,
    })
}

/// Seed the registered Vesta Brand the way production holds one: a Firm row
/// behind `firm_id`, the presentation fields the letterhead fallbacks read,
/// and — when the test asks — the uploaded mark's object key. Returns the
/// Brand's id, which the PostgreSQL row keys on.
async fn seed_vesta_brand(
    surreal: &store::test_support::ServerSurreal,
    with_logo: bool,
) -> uuid::Uuid {
    let firm_id = uuid::Uuid::now_v7();
    let brand_id = uuid::Uuid::now_v7();
    let now = "2026-11-05T22:41:19Z";
    let logo_fields = if with_logo {
        ", logo_object_key = $logo, logo_content_type = 'image/png'"
    } else {
        ""
    };
    surreal
        .db
        .query(format!(
            "CREATE $firm SET name = $firm_name, slug = $slug, \
             inserted_at = $now, updated_at = $now; \
             CREATE $id SET name = $name, brand_key = 'vesta', firm_id = $firm, \
             is_law_firm = true, legal_entity = $entity, website_url = $site{logo_fields}, \
             inserted_at = $now, updated_at = $now"
        ))
        .bind(("firm", record_id("firm", firm_id)))
        .bind(("firm_name", FIXTURE_FIRM_NAME.to_string()))
        // The applied schema tightens `slug` to `string READONLY` after its
        // backfill, so a Firm is born with the slug its name derives — the
        // way `store::firms::create` writes one.
        .bind(("slug", store::firms::slug::derive(FIXTURE_FIRM_NAME)))
        .bind(("id", record_id("brand", brand_id)))
        .bind(("name", "Vesta".to_string()))
        .bind(("entity", VESTA_ENTITY.to_string()))
        .bind(("site", VESTA_SITE.to_string()))
        .bind(("logo", "brands/vesta/mark.png".to_string()))
        .bind(("now", now.to_string()))
        .await
        .expect("seed the fixture Brand")
        .check()
        .expect("seed the fixture Brand");
    brand_id
}

/// Seed the admin-set letterhead row for the Brand `brand_id` — the row
/// `/app/admin/brands` edits, here written the way the admin page's save
/// leaves it.
async fn seed_letterhead_row(pg: &sqlx::PgPool, brand_id: uuid::Uuid, mark: &str) {
    sqlx::query(
        "INSERT INTO brand_letterhead \
         (brand_id, wordmark, mark, phone, email, site_url, postal_address, \
         inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
    )
    .bind(brand_id)
    .bind(VESTA_WORDMARK)
    .bind(mark)
    .bind(VESTA_PHONE)
    .bind(VESTA_EMAIL)
    .bind(VESTA_SITE)
    .bind(VESTA_POSTAL)
    .bind(
        chrono::DateTime::parse_from_rfc3339(FIXTURE_REVISION_SEEDED)
            .expect("the fixture revision is RFC 3339")
            .with_timezone(&chrono::Utc),
    )
    .execute(pg)
    .await
    .expect("seed the letterhead row");
}

/// The full store fixture for a store-backed render: the Vesta Brand (with
/// the uploaded mark only when the test asks) and the admin-set letterhead
/// row of `mark`. The store-backed tests call this before rendering.
async fn seed_vesta_letterhead_row(stores: &StoreEnv, mark: &str, with_logo: bool) {
    let brand_id = seed_vesta_brand(&stores.surreal, with_logo).await;
    let pg = sqlx::PgPool::connect_with(
        sqlx::postgres::PgConnectOptions::new()
            .host("127.0.0.1")
            .port(stores.postgres_port.parse().expect("test postgres port"))
            .username("postgres")
            .password("synthetic")
            .ssl_mode(sqlx::postgres::PgSslMode::Disable)
            .database(&stores.postgres_database),
    )
    .await
    .expect("connect to the disposable letterhead database");
    seed_letterhead_row(&pg, brand_id, mark).await;
    pg.close().await;
}

/// A letter template inside a Project whose `navigator.yaml` selects Vesta
/// by host.
fn letter_in_project(work: &TempDir) -> PathBuf {
    fs::write(
        work.path().join("navigator.yaml"),
        format!("version: v26.10.6\nproject:\n  name: acme\n  host: {VESTA_HOST}\n"),
    )
    .expect("write navigator.yaml");
    let src = work.path().join("demand.md");
    fs::write(&src, VALID).expect("write the letter template");
    src
}

/// The store-backed environment handed to a subprocess render: the test
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
/// case's premise.
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

fn render_notation(
    subcommand: &str,
    src: &Path,
    out: &Path,
    stores: Option<&StoreEnv>,
) -> std::process::Output {
    let mut command = Command::new(cargo_bin("hudson"));
    command
        .args(["notation", subcommand, "--preview"])
        .arg(src)
        .args(["--out", &out.to_string_lossy()]);
    match stores {
        Some(env) => with_store_env(&mut command, env),
        None => without_store_env(&mut command),
    }
    command.output().expect("run hudson notation")
}

/// The letterhead text of a rendered PDF, through poppler. The house skips
/// the text checks when pdftotext is not installed.
fn poppler() -> bool {
    Command::new("pdftotext").arg("-v").output().is_ok()
}

fn pdf_text(pdf: &Path) -> String {
    let out = Command::new("pdftotext")
        .arg(pdf)
        .arg("-")
        .output()
        .expect("pdftotext");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One part of a `.docx` package, read back out of the zip.
fn docx_part(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("docx is a zip");
    let mut xml = String::new();
    archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("docx carries {name}"))
        .read_to_string(&mut xml)
        .expect("read part");
    xml
}

/// Whether the `.docx` package carries `name` at all.
fn docx_has_part(bytes: &[u8], name: &str) -> bool {
    zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .expect("docx is a zip")
        .by_name(name)
        .is_ok()
}

#[tokio::test]
async fn a_runtime_row_frames_the_pdf_letter() {
    if !poppler() {
        eprintln!("skipped: pdftotext is not installed");
        return;
    }
    let Some(stores) = store_env("letterhead_runtime_pdf").await else {
        return;
    };
    seed_vesta_letterhead_row(&stores, "wordmark", false).await;
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.pdf");
    let result = render_notation("pdf", &src, &out, Some(&stores));
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(result.status.success(), "render failed: {stderr}");
    let text = pdf_text(&out);
    for line in [
        VESTA_WORDMARK,
        VESTA_ENTITY,
        VESTA_PHONE,
        VESTA_EMAIL,
        "1 Main Street",
        "Springfield, California 90000",
    ] {
        assert!(
            text.contains(line),
            "the PDF letterhead lacks `{line}`: {text}"
        );
    }
    assert!(
        !text.contains(DEFAULT_WORDMARK),
        "the letter must not wear the default Brand's wordmark: {text}"
    );
    assert!(
        stderr.contains(RUNTIME_PROVENANCE),
        "the provenance line must name the runtime source, brand, revision, and mark: {stderr}"
    );
    assert!(
        !stderr.contains("source=compiled") && !stderr.contains("deployment unreachable"),
        "a runtime render must not announce a fallback: {stderr}"
    );
}

#[tokio::test]
async fn a_runtime_row_frames_the_word_letter() {
    let Some(stores) = store_env("letterhead_runtime_word").await else {
        return;
    };
    seed_vesta_letterhead_row(&stores, "wordmark", false).await;
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.docx");
    let result = render_notation("word", &src, &out, Some(&stores));
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(result.status.success(), "render failed: {stderr}");
    let bytes = fs::read(&out).expect("docx written");
    let header = docx_part(&bytes, "word/header2.xml");
    // The Word letterhead's name run is set in caps — the renderer's
    // letterhead typography, pinned by `word`'s own unit test and mirrored
    // by the offline fallback test below. The remaining lines render as
    // authored.
    assert!(
        header.contains(VESTA_WORDMARK.to_uppercase().as_str()),
        "the Word letterhead lacks the wordmark: {header}"
    );
    for line in [
        VESTA_ENTITY,
        VESTA_PHONE,
        VESTA_EMAIL,
        "1 Main Street",
        "Springfield, California 90000",
    ] {
        assert!(
            header.contains(line),
            "the Word letterhead lacks `{line}`: {header}"
        );
    }
    assert!(
        !docx_has_part(&bytes, "word/media/letterhead-mark.png"),
        "a mark=wordmark row draws no uploaded logo part"
    );
    assert!(
        stderr.contains(RUNTIME_PROVENANCE),
        "the provenance line must name the runtime source: {stderr}"
    );
    assert!(
        !stderr.contains("source=compiled") && !stderr.contains("deployment unreachable"),
        "a runtime render must not announce a fallback: {stderr}"
    );
}

#[tokio::test]
async fn an_unfetchable_uploaded_mark_degrades_to_the_wordmark_loudly() {
    let Some(stores) = store_env("letterhead_runtime_logo").await else {
        return;
    };
    seed_vesta_letterhead_row(&stores, "logo", true).await;

    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.docx");
    let result = render_notation("word", &src, &out, Some(&stores));
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        result.status.success(),
        "an unfetchable mark degrades, never refuses: {stderr}"
    );
    assert!(
        stderr.contains("the Brand's uploaded mark could not be drawn"),
        "the degradation must be announced: {stderr}"
    );
    let header = docx_part(&fs::read(&out).expect("docx written"), "word/header2.xml");
    assert!(
        header.contains(VESTA_WORDMARK.to_uppercase().as_str()),
        "the degraded frame still carries the runtime wordmark: {header}"
    );
    assert!(
        stderr.contains(RUNTIME_PROVENANCE),
        "the provenance names the mark it actually drew: {stderr}"
    );
}

#[tokio::test]
async fn stores_with_no_row_frame_the_letter_on_the_compiled_registry_without_a_warning() {
    if !poppler() {
        eprintln!("skipped: pdftotext is not installed");
        return;
    }
    let Some(stores) = store_env("letterhead_runtime_norow").await else {
        return;
    };
    // The Brand row exists but no admin has set a letterhead: the runtime
    // source answers with the Brand's own fallback frame.
    seed_vesta_brand(&stores.surreal, false).await;
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.pdf");
    let result = render_notation("pdf", &src, &out, Some(&stores));
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(result.status.success(), "render failed: {stderr}");
    let text = pdf_text(&out);
    assert!(
        text.contains("Vesta") && !text.contains(DEFAULT_WORDMARK),
        "a Brand-known-but-unset letterhead wears the Brand's own fallback: {text}"
    );
    assert!(
        stderr.contains("letterhead: source=runtime brand=vesta"),
        "the source is still the runtime read: {stderr}"
    );
}

#[tokio::test]
async fn an_unreachable_deployment_renders_on_the_compiled_registry_and_says_so() {
    // No stores are needed: the premise is that none answer. Runs on every
    // machine, CI and laptop alike.
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.pdf");
    let result = render_notation("pdf", &src, &out, None);
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        result.status.success(),
        "a plain connectivity miss renders, never refuses: {stderr}"
    );
    assert!(
        stderr.contains("letterhead: source=compiled brand=vesta"),
        "the provenance names the compiled source: {stderr}"
    );
    assert!(
        stderr.contains("deployment unreachable") && stderr.contains("using compiled registry"),
        "the fallback must be announced, never silent: {stderr}"
    );
    assert!(
        fs::read(&out).is_ok(),
        "the artifact is still written on the fallback"
    );
}

#[tokio::test]
async fn an_unreachable_deployment_frames_the_word_letter_on_the_compiled_registry() {
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.docx");
    let result = render_notation("word", &src, &out, None);
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(result.status.success(), "render failed: {stderr}");
    let branding = BrandKey::Vesta.resolve_branding(&DEFAULT_BRANDING);
    let header = docx_part(&fs::read(&out).expect("docx written"), "word/header2.xml");
    assert!(
        header.contains(&branding.firm.site_name.to_uppercase()),
        "the compiled wordmark frames the offline letter: {header}"
    );
    assert!(
        stderr.contains("letterhead: source=compiled brand=vesta")
            && stderr.contains("using compiled registry"),
        "the Word render announces the same fallback: {stderr}"
    );
}
