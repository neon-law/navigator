//! LAW-124: a pleading's certificate of service is composed from the Service
//! records on the served document's committed pointer, in both renders;
//! with no record a preview omits it and a final render refuses by name.
//! Every name, date, and address here is synthetic.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

const FIXTURE: &str = "tests/fixtures/synthetic_matter";
const SLUG: &str = "pleadings/notice-of-compliance.pdf";

fn matter_tree() -> TempDir {
    let work = TempDir::new().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    copy_tree(&source, work.path());
    let template = work.path().join("templates/notice_of_compliance.md");
    let text = fs::read_to_string(&template).unwrap();
    fs::write(
        template,
        text.replacen(
            "footer_title:",
            &format!("served_document: {SLUG}\nfooter_title:"),
            1,
        ),
    )
    .unwrap();
    work
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn pointer(work: &TempDir, service: &str) -> PathBuf {
    let path = work.path().join("blobs").join(format!("{SLUG}.yaml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        format!(
            "kind: pleading\nvisibility: internal\ncurrent_version:\n  version: 1\n  \
             asset_id: 018f3b1a-0000-7000-8000-000000000000\n  created_at: 2026-03-05T12:00:00Z\n  \
             sha256: {}\n  size_bytes: 42\n{service}",
            "0".repeat(64)
        ),
    )
    .unwrap();
    path
}

const TWO_RECORDS: &str = "service:\n\
- recipient: Example Counsel\n  counsel_for: Defendant Example Co.\n  method: mail\n  \
served_on: 2026-03-05\n  address: 200 Sample Street, Sampleton, California 90000\n  declarant: Pat Sample\n\
- recipient: Other Counsel\n  method: e_service\n  served_on: 2026-03-06\n  \
address: other@example.invalid\n  declarant: Pat Sample\n";

fn render(subcommand: &str, work: &TempDir, out: &Path, preview: bool) -> Output {
    let mut command = Command::new(cargo_bin("navigator"));
    command
        .args(["notation", subcommand])
        .arg(work.path().join("templates/notice_of_compliance.md"))
        .arg("--out")
        .arg(out)
        .args([
            "--answer",
            "person__client=SAMPLE SYNTHETIC HOLDINGS OF NORTHERN CALIFORNIA",
            "--answer",
            "custom_datetime__verification_date=1 July 2026",
        ]);
    if preview {
        command.arg("--preview");
    }
    command.output().expect("run navigator notation")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn docx_part(bytes: &[u8], name: &str) -> String {
    use std::io::Read as _;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("docx is a zip");
    let mut xml = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    xml
}

#[test]
fn two_recipients_by_two_methods_print_in_pdf_and_word_with_the_declarant_block() {
    let work = matter_tree();
    pointer(&work, TWO_RECORDS);

    let out = work.path().join("out.pdf");
    let result = render("pdf", &work, &out, false);
    assert!(result.status.success(), "stderr: {}", stderr(&result));
    let bytes = fs::read(&out).unwrap();
    for text in [
        "PROOF OF SERVICE",
        "Example Counsel",
        "Other Counsel",
        "by mail on",
        "March 5,",
        "by electronic service",
        "March 6, 2026",
        "penalty of perjury",
        "Executed on",
        "Pat Sample",
    ] {
        assert!(
            pdf::occurrence_count(&bytes, text).unwrap() >= 1,
            "PDF carries {text:?}"
        );
    }
    assert_eq!(pdf::occurrence_count(&bytes, "[").unwrap(), 0);

    let out = work.path().join("out.docx");
    let result = render("word", &work, &out, false);
    assert!(result.status.success(), "stderr: {}", stderr(&result));
    let document = docx_part(&fs::read(&out).unwrap(), "word/document.xml");
    for text in [
        "PROOF OF SERVICE",
        "Example Counsel",
        "Other Counsel",
        "by electronic service",
        "March 6, 2026",
        "penalty of perjury",
    ] {
        assert!(document.contains(text), "Word carries {text:?}");
    }
}

#[test]
fn with_no_records_a_preview_omits_the_certificate_and_a_final_render_refuses_by_name() {
    let work = matter_tree();
    let path = pointer(&work, "");

    let out = work.path().join("preview.pdf");
    let result = render("pdf", &work, &out, true);
    assert!(result.status.success(), "stderr: {}", stderr(&result));
    let bytes = fs::read(&out).unwrap();
    assert_eq!(
        pdf::occurrence_count(&bytes, "PROOF OF SERVICE").unwrap(),
        0
    );

    let out = work.path().join("final.pdf");
    let result = render("pdf", &work, &out, false);
    assert!(!result.status.success());
    let message = stderr(&result);
    assert!(message.contains("no service is recorded"), "{message}");
    assert!(
        message.contains(&path.display().to_string()),
        "the refusal names the pointer: {message}"
    );
    assert!(
        message.contains("navigator project service add"),
        "{message}"
    );
    assert!(!out.exists());
}

#[test]
fn a_served_document_with_no_pointer_is_refused_by_path() {
    let work = matter_tree();
    let out = work.path().join("out.pdf");
    let result = render("pdf", &work, &out, false);
    assert!(!result.status.success());
    let message = stderr(&result);
    assert!(message.contains(SLUG), "{message}");
    assert!(message.contains("navigator project sync"), "{message}");
}
