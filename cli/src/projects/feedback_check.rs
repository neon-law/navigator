//! `navigator project feedback-check`: the Pre-Filing PII Check.
//!
//! The draft never leaves this process. Authenticated scans resolve private
//! Project, Person, and Entity terms through the deployment's scoped
//! `GET /app/api/projects/{id}/private-terms` door with the `site login`
//! session. A lawyer's machine holds a bearer token, not database coordinates.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, ensure, Result};
use serde::Deserialize;

use crate::{
    projects::{manifest, surfaces},
    remote,
};

#[derive(Deserialize)]
struct PrivateTerms {
    code: String,
    name: String,
    participants: Vec<Participant>,
    identifiers: Vec<String>,
}

#[derive(Deserialize)]
struct Participant {
    name: String,
    email: String,
    #[serde(default)]
    phone: Option<String>,
}

/// One Project's private-terms read: the terms that resolved, or the
/// refusal that stopped it, attributed to the Project's code. The code
/// comes from the caller's own firm-side participation list — never a
/// response body — so naming the matter leaks nothing the caller cannot
/// already see.
#[derive(Debug)]
pub struct ProjectOutcome {
    pub code: String,
    pub read: ProjectRead,
}

#[derive(Debug)]
pub enum ProjectRead {
    Resolved(Vec<String>),
    Refused { reason: String },
}

/// Resolve named Projects, or every visible Project when `codes` is empty.
/// Reads use stable ids so codes shared by different Firms remain distinct.
/// Unknown or ambiguous codes and incomplete reads refuse the entire check;
/// a refused per-Project read is a [`ProjectRead::Refused`] outcome —
/// collected and continued past in `--all-projects` mode, fatal for a
/// single named Project, which has nothing to continue past.
pub async fn private_terms(
    base: &str,
    token: &str,
    codes: &[String],
) -> Result<Vec<ProjectOutcome>> {
    let client = reqwest::Client::new();
    let visible = surfaces::list_visible_projects(base, token)
        .await
        .map_err(|_| {
            anyhow!("authorized Project list unavailable; privacy coverage is incomplete")
        })?;
    let selected = if codes.is_empty() {
        visible.iter().collect::<Vec<_>>()
    } else {
        let mut selected = Vec::new();
        for code in codes {
            let code = code.trim();
            ensure!(!code.is_empty(), "a Project code cannot be blank");
            let mut matches = visible.iter().filter(|project| project.code == code);
            let project = matches
                .next()
                .ok_or_else(|| anyhow!("no visible matter with that code"))?;
            ensure!(
                matches.next().is_none(),
                "that Project code is ambiguous; use --all-projects"
            );
            selected.push(project);
        }
        selected
    };
    let mut outcomes = Vec::new();
    for project in selected {
        let read = read_private_terms(&client, base, token, project.id)
            .await
            .and_then(|read| {
                if read.code == project.code {
                    Ok(resolved_terms(read))
                } else {
                    Err("private terms do not match the selected Project".to_string())
                }
            });
        let read = match read {
            Ok(terms) => ProjectRead::Resolved(terms),
            Err(reason) => {
                let reason = format!(
                    "private terms for `{}` could not be resolved: {reason}, \
                     so privacy coverage is incomplete",
                    project.code
                );
                if codes.is_empty() {
                    // `--all-projects`: one broken matter must not discard
                    // every later Project's terms. Collect the refusal and
                    // continue; the report names the uncovered matters.
                    ProjectRead::Refused { reason }
                } else {
                    return Err(anyhow!(reason));
                }
            }
        };
        outcomes.push(ProjectOutcome {
            code: project.code.clone(),
            read,
        });
    }
    Ok(outcomes)
}

/// The terms a clean resolution contributes to the scan: the identifiers,
/// then the Project's own code and name, then each participant's name,
/// mailbox, and phone.
fn resolved_terms(read: PrivateTerms) -> Vec<String> {
    let mut terms = Vec::new();
    terms.extend(read.identifiers);
    terms.push(read.code);
    terms.push(read.name);
    for person in read.participants {
        terms.push(person.name);
        terms.push(person.email);
        terms.extend(person.phone);
    }
    terms
}

/// Read one Project's private terms. The error channel is a display reason
/// built from the status class alone — the response body is server-side
/// data, and a local operator sees at most the class and code.
async fn read_private_terms(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    id: uuid::Uuid,
) -> Result<PrivateTerms, String> {
    let response = client
        .get(format!("{base}/app/api/projects/{id}/private-terms"))
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "could not read private terms".to_string())?;
    let status = response.status();
    if !status.is_success() {
        let reason = match status {
            reqwest::StatusCode::NOT_FOUND => {
                "the Project is unavailable to this firm-side session"
            }
            reqwest::StatusCode::UNAUTHORIZED => "the site login is unavailable or expired",
            reqwest::StatusCode::CONFLICT => "a linked Project record is missing",
            reqwest::StatusCode::UNPROCESSABLE_ENTITY => "stored Entity identifier data is invalid",
            _ if status.is_server_error() => "the server could not resolve private terms",
            _ => "the server refused the private-terms read",
        };
        return Err(format!("{reason} ({status})"));
    }
    response
        .json()
        .await
        .map_err(|_| "incomplete private-terms response".to_string())
}

/// The text one run checks. Each source becomes a labelled section so a
/// finding says which commit message, file diff or PR field carries it.
#[derive(Default)]
pub struct Sources {
    /// Draft title and body file; stdin when no other source is given.
    pub file: Option<PathBuf>,
    /// A Git revision range (`origin/main..HEAD`): commit messages and the
    /// lines the range adds.
    pub range: Option<String>,
    /// The pull request title.
    pub pr_title: Option<String>,
    /// A file holding the pull request body.
    pub pr_body_file: Option<PathBuf>,
}

impl Sources {
    fn read(&self) -> anyhow::Result<Vec<(String, String)>> {
        let mut sections = Vec::new();
        if self.file.is_none()
            && self.range.is_none()
            && self.pr_title.is_none()
            && self.pr_body_file.is_none()
        {
            sections.push((String::new(), std::io::read_to_string(std::io::stdin())?));
        }
        if let Some(path) = &self.file {
            sections.push((String::new(), std::fs::read_to_string(path)?));
        }
        if let Some(title) = &self.pr_title {
            sections.push(("PR title".to_string(), title.clone()));
        }
        if let Some(path) = &self.pr_body_file {
            sections.push(("PR body".to_string(), std::fs::read_to_string(path)?));
        }
        if let Some(range) = &self.range {
            sections.extend(range_sections(range)?);
        }
        Ok(sections)
    }
}

fn git(args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("git").args(args).output()?;
    if !out.status.success() {
        return Err(anyhow!("git {} failed", args.first().unwrap_or(&"")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One section per commit message and per file's added lines in `range`.
/// A range git cannot resolve is an error, never an empty clean scan.
fn range_sections(range: &str) -> anyhow::Result<Vec<(String, String)>> {
    let Some((base, tip)) = range.split_once("..") else {
        return Err(anyhow!(
            "a range needs two revisions, like origin/main..HEAD"
        ));
    };
    // Diff from the merge base, so commits that landed on the base since the
    // branch forked are not read as this branch's lines.
    let diff_range = format!("{base}...{}", tip.trim_start_matches('.'));
    let mut sections = Vec::new();
    for sha in git(&["rev-list", "--reverse", range, "--"])?.lines() {
        let message = without_attribution(&git(&["show", "-s", "--format=%B", sha])?);
        sections.push((
            format!("commit {} message", &sha[..sha.len().min(8)]),
            message,
        ));
    }
    sections.extend(added_lines_by_file(&git(&[
        "diff",
        "--no-color",
        "--no-ext-diff",
        "-U0",
        &diff_range,
        "--",
    ])?));
    Ok(sections)
}

/// Drop `Co-Authored-By` trailers naming a `noreply` address: the repository
/// requires that attribution, and it identifies no client.
fn without_attribution(message: &str) -> String {
    message
        .lines()
        .filter(|line| !(line.starts_with("Co-Authored-By:") && line.contains("<noreply@")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Group the added lines of a unified diff by file, labelled `diff <path>`.
fn added_lines_by_file(diff: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            let path = path.strip_prefix("b/").unwrap_or(path);
            sections.push((format!("diff {path}"), String::new()));
        } else if let Some(added) = line.strip_prefix('+') {
            if let Some((_, text)) = sections.last_mut() {
                text.push_str(added);
                text.push('\n');
            }
        }
    }
    sections.retain(|(_, text)| !text.is_empty());
    sections
}

/// Run the check over `file` (stdin when `None`). Exits 0 on a clean draft,
/// 1 naming each placeholder when it finds an identifier, 2 when the draft,
/// the terms, or a Project's coverage cannot be read.
pub async fn run(
    sources: Sources,
    terms: &[String],
    projects: &[String],
    all_projects: bool,
    host: Option<&str>,
) -> ExitCode {
    let Ok(sections) = sources.read() else {
        eprintln!("navigator: could not read the text to check");
        return ExitCode::from(2);
    };
    // Term-only checks work offline without any login.
    let (resolved, failures) = if projects.is_empty() && !all_projects {
        (Vec::new(), Vec::new())
    } else {
        let read = async {
            let local = manifest::read(Path::new("."));
            let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
            let (base, token) = remote::resolve(host).map_err(|_| {
                anyhow!("site login unavailable or expired; privacy coverage is incomplete")
            })?;
            private_terms(&base, &token, projects).await
        };
        match read.await {
            Ok(outcomes) => {
                let mut resolved = Vec::new();
                let mut failures = Vec::new();
                for outcome in outcomes {
                    match outcome.read {
                        ProjectRead::Resolved(mut terms) => resolved.append(&mut terms),
                        ProjectRead::Refused { reason } => failures.push((outcome.code, reason)),
                    }
                }
                (resolved, failures)
            }
            Err(e) => {
                eprintln!("navigator: privacy coverage could not complete: {e:#}");
                return ExitCode::from(2);
            }
        }
    };
    let terms = crate::pii_audit::merge_terms(&resolved, terms);
    let findings: Vec<(String, crate::feedback_scan::Finding)> = sections
        .iter()
        .flat_map(|(label, text)| {
            crate::feedback_scan::scan(text, &terms)
                .into_iter()
                .map(move |finding| (label.clone(), finding))
        })
        .collect();
    let (lines, code) = report(&findings, &failures);
    for (stream, line) in &lines {
        match stream {
            Stream::Out => println!("{line}"),
            Stream::Err => eprintln!("{line}"),
        }
    }
    code
}

/// Which standard stream a report line prints on. Findings and the
/// unresolved-coverage report go to stderr; a clean verdict goes to stdout.
#[derive(Debug, PartialEq)]
enum Stream {
    Out,
    Err,
}

/// Everything one run prints, in order, and its exit code.
///
/// The unresolved-coverage report is the first line when any matter refused:
/// a partially covered scan must never read as a clean one, and exit `2`
/// dominates both other codes — unresolved coverage is neither clean nor a
/// mere findings list.
fn report(
    findings: &[(String, crate::feedback_scan::Finding)],
    failures: &[(String, String)],
) -> (Vec<(Stream, String)>, ExitCode) {
    let mut lines = Vec::new();
    if !failures.is_empty() {
        let count = failures.len();
        lines.push((
            Stream::Err,
            format!(
                "coverage incomplete: {count} matter{} unresolved",
                if count == 1 { "" } else { "s" }
            ),
        ));
        for (code, reason) in failures {
            lines.push((Stream::Err, format!("{code}: {reason}")));
        }
    }
    for (label, finding) in findings {
        let prefix = if label.is_empty() {
            String::new()
        } else {
            format!("{label} ")
        };
        lines.push((
            Stream::Err,
            format!(
                "{prefix}line {}: {} found; write {} instead",
                finding.line, finding.kind, finding.placeholder
            ),
        ));
    }
    let code = if !failures.is_empty() {
        ExitCode::from(2)
    } else if findings.is_empty() {
        lines.push((Stream::Out, "feedback check: clean".to_string()));
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    };
    (lines, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diff_yields_only_added_lines_grouped_by_file() {
        let diff =
            "diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n@@ -1 +1,2 @@\n-old\n+new\n+more\n";
        assert_eq!(
            added_lines_by_file(diff),
            vec![("diff a.md".to_string(), "new\nmore\n".to_string())]
        );
    }

    #[test]
    fn a_bare_revision_is_not_a_range() {
        assert!(range_sections("HEAD").is_err());
    }

    #[test]
    fn the_attribution_trailer_is_not_scanned_but_other_lines_are() {
        let trailer = concat!("Co-Authored-By: Claude <noreply@", "anthropic.com>");
        let message = format!("feat: x\n\nbody\n\n{trailer}\n");
        assert_eq!(without_attribution(&message), "feat: x\n\nbody\n");
    }

    #[test]
    fn an_unresolvable_range_is_an_error_not_a_clean_scan() {
        assert!(range_sections("no-such-ref-xyz..HEAD").is_err());
    }

    #[test]
    fn a_finding_names_the_commit_or_field_that_carries_it() {
        let findings: Vec<_> =
            crate::feedback_scan::scan("Harper Cruller", &["Harper Cruller".to_string()])
                .into_iter()
                .map(|f| ("PR title".to_string(), f))
                .collect();
        let (lines, code) = report(&findings, &[]);
        assert!(lines[0].1.starts_with("PR title line 1:"));
        assert_eq!(code, ExitCode::FAILURE);
    }

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ID: &str = "22222222-2222-2222-2222-222222222222";

    /// The terms the resolved outcomes contribute, flattened for scanning.
    /// A refusal contributes nothing — that is the continue-past semantic —
    /// so the scan runs against every matter whose terms resolved.
    fn resolved_terms(outcomes: &[ProjectOutcome]) -> Vec<String> {
        outcomes
            .iter()
            .flat_map(|outcome| match &outcome.read {
                ProjectRead::Resolved(terms) => terms.clone(),
                ProjectRead::Refused { .. } => Vec::new(),
            })
            .collect()
    }

    /// The refusals as `(code, reason)` rows, in read order.
    fn refusals(outcomes: &[ProjectOutcome]) -> Vec<(String, String)> {
        outcomes
            .iter()
            .filter_map(|outcome| match &outcome.read {
                ProjectRead::Resolved(_) => None,
                ProjectRead::Refused { reason } => Some((outcome.code.clone(), reason.clone())),
            })
            .collect()
    }

    async fn site() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .and(header("authorization", "Bearer a-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "11111111-1111-1111-1111-111111111111", "code": "beta"},
                {"id": ID, "code": "cruller-estate"},
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{ID}/private-terms")))
            .and(header("authorization", "Bearer a-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "cruller-estate",
                "name": "Cruller Estate",
                "identifiers": [],
                "participants": [
                    {"name": "Harper Cruller", "email": "harper@crullers.example", "phone": "702-555-0142"},
                    {"name": "Lena Counsel", "email": "lena@firm.example", "phone": null},
                ],
            })))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn resolves_a_projects_code_and_participants_over_the_site_api() {
        let server = site().await;

        let outcomes = private_terms(&server.uri(), "a-token", &["cruller-estate".to_string()])
            .await
            .expect("resolve the private terms");
        let terms = resolved_terms(&outcomes);

        for known in [
            "cruller-estate",
            "Cruller Estate",
            "Harper Cruller",
            "harper@crullers.example",
            "702-555-0142",
            "Lena Counsel",
        ] {
            assert!(
                terms.iter().any(|t| t == known),
                "terms lack {known:?}: {terms:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_resolved_terms_flag_a_participants_name_mailbox_and_phone() {
        let server = site().await;
        let outcomes = private_terms(&server.uri(), "a-token", &["cruller-estate".to_string()])
            .await
            .expect("resolve the private terms");
        let terms = crate::pii_audit::merge_terms(&resolved_terms(&outcomes), &[]);

        for draft in [
            "Harper Cruller asked where the filing stands",
            "write to harper@crullers.example",
            "call 702-555-0142",
            "the cruller-estate matter",
        ] {
            assert!(
                !crate::feedback_scan::scan(draft, &terms).is_empty(),
                "{draft:?} scanned clean"
            );
        }
        assert!(crate::feedback_scan::scan(
            "the status command exits 0 when the folder is empty",
            &terms
        )
        .is_empty());
    }

    #[tokio::test]
    async fn all_projects_scans_an_unmentioned_matter_and_its_entity() {
        let server = site().await;
        Mock::given(method("GET"))
            .and(path(
                "/app/api/projects/11111111-1111-1111-1111-111111111111/private-terms",
            ))
            .and(header("authorization", "Bearer a-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "beta", "name": "Beta Matter", "participants": [],
                "identifiers": ["Synthetic Widget Company"]
            })))
            .mount(&server)
            .await;
        let outcomes = private_terms(&server.uri(), "a-token", &[]).await.unwrap();
        let terms = resolved_terms(&outcomes);
        for draft in [
            "beta failed",
            "Synthetic Widget Company had a failure",
            "Harper Cruller",
        ] {
            assert!(!crate::feedback_scan::scan(draft, &terms).is_empty());
        }
        assert!(
            crate::feedback_scan::scan("the command fails on an empty directory", &terms)
                .is_empty()
        );
    }

    /// A refused read is reported with the failing Project's code — from the
    /// caller's own participation list — and never echoes the response body.
    #[tokio::test]
    async fn a_server_failure_reports_incomplete_coverage_without_echoing_the_response_body() {
        let server = site().await;
        Mock::given(method("GET"))
            .and(path(
                "/app/api/projects/11111111-1111-1111-1111-111111111111/private-terms",
            ))
            .respond_with(ResponseTemplate::new(500).set_body_string("Synthetic Private Marker"))
            .mount(&server)
            .await;

        let outcomes = private_terms(&server.uri(), "a-token", &[])
            .await
            .expect("the sweep continues past the refusal");
        let refusals = refusals(&outcomes);
        assert_eq!(refusals.len(), 1);
        let (code, reason) = &refusals[0];
        assert_eq!(code, "beta");
        assert!(reason.contains("privacy coverage is incomplete"));
        assert!(reason.contains("server could not resolve private terms"));
        assert!(reason.contains("500"));
        assert!(reason.contains("beta"));
        assert!(!reason.contains("Synthetic Private Marker"));
        assert!(!reason.contains("firm-side participant"));
    }

    /// A single named Project has nothing to continue past: the refusal is
    /// fatal, and it names the Project.
    #[tokio::test]
    async fn a_single_project_refusal_names_the_project() {
        let server = site().await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{ID}/private-terms")))
            .respond_with(ResponseTemplate::new(409))
            .with_priority(1)
            .mount(&server)
            .await;

        let error = private_terms(&server.uri(), "a-token", &["cruller-estate".to_string()])
            .await
            .expect_err("a refused single-Project read must not scan as nothing");

        let message = format!("{error:#}");
        assert!(message.contains("cruller-estate"), "{message}");
        assert!(
            message.contains("a linked Project record is missing"),
            "{message}"
        );
        assert!(message.contains("409"), "{message}");
        assert!(
            message.contains("privacy coverage is incomplete"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn all_projects_continues_past_a_refused_matter_and_scans_the_terms_that_resolved() {
        let server = site().await;
        Mock::given(method("GET"))
            .and(path(
                "/app/api/projects/11111111-1111-1111-1111-111111111111/private-terms",
            ))
            .respond_with(ResponseTemplate::new(409))
            .mount(&server)
            .await;

        let outcomes = private_terms(&server.uri(), "a-token", &[])
            .await
            .expect("the sweep continues past the refusal");

        let refusals = refusals(&outcomes);
        assert_eq!(refusals.len(), 1);
        assert_eq!(refusals[0].0, "beta");
        assert!(refusals[0].1.contains("a linked Project record is missing"));
        let terms = resolved_terms(&outcomes);
        assert!(!crate::feedback_scan::scan("Harper Cruller", &terms).is_empty());
    }

    #[tokio::test]
    async fn all_projects_reports_unresolved_matters_in_the_outcome_shape() {
        let server = site().await;

        let outcomes = private_terms(&server.uri(), "a-token", &[])
            .await
            .expect("a refused matter is an outcome, not an aborted sweep");

        let refusals = refusals(&outcomes);
        assert_eq!(refusals.len(), 1);
        assert_eq!(refusals[0].0, "beta");
        assert!(refusals[0]
            .1
            .contains("unavailable to this firm-side session"));
        assert!(refusals[0].1.contains("privacy coverage is incomplete"));
        let terms = resolved_terms(&outcomes);
        assert!(
            !terms.is_empty(),
            "the healthy matter's terms still resolved"
        );
        assert!(terms.contains(&"cruller-estate".to_string()));
    }

    #[tokio::test]
    async fn failed_project_listing_does_not_echo_a_remote_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(403).set_body_string("Synthetic Private Marker"))
            .mount(&server)
            .await;
        let error = private_terms(&server.uri(), "a-token", &[])
            .await
            .unwrap_err();
        assert!(!format!("{error:#}").contains("Synthetic Private Marker"));
        assert!(format!("{error:#}").contains("privacy coverage is incomplete"));
    }

    #[tokio::test]
    async fn an_incomplete_term_response_cannot_pass_as_clean() {
        let server = site().await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{ID}/private-terms")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": "cruller-estate", "name": "Cruller Estate", "participants": []
            })))
            .with_priority(1)
            .mount(&server)
            .await;
        assert!(
            private_terms(&server.uri(), "a-token", &["cruller-estate".into()])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn identical_codes_are_read_by_id_in_all_projects_and_refused_when_ambiguous() {
        let server = site().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "11111111-1111-1111-1111-111111111111", "code": "shared"},
                {"id": ID, "code": "shared"}
            ])))
            .with_priority(1)
            .mount(&server)
            .await;
        for (id, name) in [
            (
                "11111111-1111-1111-1111-111111111111",
                "First Synthetic Entity",
            ),
            (ID, "Second Synthetic Entity"),
        ] {
            Mock::given(method("GET")).and(path(format!("/app/api/projects/{id}/private-terms")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "code": "shared", "name": "Synthetic Matter", "participants": [], "identifiers": [name]
                }))).with_priority(1).mount(&server).await;
        }
        let terms = resolved_terms(&private_terms(&server.uri(), "a-token", &[]).await.unwrap());
        assert!(!crate::feedback_scan::scan("First Synthetic Entity", &terms).is_empty());
        assert!(!crate::feedback_scan::scan("Second Synthetic Entity", &terms).is_empty());
        assert!(private_terms(&server.uri(), "a-token", &["shared".into()])
            .await
            .is_err());
    }

    #[tokio::test]
    async fn an_unknown_or_blank_code_is_an_error_not_a_clean_scan() {
        let server = site().await;

        for code in ["no-such-project", "  "] {
            assert!(
                private_terms(&server.uri(), "a-token", &[code.to_string()])
                    .await
                    .is_err(),
                "{code:?} must not scan as nothing"
            );
        }
    }

    #[tokio::test]
    async fn a_refused_read_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{"id": ID, "code": "cruller-estate"}])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{ID}/private-terms")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let error = private_terms(&server.uri(), "a-token", &["cruller-estate".to_string()])
            .await
            .expect_err("a 404 must not scan as nothing");

        let message = format!("{error:#}");
        assert!(message.contains("404"));
        assert!(message.contains("cruller-estate"), "{message}");
    }

    #[test]
    fn the_coverage_banner_is_the_first_line_of_an_incompletely_covered_run() {
        let failures = vec![
            (
                "beta".to_string(),
                "private terms for `beta` could not be resolved: a linked Project record is \
                 missing (409 Conflict), so privacy coverage is incomplete"
                    .to_string(),
            ),
            (
                "gamma".to_string(),
                "private terms for `gamma` could not be resolved: stored Entity identifier data \
                 is invalid (422 Unprocessable Entity), so privacy coverage is incomplete"
                    .to_string(),
            ),
        ];
        let findings: Vec<_> =
            crate::feedback_scan::scan("Harper Cruller", &["Harper Cruller".to_string()])
                .into_iter()
                .map(|f| (String::new(), f))
                .collect();
        assert!(!findings.is_empty(), "the scan must produce a finding");

        let (lines, code) = report(&findings, &failures);

        assert_eq!(lines[0].0, Stream::Err);
        assert_eq!(lines[0].1, "coverage incomplete: 2 matters unresolved");
        assert_eq!(
            lines[1].1,
            "beta: private terms for `beta` could not be resolved: a \
             linked Project record is missing (409 Conflict), so privacy coverage is incomplete"
        );
        let first_finding = lines
            .iter()
            .position(|(stream, line)| *stream == Stream::Err && line.starts_with("line "))
            .expect("the findings are printed");
        assert!(first_finding > 1, "the banner and its rows come first");
        assert_eq!(code, ExitCode::from(2));
    }

    #[test]
    fn unresolved_coverage_is_exit_two_even_when_the_scan_is_clean() {
        let failures = vec![("beta".to_string(), "refused".to_string())];

        let (lines, code) = report(&[], &failures);

        assert!(!lines
            .iter()
            .any(|(_, line)| line.contains("feedback check: clean")));
        assert_eq!(code, ExitCode::from(2));
    }

    #[test]
    fn a_clean_run_prints_the_clean_verdict_alone_and_succeeds() {
        let (lines, code) = report(&[], &[]);

        assert_eq!(
            lines,
            vec![(Stream::Out, "feedback check: clean".to_string())]
        );
        assert_eq!(code, ExitCode::SUCCESS);
    }

    /// A draft on disk, the pull-request title that goes with it, and no range.
    fn drafted(dir: &Path, body: &str, title: &str) -> Sources {
        let file = dir.join("draft.md");
        std::fs::write(&file, body).unwrap();
        let pr_body_file = dir.join("pr-body.md");
        std::fs::write(&pr_body_file, "Summary of the change.\n").unwrap();
        Sources {
            file: Some(file),
            pr_title: Some(title.to_string()),
            pr_body_file: Some(pr_body_file),
            range: None,
        }
    }

    #[tokio::test]
    async fn a_term_only_check_runs_offline_and_names_each_carrier() {
        let dir = tempfile::tempdir().unwrap();
        let terms = ["sample marker".to_string()];
        let clean = drafted(
            dir.path(),
            "The template cites the wrong rule.\n",
            "Fix rule",
        );
        assert_eq!(
            run(clean, &terms, &[], false, None).await,
            ExitCode::SUCCESS
        );
        let named = drafted(dir.path(), "sample marker reported it.\n", "Fix rule");
        assert_eq!(
            run(named, &terms, &[], false, None).await,
            ExitCode::FAILURE
        );
        let titled = drafted(dir.path(), "Reported.\n", "Fix sample marker's letter");
        assert_eq!(
            run(titled, &terms, &[], false, None).await,
            ExitCode::FAILURE
        );
        let unreadable = Sources {
            file: Some(dir.path().join("absent.md")),
            ..Sources::default()
        };
        assert_eq!(
            run(unreadable, &terms, &[], false, None).await,
            ExitCode::from(2)
        );
        let not_a_range = Sources {
            range: Some("HEAD".to_string()),
            ..Sources::default()
        };
        assert_eq!(
            run(not_a_range, &terms, &[], false, None).await,
            ExitCode::from(2)
        );
    }

    #[tokio::test]
    async fn a_project_check_reads_private_terms_with_the_stored_login() {
        use crate::projects::test_login::{answer, site};
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let refused = uuid::Uuid::new_v4();
        let (server, _login) = site(serde_json::json!([
            {"id": id, "code": "cruller-estate"},
            {"id": refused, "code": "beta"},
        ]))
        .await;
        let host = server.uri();
        let terms = serde_json::json!({
            "code": "cruller-estate",
            "name": "Cruller Estate",
            "identifiers": [],
            "participants": [{"name": "Harper Cruller", "email": "harper@crullers.example", "phone": null}],
        });
        let door = format!("/app/api/projects/{id}/private-terms");
        answer(&server, "GET", &door, 200, terms).await;
        let one = ["cruller-estate".to_string()];
        let draft = |body: &str| drafted(dir.path(), body, "Fix rule");
        let here = Some(host.as_str());
        let clean = run(draft("Wrong rule.\n"), &[], &one, false, here).await;
        assert_eq!(clean, ExitCode::SUCCESS);
        let named = run(draft("Harper Cruller wrote.\n"), &[], &one, false, here).await;
        assert_eq!(named, ExitCode::FAILURE);
        // `beta` refuses its terms, so an all-Projects scan is incomplete.
        let partial = run(draft("Wrong rule.\n"), &[], &[], true, here).await;
        assert_eq!(partial, ExitCode::from(2));
        let elsewhere = Some("signed-out.example.test");
        let signed_out = run(draft("Wrong rule.\n"), &[], &one, false, elsewhere).await;
        assert_eq!(signed_out, ExitCode::from(2));
    }
}
