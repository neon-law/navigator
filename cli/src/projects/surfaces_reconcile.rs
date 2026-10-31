//! `navigator projects surfaces reconcile` — the idempotent repository
//! backfill.
//!
//! Matters opened while the forge was down, and rows recorded before the
//! Forgejo governance pass existed, carry no repository or an un-governed
//! one. This command sweeps every Project visible to this login (or one
//! `--project` code) through the same admin reconcile door
//! `navigator project setup` uses — a thin HTTP client, exactly like
//! [`super::surfaces`]: the site's own admin-tier check and the server's
//! provider credentials run the reconcile, and this process never opens a
//! store connection or holds a Firm token. The pass also reconciles the
//! Drive ingest folder; the table reports the repository surface, which is
//! what a backfill run is for.
//!
//! Each Project is reconciled and reported independently: a Firm whose
//! Forgejo is unconfigured is a `skipped` row naming the missing
//! coordinate, never a failed run — the sweep continues.

use std::fmt::Write as _;
use std::process::ExitCode;

use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::projects::surfaces::{self, ReconcileDoorError, VisibleProject};
use crate::remote;
use store::project_surfaces::{ProjectSurfaces, SurfaceStatus};

/// What one Project's reconcile pass produced.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ReconcileOutcome {
    project_code: String,
    /// `created`, `adopted`, `present`, `skipped`, or `failed` — the
    /// repository surface's outcome this pass saw.
    outcome: String,
    /// The recorded repository URL, when one is recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    repository_url: Option<String>,
    /// The named reason for a skip or a failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// One Project's door result, distilled into a report row. Pure: the same
/// result always renders the same row, in tests and in the table.
fn reconcile_outcome(
    project: &VisibleProject,
    result: Result<ProjectSurfaces, ReconcileDoorError>,
) -> ReconcileOutcome {
    let base = ReconcileOutcome {
        project_code: project.code.clone(),
        outcome: String::new(),
        repository_url: None,
        detail: None,
    };
    match result {
        Ok(surfaces) => match surfaces.repository_status {
            SurfaceStatus::Created => ReconcileOutcome {
                outcome: "created".to_string(),
                repository_url: surfaces.repository_url,
                ..base
            },
            SurfaceStatus::Adopted => ReconcileOutcome {
                outcome: "adopted".to_string(),
                repository_url: surfaces.repository_url,
                ..base
            },
            SurfaceStatus::Present => ReconcileOutcome {
                outcome: "present".to_string(),
                repository_url: surfaces.repository_url,
                ..base
            },
            SurfaceStatus::Skipped => ReconcileOutcome {
                outcome: "skipped".to_string(),
                detail: Some(
                    surfaces
                        .repository_detail
                        .unwrap_or_else(|| "repository provider is not configured".to_string()),
                ),
                ..base
            },
        },
        Err(ReconcileDoorError::Unavailable(reason)) => ReconcileOutcome {
            outcome: "skipped".to_string(),
            detail: Some(reason),
            ..base
        },
        Err(ReconcileDoorError::Failed(reason)) => ReconcileOutcome {
            outcome: "failed".to_string(),
            detail: Some(reason),
            ..base
        },
    }
}

/// Reconcile every target in order. A door failure is one outcome, never an
/// abort — the run's whole job is to keep going when one Firm is
/// unconfigured or one door call faults.
async fn reconcile_all(
    base: &str,
    token: &str,
    targets: &[VisibleProject],
) -> Vec<ReconcileOutcome> {
    let mut outcomes = Vec::new();
    for project in targets {
        let result = surfaces::post_reconcile_classified(base, token, project.id).await;
        outcomes.push(reconcile_outcome(project, result));
    }
    outcomes
}

/// Resolve the sweep's targets: every Project visible to this login, or the
/// one visible Project whose code matches. The reported code is always the
/// server's own spelling, never the CLI argument echoed back.
async fn projects_to_reconcile(
    base: &str,
    token: &str,
    project_code: Option<&str>,
) -> Result<Vec<VisibleProject>> {
    let visible = surfaces::list_visible_projects(base, token).await?;
    match project_code {
        None => Ok(visible),
        Some(code) => visible
            .into_iter()
            .find(|project| project.code == code)
            .map(|project| vec![project])
            .ok_or_else(|| anyhow!("no visible matter with that code")),
    }
}

/// The sweep succeeds when nothing failed. A skip — an unconfigured Firm, a
/// provider the deployment does not use — is the sweep working as designed,
/// not a failure.
fn exit_code_for(outcomes: &[ReconcileOutcome]) -> ExitCode {
    if outcomes.iter().any(|outcome| outcome.outcome == "failed") {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The per-Project outcome table. The third column carries the repository
/// URL when one is recorded, else the reason a Project was skipped or the
/// door failed.
fn render_report(outcomes: &[ReconcileOutcome]) -> String {
    let width = outcomes
        .iter()
        .map(|outcome| outcome.project_code.len())
        .max()
        .unwrap_or(0)
        .max("project".len());
    let mut report = String::new();
    let _ = writeln!(
        report,
        "{:<width$}  {:<8}  repository",
        "project",
        "outcome",
        width = width
    );
    for outcome in outcomes {
        let third = match (&outcome.repository_url, &outcome.detail) {
            (Some(url), _) => url.clone(),
            (None, Some(detail)) => detail.clone(),
            (None, None) => String::new(),
        };
        let _ = writeln!(
            report,
            "{:<width$}  {:<8}  {}",
            outcome.project_code,
            outcome.outcome,
            third,
            width = width
        );
    }
    report
}

/// `navigator projects surfaces reconcile [--project code]`.
pub async fn run(host: Option<&str>, project_code: Option<&str>, json: bool) -> ExitCode {
    let (base, token) = match remote::resolve(host) {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            return ExitCode::from(2);
        }
    };
    let targets = match projects_to_reconcile(&base, &token, project_code).await {
        Ok(targets) => targets,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            return ExitCode::from(2);
        }
    };
    if targets.is_empty() {
        println!("no Projects are visible to this login");
        return ExitCode::SUCCESS;
    }
    let outcomes = reconcile_all(&base, &token, &targets).await;
    if json {
        println!(
            "{}",
            serde_json::to_string(&outcomes).expect("reconcile outcomes are serializable")
        );
    } else {
        print!("{}", render_report(&outcomes));
    }
    exit_code_for(&outcomes)
}

#[cfg(test)]
mod tests {
    use super::exit_code_for;
    use super::projects_to_reconcile;
    use super::reconcile_all;
    use super::reconcile_outcome;
    use super::render_report;
    use super::ReconcileOutcome;
    use std::process::ExitCode;
    use store::project_surfaces::ProjectSurfaces;
    use uuid::Uuid;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TOKEN: &str = "a-token";

    fn visible(id: Uuid, code: &str) -> super::VisibleProject {
        super::VisibleProject {
            id,
            code: code.to_string(),
        }
    }

    fn door(id: Uuid, status: u16, body: serde_json::Value) -> Mock {
        Mock::given(method("POST"))
            .and(path(format!("/app/api/project-surfaces/{id}")))
            .and(header("authorization", format!("Bearer {TOKEN}")))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
    }

    fn surfaces_body(status: &str, url: &str) -> serde_json::Value {
        serde_json::json!({
            "code": "acme",
            "documents_prefix": "projects/acme/documents",
            "drive_folder_id": "folder-1",
            "drive_status": "present",
            "repository_url": url,
            "repository_status": status,
        })
    }

    /// Two consecutive sweeps through the doors: the second reports
    /// `present` everywhere, carries the same URL each run, and posts to
    /// each Project's door exactly once per run — the sweep invents no
    /// second repository and never moves a recorded URL.
    #[tokio::test]
    async fn a_second_sweep_reports_present_everywhere_without_duplicates() {
        let server = MockServer::start().await;
        let id = Uuid::now_v7();
        let url = "https://forge.example/an-organization/acme";
        // The first sweep creates; the second finds the same repository
        // recorded and reports `present`. One mock per pass, so the mock
        // itself counts the door calls.
        door(id, 200, surfaces_body("created", url))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        door(id, 200, surfaces_body("present", url))
            .mount(&server)
            .await;
        let targets = vec![visible(id, "acme")];

        let first = reconcile_all(&server.uri(), TOKEN, &targets).await;
        let second = reconcile_all(&server.uri(), TOKEN, &targets).await;

        assert_eq!(first[0].outcome, "created");
        assert_eq!(first[0].repository_url.as_deref(), Some(url));
        assert_eq!(second[0].outcome, "present", "the second sweep adopts");
        assert_eq!(
            second[0].repository_url, first[0].repository_url,
            "the URL never moves between runs"
        );
        let posts = server
            .received_requests()
            .await
            .expect("received requests")
            .len();
        assert_eq!(posts, 2, "exactly one door call per sweep");
        assert_eq!(exit_code_for(&second), ExitCode::SUCCESS);
    }

    /// A Project whose Firm has no Forgejo coordinates arrives as the
    /// door's typed `422 forgejo_unavailable` and becomes a `skipped` row
    /// naming the missing piece — and the sweep continues to the next
    /// Project instead of aborting on the first unconfigured Firm.
    #[tokio::test]
    async fn an_unconfigured_firm_is_skipped_with_the_missing_piece_named_and_the_sweep_continues()
    {
        let server = MockServer::start().await;
        let unconfigured = Uuid::now_v7();
        let configured = Uuid::now_v7();
        door(
            unconfigured,
            422,
            serde_json::json!({
                "error": "forgejo_unavailable",
                "message": "Forgejo provisioning needs NAVIGATOR_FORGEJO_ROOT_URL_ENV, the Firm's forgejo_access_token integration secret",
            }),
        )
        .mount(&server)
        .await;
        door(
            configured,
            200,
            surfaces_body("created", "https://forge.example/an-organization/beta"),
        )
        .mount(&server)
        .await;
        let targets = vec![visible(unconfigured, "acme"), visible(configured, "beta")];

        let outcomes = reconcile_all(&server.uri(), TOKEN, &targets).await;

        assert_eq!(outcomes[0].outcome, "skipped");
        assert!(outcomes[0]
            .detail
            .as_deref()
            .expect("the skip names its reason")
            .contains("forgejo_access_token"));
        assert!(outcomes[0].repository_url.is_none());
        assert_eq!(outcomes[1].outcome, "created", "the sweep continues");
        assert_eq!(exit_code_for(&outcomes), ExitCode::SUCCESS);
    }

    /// Any other door fault is a `failed` row, still without aborting the
    /// sweep, and fails the run's exit code.
    #[tokio::test]
    async fn a_failed_door_is_one_failed_row_and_the_sweep_continues() {
        let server = MockServer::start().await;
        let broken = Uuid::now_v7();
        let healthy = Uuid::now_v7();
        door(broken, 500, serde_json::json!({ "error": "internal" }))
            .mount(&server)
            .await;
        door(
            healthy,
            200,
            surfaces_body("present", "https://forge.example/an-organization/beta"),
        )
        .mount(&server)
        .await;
        let targets = vec![visible(broken, "acme"), visible(healthy, "beta")];

        let outcomes = reconcile_all(&server.uri(), TOKEN, &targets).await;

        assert_eq!(outcomes[0].outcome, "failed");
        assert!(outcomes[0]
            .detail
            .as_deref()
            .expect("the failure names itself")
            .contains("500"));
        assert_eq!(outcomes[1].outcome, "present", "the sweep continues");
        assert_eq!(exit_code_for(&outcomes), ExitCode::FAILURE);
    }

    /// A deployment without any forge configured answers `200` with the
    /// surface `skipped` and a reason — the same skip shape as an
    /// unconfigured Firm, from the response body rather than the status.
    #[tokio::test]
    async fn a_provider_free_deployment_reports_skipped_with_the_reason() {
        let server = MockServer::start().await;
        let id = Uuid::now_v7();
        door(
            id,
            200,
            serde_json::json!({
                "code": "acme",
                "documents_prefix": "projects/acme/documents",
                "drive_folder_id": null,
                "drive_status": "skipped",
                "drive_detail": "Drive provider is not configured",
                "repository_url": null,
                "repository_status": "skipped",
                "repository_detail": "repository provider is not configured",
            }),
        )
        .mount(&server)
        .await;
        let targets = vec![visible(id, "acme")];

        let outcomes = reconcile_all(&server.uri(), TOKEN, &targets).await;

        assert_eq!(outcomes[0].outcome, "skipped");
        assert_eq!(
            outcomes[0].detail.as_deref(),
            Some("repository provider is not configured")
        );
        assert_eq!(exit_code_for(&outcomes), ExitCode::SUCCESS);
    }

    #[test]
    fn the_report_is_a_per_project_table() {
        let outcomes = vec![
            ReconcileOutcome {
                project_code: "acme".to_string(),
                outcome: "created".to_string(),
                repository_url: Some("https://forge.example/an-organization/acme".to_string()),
                detail: None,
            },
            ReconcileOutcome {
                project_code: "a-longer-code".to_string(),
                outcome: "skipped".to_string(),
                repository_url: None,
                detail: Some(
                    "Forgejo provisioning needs the Firm's forgejo_access_token integration secret"
                        .to_string(),
                ),
            },
        ];

        let report = render_report(&outcomes);

        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(lines.len(), 3, "{report}");
        assert!(lines[0].starts_with("project"), "{report}");
        assert!(
            lines[1].contains("acme") && lines[1].contains("created"),
            "{report}"
        );
        assert!(
            lines[2].contains("a-longer-code")
                && lines[2].contains("skipped")
                && lines[2].contains("forgejo_access_token"),
            "{report}"
        );
    }

    #[tokio::test]
    async fn one_project_code_resolves_to_its_own_door_call() {
        let server = MockServer::start().await;
        let acme = Uuid::now_v7();
        let beta = Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .and(header("authorization", format!("Bearer {TOKEN}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": acme.to_string(), "code": "acme"},
                {"id": beta.to_string(), "code": "beta"},
            ])))
            .mount(&server)
            .await;

        let all = projects_to_reconcile(&server.uri(), TOKEN, None)
            .await
            .expect("every visible Project");
        assert_eq!(all.len(), 2);

        let one = projects_to_reconcile(&server.uri(), TOKEN, Some("beta"))
            .await
            .expect("the named Project");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].id, beta);
        assert_eq!(one[0].code, "beta");

        let error = projects_to_reconcile(&server.uri(), TOKEN, Some("gamma"))
            .await
            .expect_err("an absent code is an error");
        assert!(
            format!("{error:#}").contains("no visible matter"),
            "{error:#}"
        );
    }

    /// The outcome row is distilled from the result, so the pure distiller
    /// decides every row the sweep can print — including the JSON spelling.
    #[test]
    fn outcomes_render_their_json_shape() {
        let id = Uuid::now_v7();
        let project = visible(id, "acme");
        let created: ProjectSurfaces = serde_json::from_str(
            &serde_json::to_string(&surfaces_body(
                "created",
                "https://forge.example/an-organization/acme",
            ))
            .expect("fixture json"),
        )
        .expect("fixture surfaces");
        let created = reconcile_outcome(&project, Ok(created));
        let json = serde_json::to_string(&created).expect("outcome is serializable");
        assert!(json.contains("\"outcome\":\"created\""), "{json}");
        assert!(json.contains("\"repository_url\""), "{json}");
        assert!(!json.contains("detail"), "{json}");

        let skipped = reconcile_outcome(
            &project,
            Err(super::surfaces::ReconcileDoorError::Unavailable(
                "Forgejo provisioning needs NAVIGATOR_FORGEJO_ROOT_URL_ENV".to_string(),
            )),
        );
        assert_eq!(skipped.outcome, "skipped");
        assert!(skipped.detail.is_some());
    }
}
