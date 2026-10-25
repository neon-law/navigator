//! A pleading's caption, counsel block, and footer come from the Project's
//! `matter:` block and the notation's own frontmatter, never from its body
//! (LAW-112). Every name, date, and case number here is synthetic.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;
use tempfile::TempDir;

const FIXTURE: &str = "tests/fixtures/synthetic_matter";

/// The synthetic matter tree copied into a tempdir, so a test can edit its
/// manifest or notations without touching the checked-in fixture.
fn matter_tree() -> TempDir {
    let work = TempDir::new().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    copy_tree(&source, work.path());
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

fn render(subcommand: &str, notation: &Path, out: &Path) -> Output {
    Command::new(cargo_bin("navigator"))
        .args(["notation", subcommand])
        .arg(notation)
        .arg("--out")
        .arg(out)
        .args([
            "--answer",
            "person__client=SAMPLE SYNTHETIC HOLDINGS OF NORTHERN CALIFORNIA",
            "--answer",
            "custom_datetime__verification_date=1 July 2026",
        ])
        .output()
        .expect("run navigator notation")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn pdf_of(work: &TempDir, template: &str) -> Vec<u8> {
    let out = work.path().join("out.pdf");
    let result = render("pdf", &work.path().join("templates").join(template), &out);
    assert!(result.status.success(), "stderr: {}", stderr(&result));
    fs::read(out).unwrap()
}

fn edit(path: PathBuf, from: &str, to: &str) {
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains(from), "{from} is in {}", path.display());
    fs::write(path, text.replacen(from, to, 1)).unwrap();
}

#[test]
fn two_pleadings_share_one_matter_with_no_caption_text_in_either_body() {
    let work = matter_tree();
    for template in ["rfa_responses.md", "notice_of_compliance.md"] {
        let body = fs::read_to_string(work.path().join("templates").join(template)).unwrap();
        assert!(!body.contains("caption\n") && !body.contains("00CV000000"));
        let bytes = pdf_of(&work, template);
        for shared in [
            "Case No. 00CV000000",
            "Dept. 12",
            "Pat Sample (SBN 000000)",
            "COUNTY OF SAMPLE",
        ] {
            assert_eq!(
                pdf::occurrence_count(&bytes, shared).unwrap(),
                1,
                "{template}: {shared}"
            );
        }
    }
}

#[test]
fn hearing_fields_print_when_declared_and_a_notation_without_them_prints_none() {
    let work = matter_tree();
    let with = pdf_of(&work, "rfa_responses.md");
    assert_eq!(pdf::occurrence_count(&with, "August 14, 2026").unwrap(), 1);
    assert_eq!(pdf::occurrence_count(&with, "8:30 a.m.").unwrap(), 1);
    assert_eq!(pdf::occurrence_count(&with, "R-0000-SAMPLE").unwrap(), 1);

    let without = pdf_of(&work, "notice_of_compliance.md");
    assert_eq!(pdf::occurrence_count(&without, "Hearing").unwrap(), 0);
    assert_eq!(pdf::occurrence_count(&without, "[").unwrap(), 0);
}

#[test]
fn a_partial_hearing_leaves_a_blank_fill_line_not_a_bracketed_placeholder() {
    let work = matter_tree();
    let template = work.path().join("templates/rfa_responses.md");
    edit(template.clone(), "hearing_time: 8:30 a.m.\n", "");
    edit(template, "reservation_id: R-0000-SAMPLE\n", "");
    let bytes = pdf_of(&work, "rfa_responses.md");
    assert_eq!(pdf::occurrence_count(&bytes, "Time: ____").unwrap(), 1);
    assert_eq!(
        pdf::occurrence_count(&bytes, "Reservation ID: ____").unwrap(),
        1
    );
    assert_eq!(pdf::occurrence_count(&bytes, "[").unwrap(), 0);
}

#[test]
fn the_footer_prints_the_short_title_from_frontmatter() {
    let work = matter_tree();
    let template = work.path().join("templates/rfa_responses.md");
    edit(
        template,
        "footer_title: PLAINTIFF'S RESPONSES TO DEFENDANT'S REQUESTS FOR ADMISSION, SET ONE",
        "footer_title: PL RFA RESPONSES, SET ONE",
    );
    let bytes = pdf_of(&work, "rfa_responses.md");
    let pages = pdf::page_count(&bytes).unwrap();
    let mut footers = std::collections::BTreeSet::new();
    for ordinal in 1..=pdf::occurrence_count(&bytes, "PL RFA RESPONSES").unwrap() {
        let hit = pdf::locate(&bytes, "PL RFA RESPONSES", ordinal).unwrap();
        footers.insert(hit.rects[0].page_index);
    }
    assert_eq!(footers, (0..pages).collect());
}

#[test]
fn changing_the_manifest_changes_every_rendered_pleading_with_no_template_edit() {
    let work = matter_tree();
    let manifest = work.path().join("navigator.yaml");
    edit(
        manifest.clone(),
        "case_number: 00CV000000",
        "case_number: 11CV111111",
    );
    edit(
        manifest,
        "100 Example Boulevard, Suite 200",
        "900 Relocated Avenue",
    );
    for template in ["rfa_responses.md", "notice_of_compliance.md"] {
        let bytes = pdf_of(&work, template);
        assert_eq!(
            pdf::occurrence_count(&bytes, "Case No. 11CV111111").unwrap(),
            1
        );
        assert_eq!(
            pdf::occurrence_count(&bytes, "900 Relocated Avenue").unwrap(),
            1
        );
        assert_eq!(pdf::occurrence_count(&bytes, "00CV000000").unwrap(), 0);
    }
}

#[test]
fn word_carries_the_same_composed_caption_and_footer() {
    let work = matter_tree();
    let out = work.path().join("out.docx");
    let template = work.path().join("templates/rfa_responses.md");
    let result = render("word", &template, &out);
    assert!(result.status.success(), "stderr: {}", stderr(&result));
    let document = docx_part(&fs::read(&out).unwrap(), "word/document.xml");
    for text in [
        "Case No. 00CV000000",
        "Pat Sample (SBN 000000)",
        "August 14, 2026",
    ] {
        assert!(document.contains(text), "{text}");
    }
    assert!(!document.contains("Parties</w:t>"), "no generic header row");
}

#[test]
fn a_missing_matter_field_is_named_and_nothing_is_written() {
    let work = matter_tree();
    edit(
        work.path().join("navigator.yaml"),
        "  case_number: 00CV000000\n",
        "",
    );
    let out = work.path().join("out.pdf");
    let result = render("pdf", &work.path().join("templates/rfa_responses.md"), &out);
    assert!(!result.status.success());
    assert!(
        stderr(&result).contains("matter.case_number"),
        "{}",
        stderr(&result)
    );
    assert!(!out.exists());
}

#[test]
fn pending_assignments_are_refused_by_final_pdf_and_word_without_writing() {
    for (field, original) in [
        ("department", "  department: \"12\""),
        (
            "judicial_officer",
            "  judicial_officer: Hon. Sample Q. Jurist",
        ),
    ] {
        let work = matter_tree();
        edit(
            work.path().join("navigator.yaml"),
            original,
            &format!("  {field}: PeNdInG"),
        );
        for format in ["pdf", "word"] {
            let out = work.path().join(if format == "pdf" {
                "final.pdf"
            } else {
                "final.docx"
            });
            let result = render(
                format,
                &work.path().join("templates/rfa_responses.md"),
                &out,
            );
            assert!(
                !result.status.success(),
                "pending {field}: {format} must refuse"
            );
            assert!(
                stderr(&result).contains(&format!("matter.{field}")),
                "{}",
                stderr(&result)
            );
            assert!(!out.exists(), "refusal must not write an artifact");
        }
    }
}

#[test]
fn explicit_pending_assignments_still_preview() {
    let work = matter_tree();
    edit(
        work.path().join("navigator.yaml"),
        "  department: \"12\"",
        "  department: pending",
    );
    edit(
        work.path().join("navigator.yaml"),
        "  judicial_officer: Hon. Sample Q. Jurist",
        "  judicial_officer: pending",
    );
    let out = work.path().join("preview.pdf");
    let result = Command::new(cargo_bin("navigator"))
        .args(["notation", "pdf"])
        .arg(work.path().join("templates/rfa_responses.md"))
        .args(["--preview", "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", stderr(&result));
    let bytes = fs::read(out).unwrap();
    assert_eq!(
        pdf::occurrence_count(&bytes, "Dept. To be assigned").unwrap(),
        1
    );
    assert_eq!(
        pdf::occurrence_count(&bytes, "Judge: To be assigned").unwrap(),
        1
    );
}

#[test]
fn a_missing_document_field_is_named() {
    let work = matter_tree();
    edit(
        work.path().join("templates/rfa_responses.md"),
        "footer_title: PLAINTIFF'S RESPONSES TO DEFENDANT'S REQUESTS FOR ADMISSION, SET ONE\n",
        "",
    );
    let out = work.path().join("out.pdf");
    let result = render("pdf", &work.path().join("templates/rfa_responses.md"), &out);
    assert!(!result.status.success());
    assert!(
        stderr(&result).contains("footer_title"),
        "{}",
        stderr(&result)
    );
}

#[test]
fn a_pleading_without_a_matter_block_is_refused_by_name() {
    let work = matter_tree();
    let manifest = work.path().join("navigator.yaml");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(&manifest, &text[..text.find("matter:").unwrap()]).unwrap();
    let out = work.path().join("out.pdf");
    let result = render("pdf", &work.path().join("templates/rfa_responses.md"), &out);
    assert!(!result.status.success());
    assert!(stderr(&result).contains("matter"), "{}", stderr(&result));
}

#[test]
fn caption_markup_left_in_a_pleading_body_is_refused_with_the_migration() {
    let work = matter_tree();
    let template = work.path().join("templates/rfa_responses.md");
    edit(
        template,
        "# PLAINTIFF'S RESPONSES",
        "```caption\ncourt: SUPERIOR COURT\n```\n\n# PLAINTIFF'S RESPONSES",
    );
    let out = work.path().join("out.pdf");
    let result = render("pdf", &work.path().join("templates/rfa_responses.md"), &out);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stdout).to_string() + &stderr(&result);
    assert!(
        message.contains("N130") && message.contains("matter:"),
        "{message}"
    );
}

#[test]
fn the_retired_scaffold_placeholders_and_party_table_are_refused() {
    let work = matter_tree();
    let template = work.path().join("templates/rfa_responses.md");
    edit(
        template,
        "# PLAINTIFF'S RESPONSES",
        "{{attorney_block}}\n\n| Parties | Case information |\n| --- | --- |\n| A | B |\n\n# PLAINTIFF'S RESPONSES",
    );
    let out = work.path().join("out.pdf");
    let result = render("pdf", &work.path().join("templates/rfa_responses.md"), &out);
    assert!(!result.status.success());
    let message = String::from_utf8_lossy(&result.stdout).to_string() + &stderr(&result);
    assert!(message.contains("N130"), "{message}");
}

/// Every `kind: pleading` template the catalog ships, so a field the renderer
/// starts requiring cannot ship without the templates that carry it (ENG-975).
fn shipped_pleadings() -> Vec<PathBuf> {
    let catalog = Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates/notations/neon_law");
    let mut found: Vec<PathBuf> = fs::read_dir(&catalog)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .filter(|path| {
            fs::read_to_string(path)
                .unwrap()
                .lines()
                .any(|line| line.trim_end() == "kind: pleading")
        })
        .collect();
    found.sort();
    found
}

#[test]
fn every_shipped_pleading_renders_against_a_fixture_matter() {
    let shipped = shipped_pleadings();
    assert!(!shipped.is_empty(), "the catalog ships pleadings");
    let work = matter_tree();
    let mut failures = Vec::new();
    for source in shipped {
        let name = source.file_name().unwrap().to_owned();
        let template = work.path().join("templates").join(&name);
        fs::copy(&source, &template).unwrap();
        let text = fs::read_to_string(&source).unwrap();
        let mut args = vec![
            "--answer".to_string(),
            "person__client=SAMPLE SYNTHETIC HOLDINGS".to_string(),
        ];
        for key in text
            .lines()
            .filter_map(|line| line.trim().strip_suffix(':'))
            .filter(|key| key.starts_with("custom_datetime__"))
        {
            args.extend(["--answer".to_string(), format!("{key}=1 July 2026")]);
        }
        let out = work.path().join("out.pdf");
        let result = Command::new(cargo_bin("navigator"))
            .args(["notation", "pdf"])
            .arg(&template)
            .arg("--out")
            .arg(&out)
            .args(&args)
            .output()
            .expect("run navigator notation pdf");
        if !result.status.success() {
            failures.push(format!("{}: {}", name.to_string_lossy(), stderr(&result)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
