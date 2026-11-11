//! End-to-end tests for `output: form` — the `AcroForm` fill branch of
//! `navigator notation pdf` (LAW-306).
//!
//! Each test stages a workspace in a tempdir: the vendored naturalization
//! template (the LAW-306 repro's exact frontmatter, copied from
//! `templates/notations/forms/`) plus a synthetic blank whose `.sha256`
//! pin file matches — the pin file wins over the compiled-in pin, the
//! same rule `forms sync` follows, so a test can stage a blank it
//! authored. Every run uses a temp CWD so the binary's `.devx/env` /
//! `.env` auto-load finds nothing, and a temp `NAVIGATOR_FORM_CACHE`, so
//! neither the runner's environment nor cache can leak in.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// The vendored template whose frontmatter is the LAW-306 repro:
/// `kind: filing` + `output: form` + `form:` + `origin_url:`.
const REPRO_TEMPLATE: &str =
    "templates/notations/forms/united_states/federal/uscis/us__naturalization.md";

/// The form code the repro template declares — a real registry entry, so
/// the tests can stage blanks and pins for it.
const CODE: &str = "us__naturalization";

/// The registry's canonical origin URL for `CODE` — half of the blank
/// cache key.
const ORIGIN_URL: &str = "https://www.uscis.gov/n-400";

/// The question-path `AcroForm` fields the synthetic blank carries — both
/// are names in the form's tracked `.fields` manifest, which is what
/// `forms::fill_values` resolves answers against.
const FIELD_GIVEN: &str = "person__client.given";
const FIELD_FAMILY: &str = "person__client.family";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate sits one level under the workspace root")
        .to_path_buf()
}

/// A blank whose pin the test controls: a minimal valid `AcroForm` with
/// the two question-path fields, plus the digest the pin file carries.
fn synthetic_blank() -> Vec<u8> {
    pdf::blank_acroform(&[FIELD_GIVEN, FIELD_FAMILY])
}

/// Stage a workspace: the repro template at `templates/notations/`, and
/// the forms shelf directory the workspace walk looks for. Returns the
/// workspace root and the staged template path.
fn stage_workspace(template_contents: &str) -> (TempDir, PathBuf) {
    let root = TempDir::new().expect("temp workspace");
    let templates = root.path().join("templates/notations");
    fs::create_dir_all(templates.join("forms/united_states/federal/uscis"))
        .expect("stage the forms shelf");
    // N110: a template must live under `notations/neon_law/` or
    // `notations/forms/`, and a form filename stem must start with the
    // jurisdiction prefix — stage the repro inside the forms shelf under
    // the `us__` prefix.
    let template = templates.join("forms/united_states/federal/uscis/us__n400_repro.md");
    fs::write(&template, template_contents).expect("stage the template");
    (root, template)
}

/// Stage the vendored working copy + pin file for `CODE` at `root`.
fn stage_working_copy(root: &Path, blank: &[u8]) {
    let copy = root
        .join("templates/notations")
        .join("forms/united_states/federal/uscis")
        .join(format!("{CODE}.pdf"));
    fs::write(&copy, blank).expect("stage the working copy");
    fs::write(copy.with_extension("sha256"), forms::sha256_hex(blank)).expect("stage the pin file");
}

/// The repro template's frontmatter, verbatim from the vendored file —
/// the test case the acceptance criteria name. `origin_url` must stay
/// the registry's: the cache key is built from it.
fn repro_template() -> String {
    fs::read_to_string(workspace_root().join(REPRO_TEMPLATE))
        .expect("read the vendored repro template")
}

fn render(template: &Path, out: &Path, cache: &Path, ci: bool) -> assert_cmd::assert::Assert {
    let mut command = Command::cargo_bin("navigator").expect("navigator binary");
    command
        .current_dir(template.parent().expect("template parent"))
        .env("NAVIGATOR_FORM_CACHE", cache)
        .env_remove("NAVIGATOR_STORAGE_BACKEND")
        .env_remove("NAVIGATOR_ASSETS_BUCKET")
        .args([
            "notation",
            "pdf",
            template.to_str().expect("template path"),
            "--out",
            out.to_str().expect("out path"),
            "--answer",
            &format!("{FIELD_GIVEN}=Nick"),
            "--answer",
            &format!("{FIELD_FAMILY}=Shook"),
        ]);
    if ci {
        command.arg("--ci");
    }
    command.assert()
}

/// A registered, pin-verified blank fills from `notation pdf` — and the
/// output keeps its live `AcroForm` fields (unflattened: the CLI's product
/// is the attorney-review artifact; the walk's post-lawyer-review step
/// owns the flatten).
#[test]
fn a_registered_vendored_blank_fills_unflattened() {
    let blank = synthetic_blank();
    let (root, template) = stage_workspace(&repro_template());
    stage_working_copy(root.path(), &blank);
    let cache = TempDir::new().expect("temp cache");
    let out = root.path().join("out.pdf");

    render(&template, &out, cache.path(), true).success();

    let filled = fs::read(&out).expect("filled PDF");
    let names = pdf::field_names(&filled).expect("readable AcroForm");
    assert!(
        !names.is_empty(),
        "the rendered form must keep its live fields — flattening belongs to the walk"
    );
    assert_eq!(
        pdf::read_field_value(&filled, FIELD_GIVEN).as_deref(),
        Some("Nick")
    );
    assert_eq!(
        pdf::read_field_value(&filled, FIELD_FAMILY).as_deref(),
        Some("Shook")
    );
}

/// The old defect: `output: form` fell through `unwrap_or_default()` to
/// a plain Typst page and exited 0. An unregistered code must refuse
/// instead — non-zero, no document, no "Rendered" line.
#[test]
fn a_form_template_never_falls_back_to_a_plain_render() {
    let template =
        repro_template().replace(&format!("form: {CODE}"), "form: no_such_registry_entry");
    let (root, template) = stage_workspace(&template);
    let cache = TempDir::new().expect("temp cache");
    let out = root.path().join("out.pdf");

    render(&template, &out, cache.path(), false)
        .failure()
        .code(2)
        .stdout(predicate::str::contains("Rendered").not())
        .stderr(predicate::str::contains(
            "vendored forms registry (`templates/notations/forms/`)",
        ));

    assert!(
        !out.exists(),
        "a refused form render must not leave a document behind"
    );
}

/// A cache miss under `--ci` refuses rather than downloading — LAW-19:
/// never a fresh fetch that could change the bytes between runs.
#[test]
fn a_cache_miss_under_ci_refuses_the_download() {
    let (root, template) = stage_workspace(&repro_template());
    // Pin file present (it is tracked in the repo) but no working copy:
    // the blank is neither vendored nor cached.
    let pin = forms::sha256_hex(&synthetic_blank());
    let shelf = root
        .path()
        .join("templates/notations/forms/united_states/federal/uscis");
    fs::write(shelf.join(format!("{CODE}.sha256")), pin).expect("stage the pin file");
    let cache = TempDir::new().expect("empty temp cache");
    let out = root.path().join("out.pdf");

    render(&template, &out, cache.path(), true)
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--ci refuses to fetch"));
}

/// A cached blank serves a `--ci` render — and the cache key is the
/// template's `origin_url` plus the content hash, so a template
/// re-pointed at a different origin misses rather than serving bytes
/// pinned under another source.
#[test]
fn a_cached_blank_serves_a_ci_render_keyed_on_origin_and_hash() {
    let blank = synthetic_blank();
    let pin = forms::sha256_hex(&blank);
    let (root, template) = stage_workspace(&repro_template());
    let shelf = root
        .path()
        .join("templates/notations/forms/united_states/federal/uscis");
    fs::write(shelf.join(format!("{CODE}.sha256")), &pin).expect("stage the pin file");
    let cache = TempDir::new().expect("temp cache");
    let key = forms::sha256_hex(format!("{ORIGIN_URL}\n{pin}").as_bytes());
    fs::write(cache.path().join(format!("{key}.pdf")), &blank).expect("seed the blank cache");
    let out = root.path().join("out.pdf");

    render(&template, &out, cache.path(), true).success();

    let filled = fs::read(&out).expect("filled PDF");
    assert_eq!(
        pdf::read_field_value(&filled, FIELD_GIVEN).as_deref(),
        Some("Nick")
    );

    // Same cache, different `origin_url`: a miss, refused under `--ci`.
    let repointed = repro_template().replace(
        "origin_url: https://www.uscis.gov/n-400",
        "origin_url: https://example.gov/n-400",
    );
    let (root2, template2) = stage_workspace(&repointed);
    let shelf2 = root2
        .path()
        .join("templates/notations/forms/united_states/federal/uscis");
    fs::create_dir_all(&shelf2).expect("stage the forms shelf");
    fs::write(shelf2.join(format!("{CODE}.sha256")), &pin).expect("stage the pin file");
    let out2 = root2.path().join("out.pdf");
    render(&template2, &out2, cache.path(), true)
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--ci refuses to fetch"));
}

/// Bytes that contradict their own pin refuse — never filled, never
/// silently replaced by another source.
#[test]
fn a_working_copy_that_contradicts_its_pin_refuses() {
    let blank = synthetic_blank();
    let (root, template) = stage_workspace(&repro_template());
    stage_working_copy(root.path(), &blank);
    // Tamper with the vendored bytes after pinning: the committed pin
    // now names bytes the working copy does not carry.
    let copy = root
        .path()
        .join("templates/notations/forms/united_states/federal/uscis/us__naturalization.pdf");
    fs::write(&copy, b"tampered - not the pinned bytes").expect("tamper");
    let cache = TempDir::new().expect("temp cache");
    let out = root.path().join("out.pdf");

    render(&template, &out, cache.path(), false)
        .failure()
        .code(2)
        .stderr(predicate::str::contains(".sha256` pin"));
}

/// An `AcroForm` fill has no Word frame: `notation word` on a form
/// template refuses instead of rendering a typeset page.
#[test]
fn a_form_template_refuses_the_word_render() {
    let blank = synthetic_blank();
    let (root, template) = stage_workspace(&repro_template());
    stage_working_copy(root.path(), &blank);
    let cache = TempDir::new().expect("temp cache");
    let out = root.path().join("out.docx");

    Command::cargo_bin("navigator")
        .expect("navigator binary")
        .current_dir(template.parent().expect("template parent"))
        .env("NAVIGATOR_FORM_CACHE", cache.path())
        .args([
            "notation",
            "word",
            template.to_str().expect("template path"),
            "--out",
            out.to_str().expect("out path"),
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("has no Word frame"));
}
