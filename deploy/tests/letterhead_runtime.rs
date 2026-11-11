//! Real-binary tests for the runtime letterhead source (LAW-266, LAW-341):
//! a letter rendered inside a Project whose `navigator.yaml` host selects a
//! non-default registered Brand wears the admin-editable `brand_letterhead`
//! row the signed-in deployment serves — read over HTTPS from
//! `GET /app/api/brands/{key}/letterhead` with the `site login` bearer, the
//! same door `/app/admin/brands` saves through — and the render says so on
//! stderr. Offline, the compiled registry frames the letter and the
//! fallback is announced, never silent.
//!
//! The deployment is stood in by a wiremock double: each test resolves the
//! row the way the deployment would (`store::brand_letterheads::for_brand_key`
//! against seeded stores, skipped on a laptop without the dependency tier)
//! and serves the resulting JSON from the door's exact path, so the binary
//! reads what a real deployment holds. The parity test renders the same
//! resolved row in-process through the same `frame_from_row` the CLI
//! applies, and requires byte-identical `.docx` output. The `--ci` cases
//! prove the row read is a pinned render input (LAW-19): cached bytes or a
//! refusal, never a silent fresh read.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use store::postgres::PostgresDb;
use store::postgres_schema::canonical_schema;
use store::surreal::record_id;
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
/// deterministic; the store serializes `DateTime<Utc>` with a `Z` offset,
/// and the render prints the row's stamp verbatim.
const FIXTURE_REVISION_SEEDED: &str = "2026-11-05T22:41:19Z";

/// The provenance line the seeded row's render prints: the runtime source,
/// the Brand, the row's `updated_at` revision (`DateTime<Utc>`'s serde
/// form, RFC 3339 with a `Z` offset — same stamp `FIXTURE_REVISION_SEEDED`
/// serves), and the mark it drew.
const RUNTIME_PROVENANCE: &str =
    "letterhead: source=runtime brand=vesta revision=2026-11-05T22:41:19Z mark=wordmark";

/// The default Brand's compiled wordmark as the PDF renders it — authored
/// mixed-case. (The Word renderer sets its name run in caps; nothing here
/// asserts that form against the default Brand.) This is the frame a letter
/// must NOT wear when the host selected Vesta.
const DEFAULT_WORDMARK: &str = "Neon Law";

/// The letterhead row is a pinned render input (LAW-19): under `--ci` a
/// cache miss refuses the render instead of re-reading the deployment.
const CI_REFUSAL: &str = "--ci refuses a letter whose letterhead row could not be read";

const VALID_BODY: &str = "\n# Demand\n\nPay the sum of `{{amount}}` without delay.\n";

/// The full template: the frontmatter the gate parses, then the body the
/// renderer receives (`strip_frontmatter`'s output).
const VALID: &str = concat!(
    "---\nkind: letter\ntitle: Test Demand\nrespondent_type: entity\nconfidential: true\noutput: letter\nquestionnaire:\n  BEGIN:\n    _: END\n  END: {}\nworkflow:\n  BEGIN:\n    intake_submitted: lawyer_review\n  lawyer_review:\n    approved: END\n    rejected: END\n  END: {}\n---\n",
    "\n# Demand\n\nPay the sum of `{{amount}}` without delay.\n"
);

/// The binary's own `strip_frontmatter` rule, mirrored so the parity test's
/// in-process body is exactly the body the binary renders. Pinned against
/// `strip_frontmatter_returns_the_body_after_the_closing_marker` in main.rs.
fn strip_frontmatter(contents: &str) -> &str {
    let Some(after_open) = contents.strip_prefix("---\n") else {
        return contents;
    };
    if let Some(end) = after_open.find("\n---\n") {
        return after_open[end + "\n---\n".len()..].trim_start_matches('\n');
    }
    // Closer at EOF with no body, or no closer at all.
    after_open.strip_suffix("\n---").map_or(contents, |_| "")
}

#[test]
fn the_template_body_is_what_the_binary_renders() {
    assert_eq!(
        strip_frontmatter(VALID),
        VALID_BODY.trim_start_matches('\n')
    );
}

/// The store-backed environment one test seeds through: a server-mode
/// `SurrealDB` database holding the Brand row (`store::test_support::
/// server_surreal`, skipped on a laptop without `NAVIGATOR_SURREAL_*`) and
/// a disposable PostgreSQL database holding the canonical schema and the
/// seeded letterhead row. The binary never sees these coordinates — it
/// reads the row through the door double like any other client.
struct StoreEnv {
    surreal: store::test_support::ServerSurreal,
    postgres_database: String,
    postgres_port: String,
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
    let admin = sqlx::PgPool::connect_with(pg_options(&admin_database, &port))
        .await
        .expect("connect to the test PostgreSQL instance");
    let database = format!("letterhead_runtime_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .expect("create the disposable letterhead database");
    admin.close().await;
    let setup = sqlx::PgPool::connect_with(pg_options(&database, &port))
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

/// The loopback connection options the tier's fixtures use.
fn pg_options(database: &str, port: &str) -> sqlx::postgres::PgConnectOptions {
    sqlx::postgres::PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().expect("test postgres port"))
        .username("postgres")
        .password("synthetic")
        .ssl_mode(sqlx::postgres::PgSslMode::Disable)
        .database(database)
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
    let pg =
        sqlx::PgPool::connect_with(pg_options(&stores.postgres_database, &stores.postgres_port))
            .await
            .expect("connect to the disposable letterhead database");
    seed_letterhead_row(&pg, brand_id, mark).await;
    pg.close().await;
}

/// The resolved letterhead row the store holds for the seeded Vesta Brand,
/// serialized exactly as the deployment door serves it: `for_brand_key` is
/// the resolve the admin save answers with, so the double serves what a
/// real deployment holds.
async fn door_row_from_stores(stores: &StoreEnv) -> String {
    let pg = PostgresDb::connect(pg_options(&stores.postgres_database, &stores.postgres_port))
        .await
        .expect("connect to the disposable letterhead database");
    let resolved = store::brand_letterheads::for_brand_key(&stores.surreal.db, &pg, "vesta")
        .await
        .expect("the letterhead resolve answers")
        .expect("the seeded Brand resolves a row");
    serde_json::to_string(&resolved).expect("the resolved row serializes")
}

/// A hand-built row in the exact shape the resolve answers — for the tests
/// that need no store tier.
fn served_row() -> String {
    serde_json::json!({
        "brand_key": "vesta",
        "wordmark": VESTA_WORDMARK,
        "mark": "wordmark",
        "firm_legal_name": VESTA_ENTITY,
        "phone": VESTA_PHONE,
        "email": VESTA_EMAIL,
        "web": VESTA_SITE,
        "postal_address": VESTA_POSTAL,
        "updated_at": FIXTURE_REVISION_SEEDED
    })
    .to_string()
}

/// The door double: `GET /app/api/brands/vesta/letterhead` answers `body`.
async fn mount_row_double(server: &wiremock::MockServer, body: String) {
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/app/api/brands/vesta/letterhead"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
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

/// A plain binary render. No session exists in the test environment, so the
/// letterhead read falls back to the compiled registry unless the test
/// points the door double at the binary.
fn render_notation(subcommand: &str, src: &Path, out: &Path) -> std::process::Output {
    let mut command = Command::new(cargo_bin("navigator-deploy"));
    command
        .args(["notation", subcommand, "--preview"])
        .arg(src)
        .args(["--out", &out.to_string_lossy()]);
    command.output().expect("run navigator-deploy notation")
}

/// Point the binary's letterhead row read at `base` — a door double or a
/// deliberately dead port — with the row cache pinned to `cache`, so runs
/// never touch the developer's own cache.
fn with_door(command: &mut Command, base: &str, cache: &TempDir, ci: bool) {
    command.env("NAVIGATOR_LETTERHEAD_API_BASE", base);
    command.env("NAVIGATOR_LETTERHEAD_CACHE", cache.path());
    if ci {
        command.arg("--ci");
    }
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
async fn the_deployment_row_frames_the_pdf_letter() {
    if !poppler() {
        eprintln!("skipped: pdftotext is not installed");
        return;
    }
    let Some(stores) = store_env("letterhead_runtime_pdf").await else {
        return;
    };
    seed_vesta_letterhead_row(&stores, "wordmark", false).await;
    let row = door_row_from_stores(&stores).await;
    let server = wiremock::MockServer::start().await;
    mount_row_double(&server, row).await;
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.pdf");
    let mut command = Command::new(cargo_bin("navigator-deploy"));
    command
        .args(["notation", "pdf", "--preview"])
        .arg(&src)
        .args(["--out", &out.to_string_lossy()]);
    with_door(&mut command, &server.uri(), &work, false);
    let result = command.output().expect("run navigator notation");
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
async fn the_deployment_row_frames_the_word_letter() {
    let Some(stores) = store_env("letterhead_runtime_word").await else {
        return;
    };
    seed_vesta_letterhead_row(&stores, "wordmark", false).await;
    let row = door_row_from_stores(&stores).await;
    let server = wiremock::MockServer::start().await;
    mount_row_double(&server, row).await;
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.docx");
    let mut command = Command::new(cargo_bin("navigator-deploy"));
    command
        .args(["notation", "word", "--preview"])
        .arg(&src)
        .args(["--out", &out.to_string_lossy()]);
    with_door(&mut command, &server.uri(), &work, false);
    let result = command.output().expect("run navigator notation");
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

/// The LAW-341 parity acceptance: the door-served row — resolved by the
/// real store resolve and served by the double — frames a `.docx`
/// byte-identical to the same row framed in-process through the same
/// `frame_from_row` the CLI applies. The HTTPS move changes where the row
/// travels, never the render it produces.
#[tokio::test]
async fn the_word_render_matches_the_store_resolve_byte_for_byte() {
    let Some(stores) = store_env("letterhead_runtime_parity").await else {
        return;
    };
    seed_vesta_letterhead_row(&stores, "wordmark", false).await;
    let door_row = door_row_from_stores(&stores).await;
    let server = wiremock::MockServer::start().await;
    mount_row_double(&server, door_row.clone()).await;

    // Render A: the binary reads the row through the door double.
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.docx");
    let mut command = Command::new(cargo_bin("navigator-deploy"));
    command
        .args(["notation", "word", "--preview"])
        .arg(&src)
        .args(["--out", &out.to_string_lossy()]);
    with_door(&mut command, &server.uri(), &work, false);
    let result = command.output().expect("run navigator notation");
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(result.status.success(), "the door render failed: {stderr}");
    let door_bytes = fs::read(&out).expect("the door render wrote a docx");

    // Render B: the same resolved row, framed in-process through the same
    // `frame_from_row` the CLI applies, rendered by the word renderer with
    // the binary's provenance contract (template name and hash, no source
    // revision in a tempdir).
    let row: store::brand_letterheads::BrandLetterhead =
        serde_json::from_str(&door_row).expect("the served row parses");
    let frame = cli::letterhead::frame_from_row(&row, pdf::LetterheadMark::Wordmark);
    let template_bytes = fs::read(&src).expect("the template bytes");
    let template_hash = word::template_hash(&template_bytes);
    let rendered = word::render_notation(
        VALID_BODY.trim_start_matches('\n'),
        Some(&word::RenderLetterhead {
            name: &frame.name,
            entity: &frame.entity,
            phone: &frame.phone,
            email: &frame.email,
            web: &frame.web,
            postal_address: &frame.postal_address,
            font_family: &frame.font_family,
            logo_png: &[],
        }),
        &word::RenderOptions {
            source_revision: None,
            pleading: None,
            provenance: Some(word::Provenance {
                template_path: "demand.md",
                template_hash: &template_hash,
            }),
            preview: true,
        },
    )
    .expect("the in-process render succeeds");
    assert_eq!(
        door_bytes, rendered,
        "the door-served row must frame the exact bytes the store resolve frames"
    );
}

#[tokio::test]
async fn an_unfetchable_uploaded_mark_degrades_to_the_wordmark_loudly() {
    let Some(stores) = store_env("letterhead_runtime_logo").await else {
        return;
    };
    seed_vesta_letterhead_row(&stores, "logo", true).await;
    let row = door_row_from_stores(&stores).await;
    // The assets route is deliberately unmounted: the door answers the row,
    // the mark's bytes are not there.
    let server = wiremock::MockServer::start().await;
    mount_row_double(&server, row).await;
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.docx");
    let mut command = Command::new(cargo_bin("navigator-deploy"));
    command
        .args(["notation", "word", "--preview"])
        .arg(&src)
        .args(["--out", &out.to_string_lossy()]);
    with_door(&mut command, &server.uri(), &work, false);
    let result = command.output().expect("run navigator notation");
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
async fn a_brand_with_no_admin_row_wears_its_own_fallback_frame() {
    if !poppler() {
        eprintln!("skipped: pdftotext is not installed");
        return;
    }
    let Some(stores) = store_env("letterhead_runtime_norow").await else {
        return;
    };
    // The Brand row exists but no admin has set a letterhead: the door's
    // resolve still answers, with the Brand's own fallback frame.
    seed_vesta_brand(&stores.surreal, false).await;
    let row = door_row_from_stores(&stores).await;
    let server = wiremock::MockServer::start().await;
    mount_row_double(&server, row).await;
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.pdf");
    let mut command = Command::new(cargo_bin("navigator-deploy"));
    command
        .args(["notation", "pdf", "--preview"])
        .arg(&src)
        .args(["--out", &out.to_string_lossy()]);
    with_door(&mut command, &server.uri(), &work, false);
    let result = command.output().expect("run navigator notation");
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
    // No session and no door double are needed: the premise is that the
    // deployment cannot be asked. Runs on every machine, CI and laptop
    // alike. The fallback is the Project's owning Firm brand — the compiled
    // registry for the host-selected key — never the Neon House Brand.
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.pdf");
    let result = render_notation("pdf", &src, &out);
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
    let result = render_notation("word", &src, &out);
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

/// Under `--ci` a letterhead read that cannot answer refuses the render:
/// a CI render's bytes must not depend on which answer a live read would
/// have given (LAW-19). Here the door is a dead port and the cache is
/// empty — the render gets pinned bytes or nothing.
#[tokio::test]
async fn ci_refuses_a_render_whose_letterhead_row_is_not_cached() {
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let out = work.path().join("demand.pdf");
    let cache = TempDir::new().expect("temp cache");
    let mut command = Command::new(cargo_bin("navigator-deploy"));
    command
        .args(["notation", "pdf", "--preview"])
        .arg(&src)
        .args(["--out", &out.to_string_lossy()]);
    // A port nothing listens on: the door cannot answer, and the cache is
    // empty, so --ci refuses rather than falling back.
    with_door(&mut command, "http://127.0.0.1:1", &cache, true);
    let result = command.output().expect("run navigator notation");
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        !result.status.success(),
        "--ci must refuse a letterhead read it cannot pin: {stderr}"
    );
    assert!(
        stderr.contains(CI_REFUSAL),
        "the refusal must name the pinned-bytes rule: {stderr}"
    );
    assert!(
        !out.exists(),
        "a refused render writes no artifact: {stderr}"
    );
}

/// The cache is the pin: after one non-`--ci` render warms it from the door
/// double, a `--ci` render reads the cached row and never asks the door
/// again — proven by the double's `expect(1)` verification, which fails if
/// a second request arrives.
#[tokio::test]
async fn ci_renders_from_a_warmed_cache_without_asking_the_door_again() {
    let server = wiremock::MockServer::start().await;
    mount_row_double(&server, served_row()).await;
    let work = TempDir::new().expect("tempdir");
    let src = letter_in_project(&work);
    let cache = TempDir::new().expect("temp cache");

    // Render 1, not under --ci: fetches the row from the door and warms
    // the cache.
    let out = work.path().join("warm.docx");
    let mut command = Command::new(cargo_bin("navigator-deploy"));
    command
        .args(["notation", "word", "--preview"])
        .arg(&src)
        .args(["--out", &out.to_string_lossy()]);
    with_door(&mut command, &server.uri(), &cache, false);
    let result = command.output().expect("run navigator notation");
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        result.status.success(),
        "the warm-up render failed: {stderr}"
    );
    assert!(
        stderr.contains(RUNTIME_PROVENANCE),
        "the warm-up render reads the runtime row: {stderr}"
    );

    // Render 2, under --ci: pinned to the cache; the door is never asked.
    // Rendered under pdf, the subcommand that threads `--ci` through
    // run_render.
    let out = work.path().join("pinned.pdf");
    let mut command = Command::new(cargo_bin("navigator-deploy"));
    // `with_door(.., true)` adds `--ci`; the pdf subcommand threads it
    // through run_render.
    command
        .args(["notation", "pdf", "--preview"])
        .arg(&src)
        .args(["--out", &out.to_string_lossy()]);
    with_door(&mut command, &server.uri(), &cache, true);
    let result = command.output().expect("run navigator notation");
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    assert!(
        result.status.success(),
        "a --ci render on a warm cache succeeds: {stderr}"
    );
    assert!(
        stderr.contains(RUNTIME_PROVENANCE),
        "the cached row is still the runtime source, with the same revision: {stderr}"
    );
    let received = server
        .received_requests()
        .await
        .expect("the double records requests");
    assert_eq!(
        received.len(),
        1,
        "the door was asked exactly once: the --ci render must read the cache, never the door"
    );
    assert!(
        fs::read(&out).is_ok(),
        "the pinned render wrote its artifact"
    );
}
