//! `navigator notation import` — apply the lawyer's edited Word document
//! back into the notation template it was rendered from.
//!
//! The edited `.docx` carries the export's provenance record (template path
//! and template hash). The planner verifies the hash before planning, so an
//! import against a template that changed since the export refuses rather
//! than splicing edits into the wrong lines. Placeholders are protected: an
//! edit may rewrite prose around a `{{code}}` or `{{> partial}}` span but
//! may not drop one the template needs or invent one the template never
//! had. `--dry-run` prints the plan and the resulting template without
//! writing anything — the word-editing flow's read-then-approve step.

use std::path::Path;
use std::process::ExitCode;

/// One import command: plan, report, and — without `--dry-run` — write.
pub fn run(template: &Path, edited: &Path, dry_run: bool) -> ExitCode {
    let contents = match std::fs::read(template) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!(
                "navigator: could not read the template file: {}",
                error.kind()
            );
            return ExitCode::from(2);
        }
    };
    let Ok(template_text) = String::from_utf8(contents.clone()) else {
        eprintln!("navigator: the template is not valid UTF-8");
        return ExitCode::from(2);
    };
    let docx = match std::fs::read(edited) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!(
                "navigator: could not read the edited Word file: {}",
                error.kind()
            );
            return ExitCode::from(2);
        }
    };

    let (plan, applied) = match word::dry_run_import(&template_text, &contents, &docx) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("navigator: import refused: {error}");
            return ExitCode::from(2);
        }
    };

    println!("Import plan for {}:", plan.template_path);
    if plan.hunks.is_empty() {
        println!("  no changes — the template is already current");
    }
    for hunk in &plan.hunks {
        println!("  {}", hunk.summary());
    }
    if dry_run {
        println!("\nDry run — the updated template, not written:");
        println!("{applied}");
        return ExitCode::SUCCESS;
    }

    if let Err(error) = std::fs::write(template, applied) {
        eprintln!("navigator: could not write the updated template: {error}");
        return ExitCode::FAILURE;
    }
    // Do not echo the CLI path here: the parsed command tree carries the
    // Secrets subcommand, so anything read from it is treated as sensitive.
    // The plan header above already names the template via its provenance.
    println!(
        "\nImported: {} hunk{} applied to the notation template.",
        plan.hunks.len(),
        if plan.hunks.len() == 1 { "" } else { "s" }
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    const TEMPLATE: &str = "# Service Agreement\n\nThis agreement is between {{client_name}} and the Firm.\n\nFees are due on the first of each month.\n";
    const EDITED: &str = "# Service Agreement\n\nThis agreement is between {{client_name}} and the Firm.\n\nFees are due on the tenth of each month.\n";

    /// An "edited" Word file for the CLI layer: the same Word render the
    /// export produces, built from text a lawyer changed, with provenance
    /// claiming the original template's bytes. The word crate's own tests
    /// cover the planner; here the render stands in for the lawyer's edit.
    fn edited_docx(original_template: &str) -> Vec<u8> {
        word::render_notation(
            EDITED,
            None,
            &word::RenderOptions {
                provenance: Some(word::Provenance {
                    template_path: "service.md",
                    template_hash: &word::template_hash(original_template.as_bytes()),
                }),
                ..word::RenderOptions::default()
            },
        )
        .unwrap()
    }

    fn work_dir() -> (TempDir, PathBuf, PathBuf) {
        let dir = TempDir::new().unwrap();
        let template = dir.path().join("service.md");
        fs::write(&template, TEMPLATE).unwrap();
        let docx_path = dir.path().join("service.docx");
        fs::write(&docx_path, edited_docx(TEMPLATE)).unwrap();
        (dir, template, docx_path)
    }

    #[test]
    fn a_dry_run_reports_the_plan_and_leaves_the_template_alone() {
        let (_dir, template, docx_path) = work_dir();
        assert_eq!(run(&template, &docx_path, true), ExitCode::SUCCESS);
        assert_eq!(fs::read_to_string(&template).unwrap(), TEMPLATE);
    }

    #[test]
    fn an_apply_rewrites_the_template_with_the_edited_paragraph() {
        let (_dir, template, docx_path) = work_dir();
        assert_eq!(run(&template, &docx_path, false), ExitCode::SUCCESS);
        let applied = fs::read_to_string(&template).unwrap();
        assert!(applied.contains("Fees are due on the tenth of each month."));
        assert!(!applied.contains("Fees are due on the first"));
        // Placeholders survive the lawyer's edit untouched.
        assert!(applied.contains("{{client_name}}"));
    }

    #[test]
    fn a_template_that_changed_since_export_is_refused_in_place() {
        let (_dir, template, docx_path) = work_dir();
        fs::write(&template, TEMPLATE.replace("the Firm", "the Practice")).unwrap();
        let tampered = fs::read_to_string(&template).unwrap();
        assert_eq!(run(&template, &docx_path, false), ExitCode::from(2));
        assert_eq!(fs::read_to_string(&template).unwrap(), tampered);
    }

    #[test]
    fn a_missing_template_file_is_a_usage_error() {
        let dir = TempDir::new().unwrap();
        let docx_path = dir.path().join("service.docx");
        fs::write(&docx_path, edited_docx(TEMPLATE)).unwrap();
        assert_eq!(
            run(&dir.path().join("absent.md"), &docx_path, false),
            ExitCode::from(2)
        );
    }

    #[test]
    fn a_non_utf8_template_is_refused() {
        let dir = TempDir::new().unwrap();
        let template = dir.path().join("service.md");
        fs::write(&template, [0xff, 0xfe]).unwrap();
        let docx_path = dir.path().join("service.docx");
        fs::write(&docx_path, edited_docx(TEMPLATE)).unwrap();
        assert_eq!(run(&template, &docx_path, false), ExitCode::from(2));
    }

    #[test]
    fn an_unedited_render_reports_no_changes_and_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let template = dir.path().join("service.md");
        fs::write(&template, TEMPLATE).unwrap();
        // The lawyer exported and made no edit: the render's text is the
        // template's own, and the provenance hash still matches the bytes.
        let docx_path = dir.path().join("service.docx");
        let docx = word::render_notation(
            TEMPLATE,
            None,
            &word::RenderOptions {
                provenance: Some(word::Provenance {
                    template_path: "service.md",
                    template_hash: &word::template_hash(TEMPLATE.as_bytes()),
                }),
                ..word::RenderOptions::default()
            },
        )
        .unwrap();
        fs::write(&docx_path, docx).unwrap();
        assert_eq!(run(&template, &docx_path, false), ExitCode::SUCCESS);
        assert_eq!(fs::read_to_string(&template).unwrap(), TEMPLATE);
    }
}
