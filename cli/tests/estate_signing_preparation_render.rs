//! LAW-83 regression: `templates/notations/neon_law/estate_signing_preparation__letter.md` —
//! the reusable "Prepare for your Signing" email the estate-signing checklist
//! in `skills/nv/estates.md` names — must validate and render with complete
//! synthetic answers for every questionnaire state it declares, and every
//! answer must actually reach the rendered page.
//!
//! `navigator notation pdf` validates a template against the same rule set as
//! `navigator validate` before rendering (see `cli::run_render`), so a
//! successful render here is also proof the checked-in template passes the
//! gate. This test points at the real repository file rather than an inline
//! fixture, so a future edit to the template itself is what this test
//! exercises.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("workspace root exists")
}

fn template_path() -> PathBuf {
    workspace_root().join("templates/notations/neon_law/estate_signing_preparation__letter.md")
}

#[test]
fn estate_signing_preparation_validates_and_renders_with_synthetic_answers() {
    let template = template_path();
    assert!(
        template.exists(),
        "expected {} to exist",
        template.display()
    );

    let work = TempDir::new().unwrap();
    let out = work.path().join("estate_signing_preparation.pdf");

    // Synthetic per docs/public-contributor-safety.md: no real client, matter,
    // or address. One answer per questionnaire state the template declares.
    let result = Command::new(cargo_bin("navigator"))
        .args(["notation", "pdf"])
        .arg(&template)
        .arg("--out")
        .arg(&out)
        .args([
            "--answer",
            "person__client.name=Marisol Kestrel",
            "--answer",
            "project__engagement.name=Estate of Marisol Kestrel",
            "--answer",
            "address__signing_location=400 Nevada Highway, Las Vegas, NV 89101",
            "--answer",
            "custom_datetime__signing_start=2026-11-10 10:00 AM",
            "--answer",
            "custom_datetime__signing_end=2026-11-10 11:30 AM",
        ])
        .output()
        .expect("run navigator notation pdf");

    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let bytes = fs::read(&out).expect("pdf written");
    assert_eq!(&bytes[..4], b"%PDF", "output is not a PDF");
    pdf::validate_pdf(&bytes)
        .expect("the rendered estate-signing-preparation letter must be a valid PDF");

    for needle in [
        "Marisol Kestrel",
        "Estate of Marisol Kestrel",
        "400 Nevada Highway, Las Vegas, NV 89101",
        "2026-11-10 10:00 AM",
        "2026-11-10 11:30 AM",
    ] {
        assert!(
            pdf::occurrence_count(&bytes, needle)
                .unwrap_or_else(|e| panic!("scan the rendered pdf for `{needle}`: {e}"))
                >= 1,
            "synthetic answer `{needle}` must reach the rendered letter"
        );
    }
}
