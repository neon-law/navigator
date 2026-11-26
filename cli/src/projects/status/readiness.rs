use std::{collections::BTreeMap, process::ExitCode};

use anyhow::Result;
use chrono::{DateTime, Utc};
use comfy_table::Table;
use futures::{stream, StreamExt};
use model::billing_report::{BillingAction, BillingReport};
use model::projects::OnboardingAgreement;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::remote;

use super::super::{calendar, deadline_brief as deadlines};

#[derive(Deserialize)]
struct Project {
    id: Uuid,
    code: String,
    name: String,
    status: String,
    practice_area: Option<String>,
}

#[derive(Deserialize)]
struct Inventory {
    projects: Vec<Project>,
}

pub(super) async fn run(host: Option<&str>, json: bool) -> ExitCode {
    match report(host).await {
        Ok(report) => {
            if json {
                println!("{report}");
            } else {
                print(&report);
            }
            if report["complete"] == true {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("navigator project status: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn report(host: Option<&str>) -> Result<Value> {
    let (base, token) = remote::resolve(host)?;
    read_report(&base, &token).await
}

async fn read_report(base: &str, token: &str) -> Result<Value> {
    let client = reqwest::Client::new();
    let now = Utc::now();
    let (projects, agreements) = tokio::join!(
        deadlines::list::<Inventory>(
            &client,
            format!("{base}{}?include_pitch=true", deadlines::INVENTORY_PATH),
            token,
        ),
        deadlines::list::<Vec<OnboardingAgreement>>(
            &client,
            format!("{base}/app/api/onboarding-agreements"),
            token,
        ),
    );
    let projects = projects?.projects;
    let expected = projects.len();
    let mut rows = stream::iter(projects.into_iter().map(|project| {
        read_project(
            &client,
            base,
            token,
            project,
            agreements.as_deref().ok(),
            now,
        )
    }))
    .buffer_unordered(deadlines::READ_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    rows.sort_by(|a, b| {
        (a["practice_area"].as_str(), a["code"].as_str())
            .cmp(&(b["practice_area"].as_str(), b["code"].as_str()))
    });
    let mut blockers = BTreeMap::new();
    let mut complete = agreements.is_ok();
    for row in &rows {
        for field in ["status", "billing", "calendar", "deadlines"] {
            if row[field].get("error").is_some() {
                complete = false;
                *blockers.entry(format!("{field} read failed")).or_insert(0) += 1;
            }
        }
        if let Some(missing) = row["calendar"]["project_calendar"]["missing"].as_str() {
            *blockers
                .entry(format!("Project calendar {missing}"))
                .or_insert(0) += 1;
        }
    }
    Ok(json!({
        "host": base,
        "as_of": now,
        "complete": complete,
        "projects_expected": expected,
        "agreement_error": agreements.err().map(|error| error.to_string()),
        "blockers": blockers,
        "projects": rows,
    }))
}

async fn read_project(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    project: Project,
    agreements: Option<&[OnboardingAgreement]>,
    now: DateTime<Utc>,
) -> Value {
    let route = format!("{base}/app/api/projects/{}", project.id);
    let mut row = json!({
        "code": project.code, "name": project.name, "project_status": project.status,
        "practice_area": project.practice_area,
    });
    let status: Value = match deadlines::list(client, format!("{route}/status"), token).await {
        Ok(status) => status,
        Err(error) => {
            row["status"] = json!({"error": error.to_string()});
            return row;
        }
    };
    let (billing, calendar, open) = tokio::join!(
        deadlines::list::<Value>(client, format!("{route}/billing"), token),
        calendar::post(
            base,
            token,
            "/app/api/calendars/access",
            json!({"project_code": project.code})
        ),
        deadlines::open_for_project(
            client,
            base,
            token,
            project.id,
            &project.code,
            &project.name,
            now
        ),
    );
    let envelopes =
        agreements.map(|agreements| engagement_envelopes(&status, &project.code, agreements));
    row["status"] = json!({
        "onboarding": status["onboarding"],
        "engagement_envelopes": envelopes,
    });
    row["billing"] = read_result(billing);
    row["calendar"] = read_result(calendar);
    row["deadlines"] = read_result(open.map(Value::Array));
    row
}

fn read_result(result: Result<Value>) -> Value {
    result.unwrap_or_else(|error| json!({"error": error.to_string()}))
}

fn engagement_envelopes<'a>(
    status: &'a Value,
    code: &str,
    agreements: &[OnboardingAgreement],
) -> Vec<&'a Value> {
    status["envelopes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            agreements.iter().any(|agreement| {
                agreement.project == code
                    && (agreement
                        .notation_id
                        .is_some_and(|id| entry["notation_id"] == json!(id))
                        || agreement
                            .envelope_id
                            .as_deref()
                            .is_some_and(|id| entry["envelope"]["envelopeId"].as_str() == Some(id)))
            })
        })
        .map(|entry| &entry["envelope"])
        .collect()
}

fn next_deadline(project: &Value) -> String {
    let deadline = &project["deadlines"][0];
    let when = if deadline["due_on"].is_object() {
        &deadline["due_local"]
    } else {
        &deadline["due_at"]
    };
    if let Some(due) = when.as_str() {
        format!(
            "{}{} • {}",
            if deadline["overdue"] == true {
                "Overdue "
            } else {
                ""
            },
            due,
            deadline["title"].as_str().unwrap_or("Deadline")
        )
    } else if project["deadlines"].is_array() {
        "None recorded".into()
    } else {
        "Unavailable".into()
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum BillingException {
    PendingTime(Uuid),
    UnallocatedWork,
    OutstandingClaim(Uuid),
}

fn billing_exceptions(code: &str, billing: &Value, lines: &mut Vec<String>) {
    let Ok(report) = BillingReport::deserialize(billing) else {
        lines.push(format!("{code} · Billing unavailable"));
        return;
    };

    // Prefer the action when the same annotation or claim also appears in the detail.
    let mut exceptions = BTreeMap::new();
    for action in report
        .needs_my_action
        .into_iter()
        .chain(report.other_claims.into_iter().map(BillingAction::Claim))
    {
        let (identity, line) = match action {
            BillingAction::PendingAnnotation {
                annotation_id,
                performed_on,
                next_action,
                ..
            } => (
                BillingException::PendingTime(annotation_id),
                format!("Pending time · {performed_on} · {next_action}"),
            ),
            BillingAction::UnallocatedWork { next_action, .. } => (
                BillingException::UnallocatedWork,
                format!("Completed unallocated work · {next_action}"),
            ),
            BillingAction::Claim(claim) => {
                let Some(cents) = claim.approved_outstanding_cents.filter(|&cents| cents > 0)
                else {
                    continue;
                };
                let amount = crate::billing::money(Some(cents));
                let mut row = format!(
                    "Claim · {} · Approved outstanding {amount} · {}",
                    claim.benefit, claim.performed_on
                );
                if let Some(action) = claim.next_action {
                    row.push_str(" · ");
                    row.push_str(&action);
                }
                (BillingException::OutstandingClaim(claim.id), row)
            }
        };
        exceptions.entry(identity).or_insert(line);
    }
    for time in report
        .my_time
        .into_iter()
        .chain(report.project_time.into_iter().flatten())
    {
        if time.status == "pending_review" {
            exceptions
                .entry(BillingException::PendingTime(time.id))
                .or_insert_with(|| {
                    format!(
                        "Pending time · {} · {} min",
                        time.performed_on, time.minutes
                    )
                });
        }
    }
    lines.extend(
        exceptions
            .into_values()
            .map(|line| format!("{code} · {line}")),
    );
}

fn print(report: &Value) {
    let mut table = Table::new();
    let mut exceptions = Vec::new();
    table.set_header([
        "Practice",
        "Project",
        "Next deadline",
        "Onboarding",
        "Envelope",
        "Calendar",
        "Billing",
    ]);
    for project in report["projects"].as_array().into_iter().flatten() {
        let next = next_deadline(project);
        let envelopes = project["status"]["engagement_envelopes"].as_array();
        let states = envelopes
            .into_iter()
            .flatten()
            .filter_map(|e| e["status"].as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let calendar = &project["calendar"]["project_calendar"];
        let billing = &project["billing"];
        let code = project["code"].as_str().unwrap_or_default();
        billing_exceptions(code, billing, &mut exceptions);
        let fees = if billing.is_null() || billing.get("error").is_some() {
            "Unavailable".into()
        } else {
            format!(
                "{} actions, {} fee projections, {} other claims",
                billing["needs_my_action"].as_array().map_or(0, Vec::len),
                billing["verified_projected_fees"]
                    .as_array()
                    .map_or(0, Vec::len),
                billing["other_claims"].as_array().map_or(0, Vec::len)
            )
        };
        table.add_row([
            project["practice_area"].as_str().unwrap_or("Unassigned"),
            code,
            &next,
            project["status"]["onboarding"]["status"]
                .as_str()
                .unwrap_or("Unavailable"),
            if states.is_empty() {
                "Dispatch unverified"
            } else {
                &states
            },
            if calendar["readable"] == true {
                "Readable"
            } else {
                calendar["missing"].as_str().unwrap_or("Unavailable")
            },
            &fees,
        ]);
    }
    println!("{table}");
    println!("\nDaily billing exceptions");
    if exceptions.is_empty() {
        println!("  None");
    }
    for line in exceptions {
        println!("  {line}");
    }
    if let Some(error) = report["agreement_error"].as_str() {
        println!("Agreement evidence unavailable: {error}");
    }
    if let Some(blockers) = report["blockers"].as_object() {
        for (blocker, count) in blockers {
            println!("{blocker}: {count}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_deadline_labels_report_the_civil_date_with_its_recorded_cutoff() {
        let project = json!({"deadlines": [{
            "title":"Synthetic response", "due_at":"2028-07-13T06:59:00Z",
            "due_on":{"date":"2028-07-12","cutoff":"23:59:00","timezone":"America/Los_Angeles"},
            "due_local":"Wednesday, 12 Jul 2028 · 23:59:00 America/Los_Angeles"
        }]});
        assert_eq!(
            next_deadline(&project),
            "Wednesday, 12 Jul 2028 · 23:59:00 America/Los_Angeles • Synthetic response"
        );
    }

    #[test]
    fn the_open_only_inventory_cannot_look_like_an_empty_fleet() {
        assert!(serde_json::from_value::<Inventory>(json!([])).is_err());
        assert!(serde_json::from_value::<Inventory>(json!({"projects": []}))
            .unwrap()
            .projects
            .is_empty());
    }

    #[tokio::test]
    async fn readiness_keeps_other_projects_when_participation_is_revoked() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let open = Uuid::new_v4();
        let revoked = Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(deadlines::INVENTORY_PATH))
            .and(query_param("include_pitch", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"projects": [
                {"id": open, "code": "sample-open", "name": "Sample Open", "status": "pitch", "practice_area": "estates"},
                {"id": revoked, "code": "sample-revoked", "name": "Sample Revoked", "status": "open"}
            ]})))
            .mount(&server).await;
        for (route, body) in [
            ("/app/api/onboarding-agreements".into(), json!([])),
            (
                format!("/app/api/projects/{open}/status"),
                json!({"onboarding": {"status": "pitch"}}),
            ),
            (format!("/app/api/projects/{open}/billing"), json!({})),
            (format!("/app/api/projects/{open}/deadlines"), json!([])),
        ] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/app/api/calendars/access"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "project_calendar": {"readable": false, "missing": "unrecorded"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let report = read_report(&server.uri(), "fixture-token").await.unwrap();
        assert_eq!(report["complete"], false);
        assert_eq!(report["blockers"]["Project calendar unrecorded"], 1);
        assert_eq!(report["projects"][0]["code"], "sample-revoked");
        assert!(report["projects"][0]["status"]["error"]
            .as_str()
            .unwrap()
            .contains("404"));
        assert_eq!(report["projects"][1]["deadlines"], json!([]));
        assert_eq!(
            report["projects"][1]["status"]["onboarding"]["status"],
            "pitch"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 7);
    }

    #[test]
    fn engagement_evidence_excludes_unrelated_and_unidentified_envelopes() {
        let status = json!({"envelopes": [
            {"notation_id": null, "envelope": {"envelopeId": "unrelated", "status": "completed"}},
            {"notation_id": null, "envelope": {"envelopeId": "engagement", "status": "sent"}}
        ]});
        let agreements = [OnboardingAgreement {
            project: "sample-matter".into(),
            title: "Engagement".into(),
            state: "requested".into(),
            notation_id: None,
            document_id: None,
            envelope_id: Some("engagement".into()),
        }];
        let envelopes = engagement_envelopes(&status, "sample-matter", &agreements);
        assert_eq!(envelopes, vec![&status["envelopes"][1]["envelope"]]);
        assert!(engagement_envelopes(&status, "another-matter", &agreements).is_empty());
    }
}
