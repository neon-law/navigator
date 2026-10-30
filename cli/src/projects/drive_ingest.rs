//! `navigator project drive-ingest <code>`: file a Project's Drive dropbox
//! folder into the matter through the portal's drive-import door.
//!
//! The door downloads every direct child of the recorded folder, verifies
//! each download against Drive's size and MD5 receipt, screens the bytes for
//! malware, and files the survivors as Project documents named by their
//! original Drive filenames. The CLI always sends `visibility=internal`:
//! client-visible documents are shared deliberately, not swept in with the
//! folder. `--dry-run` reports exactly what a real pass would file, leave
//! unchanged, and refuse, and writes nothing.
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// One file the door filed.
#[derive(Debug, Deserialize)]
struct ImportedFile {
    filename: String,
    matter_folder: String,
    changed: bool,
}

/// One file the door refused, and why.
#[derive(Debug, Deserialize)]
struct RefusedFile {
    drive_file_id: String,
    reason: String,
}

/// The door's JSON report.
#[derive(Debug, Default, Deserialize)]
struct DriveImportReport {
    #[serde(default)]
    imported: Vec<ImportedFile>,
    #[serde(default)]
    refused: Vec<RefusedFile>,
}

/// `navigator project drive-ingest <code> [--dry-run]`.
pub async fn run(host: Option<&str>, code: &str, dry_run: bool) -> ExitCode {
    match run_inner(host, code, dry_run).await {
        Ok(exit) => exit,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::from(2)
        }
    }
}

async fn run_inner(host: Option<&str>, code: &str, dry_run: bool) -> Result<ExitCode> {
    let (base, token) = crate::remote::resolve(host)?;
    let report = import(&base, &token, code, dry_run).await?;
    report_exit(&report, dry_run)
}

/// Render the report and decide the exit: refusals fail the verb so scripts
/// notice, the way the door's own refusal statuses would.
///
/// Deliberately never echoes the caller's `code`: the `rust/cleartext-logging`
/// CodeQL query taints every clap-parsed argument through the unrelated
/// `Commands::Secrets` arm, so echoing it would trip that known false
/// positive (the same pattern [`super::skill::unresolved_message`] avoids).
fn report_exit(report: &DriveImportReport, dry_run: bool) -> Result<ExitCode> {
    if report.imported.is_empty() && report.refused.is_empty() {
        println!(
            "Nothing to import from the Project's Drive folder{}.",
            if dry_run { " (dry run)" } else { "" }
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{} the Project's Drive folder{}:",
        if dry_run { "Plan for" } else { "Imported" },
        if dry_run {
            " (dry run, nothing written)"
        } else {
            ""
        }
    );
    for file in &report.imported {
        let state = if file.changed { "filed" } else { "unchanged" };
        println!("  {state}: {} -> {}", file.filename, file.matter_folder);
    }
    for refused in &report.refused {
        println!("  refused: {} ({})", refused.drive_file_id, refused.reason);
    }
    if report.refused.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        bail!("{} file(s) were refused", report.refused.len())
    }
}

/// POST the drive-import door and decode its report.
async fn import(base: &str, token: &str, code: &str, dry_run: bool) -> Result<DriveImportReport> {
    let mut form = vec![("visibility", "internal")];
    if dry_run {
        form.push(("dry_run", "true"));
    }
    let response = reqwest::Client::new()
        .post(format!("{base}/app/projects/{code}/documents/drive-import"))
        .bearer_auth(token)
        .form(&form)
        .send()
        .await
        .context("POST drive import")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("read the drive import response")?;
    if !status.is_success() {
        bail!("drive import refused: {status}: {body}");
    }
    serde_json::from_str(&body).with_context(|| format!("parse the drive import report: {body}"))
}

#[cfg(test)]
mod tests {
    use super::{import, report_exit};
    use anyhow::Result;
    use std::process::ExitCode;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const REPORT: &str = r#"{"imported":[{"drive_file_id":"f1","filename":"notice.pdf","matter_folder":"Work Product","changed":true}],"refused":[]}"#;

    async fn door() -> MockServer {
        MockServer::start().await
    }

    async fn mount(server: &MockServer, status: u16, body: &str) {
        Mock::given(method("POST"))
            .and(path("/app/projects/matter-one/documents/drive-import"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body.to_string()))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn the_verb_files_internal_under_original_names() {
        let server = door().await;
        mount(&server, 200, REPORT).await;
        let report = import(&server.uri(), "token", "matter-one", false)
            .await
            .unwrap();
        assert_eq!(report.imported[0].filename, "notice.pdf");
        assert_eq!(report.imported[0].matter_folder, "Work Product");
        assert!(report.imported[0].changed);
        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&requests[0].body).into_owned();
        assert!(body.contains("visibility=internal"), "{body}");
        assert!(!body.contains("dry_run"), "a real pass never plans: {body}");
    }

    #[tokio::test]
    async fn a_dry_run_posts_the_flag_and_forces_internal_visibility() {
        let server = door().await;
        mount(&server, 200, r#"{"imported":[],"refused":[]}"#).await;
        import(&server.uri(), "token", "matter-one", true)
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&requests[0].body).into_owned();
        assert!(body.contains("visibility=internal"), "{body}");
        assert!(body.contains("dry_run=true"), "{body}");
    }

    #[tokio::test]
    async fn refusals_fail_the_verb_and_successes_pass() {
        let refused = super::DriveImportReport {
            imported: vec![super::ImportedFile {
                filename: "notice.pdf".into(),
                matter_folder: "Work Product".into(),
                changed: true,
            }],
            refused: vec![super::RefusedFile {
                drive_file_id: "f2".into(),
                reason: "name_collision".into(),
            }],
        };
        let error = report_exit(&refused, false).unwrap_err();
        assert!(error.to_string().contains("1 file(s) were refused"));

        let clean = super::DriveImportReport::default();
        assert_eq!(report_exit(&clean, false).unwrap(), ExitCode::SUCCESS);
    }

    #[tokio::test]
    async fn a_server_refusal_names_the_reason() -> Result<()> {
        let server = door().await;
        mount(&server, 503, "Drive import stopped; run it again").await;
        let error = import(&server.uri(), "token", "matter-one", false)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Drive import stopped; run it again"),
            "{error}"
        );
        Ok(())
    }
}
