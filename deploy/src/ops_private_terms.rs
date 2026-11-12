//! `hudson ops private-terms` — the private-terms reconciliation report
//! (LAW-301).
//!
//! Read-only, and with no local database coordinates: it reads the
//! admin-tier `GET /app/api/project-private-terms-reconciliation` door over
//! the `site login` session — the same discipline as `projects drift` — and
//! prints one row per broken matter. It never repairs a row: a finding
//! names the work (`remove_participant`, restore the record, repair the
//! Entity field), and a human decides before anything writes.
//!
//! # Exit codes
//!
//! | Code | Meaning |
//! | --- | --- |
//! | `0` | Every matter's private-terms records reconcile. |
//! | `1` | At least one matter is broken. |
//! | `2` | The report could not be produced — the login, the host, or the response. |
//!
//! The split between `1` and `2` is the one that matters: a gate that
//! treats "broken" and "could not ask" alike goes green on an expired
//! token.

use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use uuid::Uuid;

/// The reconciliation report the door returns: one entry per broken matter,
/// nothing for the healthy ones.
#[derive(Debug, Deserialize)]
struct Reconciliation {
    findings: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    project_id: Uuid,
    code: String,
    items: Vec<Item>,
}

/// One finding. The kinds mirror `store::private_terms::Finding`; the
/// payload is only the dangling record id and where it hangs from — or
/// which field is malformed — never a name, mailbox, phone, or EIN.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Item {
    /// A participation or referenced-person id with no Person record.
    MissingPerson { id: Uuid, attached_as: String },
    /// The matter's client Entity, or a referenced-entity id, with no
    /// Entity record.
    MissingEntity { id: Uuid, attached_as: String },
    /// An Entity's stored `metadata` is not a JSON object.
    InvalidEntityMetadata { id: Uuid, field: String },
    /// An Entity's `metadata.ein` is present but not `NN-NNNNNNN`.
    InvalidEntityEin { id: Uuid, field: String },
}

impl std::fmt::Display for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Item::MissingPerson { id, attached_as } => {
                write!(f, "missing_person {id} attached as {attached_as}")
            }
            Item::MissingEntity { id, attached_as } => {
                write!(f, "missing_entity {id} attached as {attached_as}")
            }
            Item::InvalidEntityMetadata { id, field } => {
                write!(f, "invalid_entity_metadata {id} in {field}")
            }
            Item::InvalidEntityEin { id, field } => {
                write!(f, "invalid_entity_ein {id} in {field}")
            }
        }
    }
}

/// Read the reconciliation door.
async fn fetch(base: &str, token: &str) -> Result<Reconciliation> {
    let response = reqwest::Client::new()
        .get(format!(
            "{base}/app/api/project-private-terms-reconciliation"
        ))
        .bearer_auth(token)
        .send()
        .await
        .context("GET /app/api/project-private-terms-reconciliation")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        // The body is quoted, not discarded: this door is admin-tier, so
        // the overwhelmingly likely failure is a caller who is merely
        // lawyer-tier, and a bare status leaves them guessing which of the
        // two it was.
        return Err(anyhow!(
            "reading the private-terms reconciliation failed: {status}: {}",
            first_line(&body)
        ));
    }
    serde_json::from_str(&body).context("parse the private-terms reconciliation report")
}

/// The first line of a response body, for a one-line error.
fn first_line(body: &str) -> &str {
    body.lines().next().unwrap_or_default().trim()
}

/// The summary line under the rows.
fn summary(broken: usize, total: usize) -> String {
    format!(
        "{broken} broken matter{}, {total} finding{}",
        if broken == 1 { "" } else { "s" },
        if total == 1 { "" } else { "s" }
    )
}

/// Every line the report prints, in order. One row per broken matter — its
/// code, then each finding's kind, id, and attachment or field path — and
/// the summary line under them.
fn render(report: &Reconciliation) -> Vec<String> {
    if report.findings.is_empty() {
        return vec![summary(0, 0)];
    }
    let mut lines = Vec::new();
    let mut total = 0;
    for entry in &report.findings {
        lines.push(format!("{} ({})", entry.code, entry.project_id));
        for item in &entry.items {
            total += 1;
            lines.push(format!("  {item}"));
        }
    }
    lines.push(summary(report.findings.len(), total));
    lines
}

/// `hudson ops private-terms`.
pub(crate) async fn run(host: Option<&str>) -> ExitCode {
    let (base, token) = match crate::remote::resolve(host) {
        Ok((base, token)) => (base, token),
        Err(error) => {
            eprintln!("navigator: {error:#}");
            return ExitCode::from(2);
        }
    };
    match fetch(&base, &token).await {
        Ok(report) => {
            for line in render(&report) {
                println!("{line}");
            }
            if report.findings.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PERSON: &str = "22222222-2222-2222-2222-222222222222";
    const ENTITY: &str = "33333333-3333-3333-3333-333333333333";
    const PROJECT: &str = "44444444-4444-4444-4444-444444444444";

    /// The shape the door's own handler tests assert (LAW-301 PR 1), quoted
    /// here as a fixture so the CLI's rendering is pinned to the real wire
    /// format rather than to this module's types.
    fn fixture_report() -> String {
        serde_json::json!({
            "findings": [
                {
                    "project_id": PROJECT,
                    "code": "cruller-estate",
                    "items": [
                        {"kind": "missing_person", "id": PERSON, "attached_as": "participation"},
                        {"kind": "invalid_entity_ein", "id": ENTITY, "field": "metadata.ein"},
                    ],
                },
            ],
        })
        .to_string()
    }

    #[tokio::test]
    async fn renders_findings_from_a_fixture_door_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/project-private-terms-reconciliation"))
            .and(header("authorization", "Bearer a-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture_report()))
            .mount(&server)
            .await;

        let report = fetch(&server.uri(), "a-token")
            .await
            .expect("read the report");

        let lines = render(&report);
        assert_eq!(
            lines,
            vec![
                format!("cruller-estate ({PROJECT})"),
                format!("  missing_person {PERSON} attached as participation"),
                format!("  invalid_entity_ein {ENTITY} in metadata.ein"),
                "1 broken matter, 2 findings".to_string(),
            ]
        );
    }

    #[test]
    fn a_clean_deployment_prints_the_summary_alone() {
        let report = Reconciliation {
            findings: Vec::new(),
        };
        assert_eq!(render(&report), vec!["0 broken matters, 0 findings"]);
    }

    #[test]
    fn the_summary_line_pluralizes() {
        assert_eq!(summary(1, 1), "1 broken matter, 1 finding");
        assert_eq!(summary(2, 1), "2 broken matters, 1 finding");
        assert_eq!(summary(1, 3), "1 broken matter, 3 findings");
    }

    #[tokio::test]
    async fn every_finding_class_the_door_reports_is_rendered() {
        let report: Reconciliation = serde_json::from_str(&serde_json::json!({
            "findings": [
                {
                    "project_id": PROJECT,
                    "code": "cruller-estate",
                    "items": [
                        {"kind": "missing_person", "id": PERSON, "attached_as": "reference:person"},
                        {"kind": "missing_entity", "id": ENTITY, "attached_as": "project.client"},
                        {"kind": "missing_entity", "id": ENTITY, "attached_as": "reference:entity"},
                        {"kind": "invalid_entity_metadata", "id": ENTITY, "field": "metadata"},
                    ],
                },
            ],
        })
        .to_string())
        .expect("parse the fixture");

        assert_eq!(
            render(&report)[1..5],
            [
                format!("  missing_person {PERSON} attached as reference:person"),
                format!("  missing_entity {ENTITY} attached as project.client"),
                format!("  missing_entity {ENTITY} attached as reference:entity"),
                format!("  invalid_entity_metadata {ENTITY} in metadata"),
            ]
        );
    }

    #[tokio::test]
    async fn a_lawyer_tier_refusal_quotes_the_response_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/project-private-terms-reconciliation"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": "admin_tier_required",
            })))
            .mount(&server)
            .await;

        let error = fetch(&server.uri(), "a-token")
            .await
            .expect_err("a 403 must not read as a clean report");

        let rendered = format!("{error:#}");
        assert!(rendered.contains("403"), "{rendered}");
        assert!(rendered.contains("admin_tier_required"), "{rendered}");
    }

    #[tokio::test]
    async fn a_response_that_is_not_the_report_cannot_pass_as_clean() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/project-private-terms-reconciliation"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        assert!(fetch(&server.uri(), "a-token").await.is_err());
    }
}
