//! `navigator project setup` — converge the existing Project setup doors.
//!
//! This is deliberately an HTTP composition. The surface, Slack, and Notion
//! operations keep their own authenticated server-side authorization and
//! provider semantics; this command only joins their per-resource outcomes
//! into one operator report. A retry therefore reuses the recorded resources
//! through the same doors instead of inventing a second provisioning path.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::projects::manifest;
use crate::projects::surfaces::{self, VisibleProject};
use crate::remote::{self, IntegrationOutcome};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ResourceOutcome {
    outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ProjectSetupOutcome {
    project_code: String,
    resources: BTreeMap<String, ResourceOutcome>,
}

#[derive(Debug, Serialize)]
struct SetupReport {
    results: Vec<ProjectSetupOutcome>,
}

fn resource(outcome: impl Into<String>) -> ResourceOutcome {
    ResourceOutcome {
        outcome: outcome.into(),
        detail: None,
    }
}

fn resource_with_detail(outcome: impl Into<String>, detail: impl Into<String>) -> ResourceOutcome {
    ResourceOutcome {
        outcome: outcome.into(),
        detail: Some(detail.into()),
    }
}

fn integration_result(code: &str, result: Result<IntegrationOutcome>) -> ResourceOutcome {
    match result {
        Ok(outcome) if outcome.project_code.as_deref() == Some(code) => match outcome.detail {
            Some(detail) => resource_with_detail(outcome.outcome, detail),
            None => resource(outcome.outcome),
        },
        Ok(_) => resource("malformed_result"),
        Err(error) => resource_with_detail("request_failed", format!("{error:#}")),
    }
}

fn surface_outcome(
    status: model::project_surfaces::SurfaceStatus,
    detail: Option<String>,
) -> ResourceOutcome {
    match status {
        model::project_surfaces::SurfaceStatus::Created => resource("created"),
        model::project_surfaces::SurfaceStatus::Adopted => resource("adopted"),
        // The detail says how a recorded resource was found: for Drive, that
        // the folder already set on the Project was adopted, not created.
        model::project_surfaces::SurfaceStatus::Present => match detail {
            Some(detail) => resource_with_detail("present", detail),
            None => resource("present"),
        },
        model::project_surfaces::SurfaceStatus::Skipped => resource_with_detail(
            "skipped",
            detail.unwrap_or_else(|| "provider is not configured".to_string()),
        ),
    }
}

fn surface_results(
    result: Result<model::project_surfaces::ProjectSurfaces>,
) -> [(String, ResourceOutcome); 2] {
    match result {
        Ok(surfaces) => [
            (
                "drive".to_string(),
                surface_outcome(surfaces.drive_status, surfaces.drive_detail),
            ),
            (
                "github".to_string(),
                surface_outcome(surfaces.repository_status, surfaces.repository_detail),
            ),
        ],
        Err(error) => {
            let detail = format!("{error:#}");
            [
                (
                    "drive".to_string(),
                    resource_with_detail("request_failed", detail.clone()),
                ),
                (
                    "github".to_string(),
                    resource_with_detail("request_failed", detail),
                ),
            ]
        }
    }
}

fn succeeded(outcome: &ResourceOutcome) -> bool {
    matches!(
        outcome.outcome.as_str(),
        "created"
            | "present"
            | "adopted"
            | "unchanged"
            | "repaired"
            | "notified"
            | "read_only"
            | "synced"
            | "verified"
            | "deferred"
    )
}

fn print_report(report: &SetupReport, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(report).expect("setup report is serializable")
        );
        return;
    }
    for project in &report.results {
        for (name, outcome) in &project.resources {
            match &outcome.detail {
                Some(detail) => println!(
                    "{} {name}: {} ({detail})",
                    project.project_code, outcome.outcome
                ),
                None => println!("{} {name}: {}", project.project_code, outcome.outcome),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum WorkspaceMode {
    Finish,
    Pending,
    Deferred,
}

async fn setup_one(
    base: &str,
    token: &str,
    project: &VisibleProject,
    workspace_mode: WorkspaceMode,
) -> ProjectSetupOutcome {
    let mut resources = BTreeMap::new();
    let reconciled = surfaces::post_reconcile(base, token, project.id).await;
    let repository_attached = reconciled
        .as_ref()
        .ok()
        .and_then(|surface| surface.repository_url.as_ref())
        .is_some();
    resources.extend(surface_results(reconciled));
    // Contacts runs after the calendar so a matter's people reach each
    // member's phone once the matter is set up.
    for (name, door) in [
        ("slack", "/app/api/integrations/slack/ensure"),
        ("calendar", "/app/api/integrations/calendar/ensure"),
        ("contacts", "/app/api/integrations/contacts/sync"),
        ("notion", "/app/api/integrations/notion/ensure"),
    ] {
        resources.insert(
            name.to_string(),
            integration_result(
                &project.code,
                remote::post_integration_door(
                    base,
                    token,
                    door,
                    serde_json::json!({ "project_code": project.code }),
                )
                .await
                .and_then(|report| one_integration_result(&report.results)),
            ),
        );
    }
    if repository_attached {
        let workspace = match workspace_mode {
            WorkspaceMode::Finish => match super::workspace::finish(base, token, project.id, &project.code).await {
                Ok(true) => resource("verified"),
                Ok(false) => resource("read_only"),
                Err(error) => resource_with_detail("incomplete", format!("{error:#}")),
            },
            WorkspaceMode::Pending => resource_with_detail("pending", "run navigator project setup for this Project to verify source, branch protection, mirror, and checkout"),
            WorkspaceMode::Deferred => resource_with_detail("deferred", "run setup for this Project by code to finish its repository and checkout"),
        };
        resources.insert("workspace".to_owned(), workspace);
    }
    ProjectSetupOutcome {
        project_code: project.code.clone(),
        resources,
    }
}

fn one_integration_result(results: &[IntegrationOutcome]) -> Result<IntegrationOutcome> {
    match results {
        [result] => Ok(result.clone()),
        _ => Err(anyhow!(
            "integration door returned an unexpected result count"
        )),
    }
}

/// Finish opening a Project by running the same idempotent setup `navigator
/// project setup` runs, so a new Project gets its Drive folder, repository,
/// Slack channel, shared calendar, contacts, and Notion page without anyone
/// remembering a second command. It prints one line per resource and never
/// fails the open: a resource whose runtime is not configured reports that,
/// and the Project stays open. Returns whether every resource succeeded.
pub async fn after_create(base: &str, token: &str, id: uuid::Uuid, code: &str) -> bool {
    let project = VisibleProject {
        id,
        code: code.to_string(),
        firm_id: None,
        brand: None,
        status: None,
    };
    let report = SetupReport {
        results: vec![setup_one(base, token, &project, WorkspaceMode::Pending).await],
    };
    print_report(&report, false);
    let complete = report
        .results
        .iter()
        .flat_map(|project| project.resources.values())
        .all(succeeded);
    if !complete {
        println!(
            "The Project is open; setup is incomplete. Fix the lines above, then run `navigator project setup {code}`."
        );
    }
    complete
}

/// Resolve `--host` and the Project code against `navigator.yaml` in `dir`
/// before touching the network. A repository root already names both, the
/// same way `project gate` reads them, so a bare `navigator project setup`
/// run there needs neither flag. An explicit `--host` that disagrees with the
/// manifest is refused rather than guessed at — the two can only name
/// different deployments by mistake.
fn resolve_from_manifest(
    dir: &Path,
    host: Option<&str>,
    project_code: Option<&str>,
    all: bool,
) -> Result<(Option<String>, Option<String>)> {
    let found = manifest::read(dir);
    let manifest_host = found
        .as_ref()
        .and_then(|manifest| manifest::non_empty(manifest.host.as_deref()));
    let manifest_project = found
        .as_ref()
        .and_then(|manifest| manifest::non_empty(manifest.project.as_deref()));

    if let (Some(flag), Some(declared)) = (host, manifest_host.as_deref()) {
        if flag != declared {
            return Err(anyhow!(
                "--host {flag} disagrees with navigator.yaml's project.host {declared}; refusing to guess which deployment you mean"
            ));
        }
    }

    let resolved_host = host.map(str::to_string).or(manifest_host);
    let resolved_project = if all {
        project_code.map(str::to_string)
    } else {
        project_code.map(str::to_string).or(manifest_project)
    };
    Ok((resolved_host, resolved_project))
}

/// Run setup for one visible Project or every visible Project admitted by the
/// authenticated list. Each resource is attempted even if an earlier one
/// fails, so a later invocation can retry only the incomplete provider work.
pub async fn run(
    host: Option<&str>,
    project_code: Option<&str>,
    all: bool,
    json: bool,
) -> ExitCode {
    let (host, project_code) = match resolve_from_manifest(Path::new("."), host, project_code, all)
    {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            return ExitCode::from(2);
        }
    };
    let host = host.as_deref();
    let project_code = project_code.as_deref();
    let (base, token) = match remote::resolve(host) {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            return ExitCode::from(2);
        }
    };
    let projects = if all {
        match surfaces::list_visible_projects(&base, &token).await {
            Ok(projects) => projects,
            Err(error) => {
                eprintln!("navigator: {error:#}");
                return ExitCode::from(2);
            }
        }
    } else {
        let Some(code) = project_code else {
            eprintln!("navigator: provide a Project code or --all");
            return ExitCode::from(2);
        };
        if !model::projects::is_valid_code(code) {
            eprintln!("navigator: invalid project code");
            return ExitCode::from(2);
        }
        match surfaces::resolve_project_id(&base, &token, code).await {
            Ok(id) => vec![VisibleProject {
                firm_id: None,
                brand: None,
                status: None,
                id,
                code: code.to_string(),
            }],
            Err(error) => {
                eprintln!("navigator: {error:#}");
                return ExitCode::from(2);
            }
        }
    };
    let mut report = SetupReport {
        results: Vec::new(),
    };
    let workspace_mode = if all {
        WorkspaceMode::Deferred
    } else {
        WorkspaceMode::Finish
    };
    for project in &projects {
        report
            .results
            .push(setup_one(&base, &token, project, workspace_mode).await);
    }
    let success = report
        .results
        .iter()
        .flat_map(|project| project.resources.values())
        .all(succeeded);
    print_report(&report, json);
    if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_from_manifest, resource, succeeded, surface_results};
    use store::project_surfaces::{ProjectSurfaces, SurfaceStatus};

    fn manifest_dir(contents: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("navigator.yaml"), contents).unwrap();
        dir
    }

    #[test]
    fn no_flags_resolves_host_and_code_from_the_manifest() {
        let dir = manifest_dir("project:\n  host: www.neonlaw.com\n  name: acme\n");
        let (host, code) = resolve_from_manifest(dir.path(), None, None, false).unwrap();
        assert_eq!(host.as_deref(), Some("www.neonlaw.com"));
        assert_eq!(code.as_deref(), Some("acme"));
    }

    #[test]
    fn an_explicit_flag_overrides_a_manifest_with_no_conflict() {
        let dir = manifest_dir("project:\n  host: www.neonlaw.com\n  name: acme\n");
        let (host, code) =
            resolve_from_manifest(dir.path(), None, Some("other-code"), false).unwrap();
        assert_eq!(host.as_deref(), Some("www.neonlaw.com"));
        assert_eq!(code.as_deref(), Some("other-code"));
    }

    #[test]
    fn a_conflicting_host_flag_is_refused_rather_than_guessed() {
        let dir = manifest_dir("project:\n  host: www.neonlaw.com\n  name: acme\n");
        let error = resolve_from_manifest(dir.path(), Some("staging.neonlaw.com"), None, false)
            .unwrap_err();
        assert!(error.to_string().contains("disagrees"));
    }

    #[test]
    fn all_ignores_the_manifest_project_code() {
        let dir = manifest_dir("project:\n  host: www.neonlaw.com\n  name: acme\n");
        let (host, code) = resolve_from_manifest(dir.path(), None, None, true).unwrap();
        assert_eq!(host.as_deref(), Some("www.neonlaw.com"));
        assert_eq!(code, None);
    }

    #[test]
    fn no_manifest_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (host, code) = resolve_from_manifest(dir.path(), None, None, false).unwrap();
        assert_eq!(host, None);
        assert_eq!(code, None);
    }

    #[test]
    fn skipped_existing_surfaces_are_required_setup_failures() {
        let outcomes = surface_results(Ok(ProjectSurfaces {
            code: "acme".to_string(),
            documents_prefix: "projects/acme/documents".to_string(),
            drive_folder_id: None,
            drive_status: SurfaceStatus::Skipped,
            drive_detail: Some("Drive provider is not configured".to_string()),
            repository_url: Some("https://forge.example/acme".to_string()),
            repository_status: SurfaceStatus::Present,
            repository_detail: None,
        }));
        assert_eq!(outcomes[0].1.outcome, "skipped");
        assert_eq!(
            outcomes[0].1.detail.as_deref(),
            Some("Drive provider is not configured")
        );
        assert_eq!(outcomes[1].1.outcome, "present");
        assert!(!succeeded(&outcomes[0].1));
        assert!(succeeded(&outcomes[1].1));
    }

    /// A folder set with `--drive-folder` before setup is reported as adopted
    /// in place, never as created, and setup counts it as converged.
    #[test]
    fn a_folder_already_set_is_reported_as_adopted_not_created() {
        let outcomes = surface_results(Ok(ProjectSurfaces {
            code: "acme".to_string(),
            documents_prefix: "projects/acme/documents".to_string(),
            drive_folder_id: Some("1SyntheticFolderId".to_string()),
            drive_status: SurfaceStatus::Present,
            drive_detail: Some(store::project_surfaces::DRIVE_FOLDER_ALREADY_SET.to_string()),
            repository_url: None,
            repository_status: SurfaceStatus::Skipped,
            repository_detail: Some("repository provider is not configured".to_string()),
        }));
        assert_eq!(outcomes[0].1.outcome, "present");
        assert_eq!(
            outcomes[0].1.detail.as_deref(),
            Some(store::project_surfaces::DRIVE_FOLDER_ALREADY_SET)
        );
        assert!(succeeded(&outcomes[0].1));
    }

    #[test]
    fn unknown_provider_outcomes_fail_closed() {
        assert!(!succeeded(&resource("provider_unknown")));
    }
}
