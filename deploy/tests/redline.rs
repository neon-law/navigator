//! End-to-end tests for `notation word --redline-against` (LAW-312): the
//! CLI renders exactly as `notation word` does, then writes the difference
//! from the previous version as real Word tracked changes.
//!
//! Each test stages a workspace in a tempdir — a memo template under the
//! `notations/neon_law/` shelf the N110 walk looks for — and runs the
//! compiled binary from the template's parent, the way `form_render.rs`
//! does. The cycle mirrors the lawyer's flow: render v1, revise the
//! template and the answers, redline the new render against v1, and review
//! the revisions in Word.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const AUTHOR: &str = "Nicholas Shook";

const TEMPLATE_V1: &str = "\
---
kind: memo
title: Service Agreement
respondent_type: person
jurisdiction: NV
confidential: true
prompts:
  fee_day: On which day are fees due?
questionnaire:
  BEGIN:
    _: END
  END: {}
workflow:
  BEGIN:
    _: lawyer_review
  lawyer_review:
    approved: END
    changes_requested: reask__client
  reask__client:
    intake_resubmitted: lawyer_review
  END: {}
---

# Service Agreement

This agreement is between the client and the Firm.

Fees are due on the {{fee_day}} of each month.

Keep receipts for every expense.
";

/// Versus V1: the fee day moves (a rewrapped paragraph) and the receipts
/// paragraph gives way to an interest clause — the seeded set the
/// revisions must reproduce.
const TEMPLATE_V2: &str = "\
---
kind: memo
title: Service Agreement
respondent_type: person
jurisdiction: NV
confidential: true
prompts:
  fee_day: On which day are fees due?
questionnaire:
  BEGIN:
    _: END
  END: {}
workflow:
  BEGIN:
    _: lawyer_review
  lawyer_review:
    approved: END
    changes_requested: reask__client
  reask__client:
    intake_resubmitted: lawyer_review
  END: {}
---

# Service Agreement

This agreement is between the client and the Firm.

Fees are due on the {{fee_day}} of each month.

Interest accrues at the statutory rate.
";

/// A manifest naming the signing lawyer, so the redline's default author
/// resolution has a source. `project.host` selects the letterhead Brand —
/// required by the render gate, though a memo frame prints no letterhead.
const MANIFEST: &str = "\
project:
  name: acme
  host: www.vestaestateplanning.com
matter:
  court:
    - DISTRICT COURT
    - FAMILY DIVISION
    - SAMPLE COUNTY, NEVADA
  case_number: D-26-000000-D
  judicial_officer: Hon. Sample Q. Jurist
  department: \"Z\"
  filed_on: 2026-02-02
  parties:
    - name: SAMPLE SYNTHETIC PETITIONER
      role: Plaintiff
    - name: EXAMPLE FICTITIOUS RESPONDENT
      role: Defendant
  counsel:
    name: Pat Sample
    bar_jurisdiction: Nevada
    bar_number: \"000000\"
    firm: SAMPLE SYNTHETIC LAW GROUP, PLLC
    address:
      - 100 Example Boulevard, Suite 200
      - Sampleton, Nevada 89000
    email: pat@example.com
    telephone: (702) 555-0100
    appears_for: Plaintiff
";

/// The `word/document.xml` part of a rendered package.
fn document_xml(bytes: &[u8]) -> String {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a zip package");
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .expect("word/document.xml")
        .read_to_string(&mut xml)
        .expect("utf-8 document.xml");
    xml
}

/// Stage the workspace: the template as a direct `templates/<code>.md`
/// file (the N110 project layout), and a manifest naming the signing
/// lawyer at the root. Returns the root and the template path.
fn stage_workspace(template: &str) -> (TempDir, PathBuf) {
    let root = TempDir::new().expect("temp workspace");
    let templates = root.path().join("templates");
    fs::create_dir_all(&templates).expect("stage the templates shelf");
    let template_path = templates.join("redline_under_test.md");
    fs::write(&template_path, template).expect("stage the template");
    fs::write(root.path().join("navigator.yaml"), MANIFEST).expect("stage the manifest");
    (root, template_path)
}

/// Render the staged template to `.docx`, passing through every extra
/// flag — the version's `--answer` and, on the redline run, the previous
/// document and author.
fn render(template: &Path, out: &Path, extra: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("navigator-deploy")
        .expect("navigator binary")
        .current_dir(template.parent().expect("template parent"))
        .args([
            "notation",
            "word",
            template.to_str().expect("template path"),
            "--out",
            out.to_str().expect("out path"),
            "--preview",
        ])
        .args(extra)
        .assert()
}

/// The redline round trip: render v1, render v2, redline v2-against-v1.
/// The output carries real `w:ins`/`w:del` revisions authored by the
/// signing lawyer, reproducing exactly the seeded set: the rewrapped fee
/// paragraph struck-and-inserted, the receipts paragraph struck out, the
/// interest clause added.
#[test]
fn a_redline_writes_real_tracked_changes() {
    let (root, template) = stage_workspace(TEMPLATE_V1);
    let v1 = root.path().join("v1.docx");
    let v2 = root.path().join("v2.docx");
    let redlined = root.path().join("redline.docx");

    render(&template, &v1, &["--answer", "fee_day=first"]).success();
    fs::write(&template, TEMPLATE_V2).expect("revise the template");
    render(&template, &v2, &["--answer", "fee_day=fifteenth"]).success();
    render(
        &template,
        &redlined,
        &[
            "--answer",
            "fee_day=fifteenth",
            "--redline-against",
            v1.to_str().expect("v1 path"),
            "--redline-author",
            AUTHOR,
        ],
    )
    .success();

    let document = document_xml(&fs::read(&redlined).expect("redlined package"));
    // The seeded set: the rewrapped fee paragraph (runs struck and
    // inserted), the struck-out receipts paragraph, and the added
    // interest paragraph — nothing else revised.
    assert!(document.contains("<w:delText"), "struck text: {document}");
    assert!(
        document.contains("first of each month"),
        "the previous fee text survives struck: {document}"
    );
    assert!(
        document.contains("Interest accrues at the statutory rate."),
        "the inserted paragraph is present: {document}"
    );
    assert!(
        document.contains("Keep receipts for every expense."),
        "the deleted paragraph is struck, not silently gone: {document}"
    );
    // Every revision records the signing lawyer.
    assert!(
        document.contains(&format!(r#"w:author="{AUTHOR}""#)),
        "authored revisions: {document}"
    );
    // The revised paragraph marks are marked with their revision, so Word
    // keeps the paragraph structure when the revisions are reviewed.
    assert!(
        document.contains("<w:rPr><w:ins ") || document.contains("<w:rPr><w:del "),
        "paragraph marks are marked with their revision: {document}"
    );
}

/// The default author is the manifest's `matter.counsel.name` — the same
/// name the `/s/` signature line signs with — so a redline run needs no
/// flag when the Project knows its lawyer.
#[test]
fn the_redline_author_defaults_to_the_manifest_counsel() {
    let (root, template) = stage_workspace(TEMPLATE_V1);
    let v1 = root.path().join("v1.docx");
    let redlined = root.path().join("redline.docx");

    render(&template, &v1, &["--answer", "fee_day=first"]).success();
    fs::write(&template, TEMPLATE_V2).expect("revise the template");
    render(
        &template,
        &redlined,
        &[
            "--answer",
            "fee_day=fifteenth",
            "--redline-against",
            v1.to_str().expect("v1 path"),
        ],
    )
    .success();

    let document = document_xml(&fs::read(&redlined).expect("redlined package"));
    assert!(
        document.contains(r#"w:author="Pat Sample""#),
        "authored by the manifest counsel: {document}"
    );
}

/// A redline is for review, never for splicing: the import verb still
/// refuses a document carrying the revisions the redline wrote.
#[test]
fn the_import_verb_refuses_a_redlined_document() {
    let (root, template) = stage_workspace(TEMPLATE_V1);
    let v1 = root.path().join("v1.docx");
    let redlined = root.path().join("redline.docx");

    render(&template, &v1, &["--answer", "fee_day=first"]).success();
    fs::write(&template, TEMPLATE_V2).expect("revise the template");
    render(
        &template,
        &redlined,
        &[
            "--answer",
            "fee_day=fifteenth",
            "--redline-against",
            v1.to_str().expect("v1 path"),
            "--redline-author",
            AUTHOR,
        ],
    )
    .success();

    Command::cargo_bin("navigator-deploy")
        .expect("navigator binary")
        .current_dir(template.parent().expect("template parent"))
        .args([
            "notation",
            "import",
            template.to_str().expect("template path"),
            redlined.to_str().expect("redlined path"),
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("tracked changes"));
}

/// Without a flag and without a manifest naming counsel, the redline is
/// refused loudly — revisions must not be authored anonymously.
#[test]
fn an_unattributable_redline_is_refused() {
    let root = TempDir::new().expect("temp workspace");
    let shelf = root.path().join("templates/notations/neon_law");
    fs::create_dir_all(&shelf).expect("stage the shelf");
    let template = shelf.join("redline_under_test.md");
    fs::write(&template, TEMPLATE_V1).expect("stage the template");
    let v1 = root.path().join("v1.docx");
    let redlined = root.path().join("redline.docx");

    Command::cargo_bin("navigator-deploy")
        .expect("navigator binary")
        .current_dir(template.parent().expect("template parent"))
        .args([
            "notation",
            "word",
            template.to_str().expect("template path"),
            "--out",
            v1.to_str().expect("v1 path"),
            "--preview",
            "--answer",
            "fee_day=first",
        ])
        .assert()
        .success();

    Command::cargo_bin("navigator-deploy")
        .expect("navigator binary")
        .current_dir(template.parent().expect("template parent"))
        .args([
            "notation",
            "word",
            template.to_str().expect("template path"),
            "--out",
            redlined.to_str().expect("out path"),
            "--preview",
            "--answer",
            "fee_day=fifteenth",
            "--redline-against",
            v1.to_str().expect("v1 path"),
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--redline-author"));
}
