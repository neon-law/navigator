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

use tempfile::TempDir;
use views::brand::{BrandKey, DEFAULT_BRANDING};

const VESTA_HOST: &str = "www.vestaestateplanning.com";
const VESTA_WORDMARK: &str = "Vesta Estate Planning";
const VESTA_ENTITY: &str = "Vesta Estate Planning PC";
const VESTA_PHONE: &str = "+1 555 010 0001";
const VESTA_EMAIL: &str = "letters@vestaestateplanning.com";
const VESTA_SITE: &str = "https://www.vestaestateplanning.com";
const VESTA_POSTAL: &str = "1 Main Street\nSuite 200\nSpringfield, California 90000";

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
    let mut command = Command::new(cargo_bin("hudson"));
    command
        .args(["notation", subcommand, "--preview"])
        .arg(src)
        .args(["--out", &out.to_string_lossy()]);
    command.output().expect("run hudson notation")
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
    let mut command = Command::new(cargo_bin("hudson"));
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
    let mut command = Command::new(cargo_bin("hudson"));
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
    let mut command = Command::new(cargo_bin("hudson"));
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
