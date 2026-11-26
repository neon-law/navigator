//! The Project status command reads the authenticated live state without writes.
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn status_shows_live_history_in_text_and_json() {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
    std::fs::write(
        root.path().join("navigator.yaml"),
        format!("version: 26.10.1\nproject:\n  host: {host}\n  name: acme\n"),
    )
    .unwrap();
    let credentials = root.path().join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::json!({"hosts": {host.clone(): {
            "token": "test-token", "person_email": "lawyer@example.com",
            "role": "lawyer", "expires_at": i64::MAX
        }}})
        .to_string(),
    )
    .unwrap();
    let id = Uuid::now_v7();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": id, "code": "acme"}
        ])))
        .expect(2)
        .mount(&server)
        .await;
    let attester = Uuid::now_v7();
    let payload = serde_json::json!({"code":"acme", "status":"active",
        "history": [
            {"from_status": null, "to_status": "open", "effective_at": "2026-01-01T00:00:00Z",
             "recorded_at": "2026-01-01T00:00:00Z", "reason": null, "attesting_person_id": attester},
            {"from_status": "open", "to_status": "closed", "effective_at": "2026-02-01T00:00:00Z",
             "recorded_at": "2026-02-02T00:00:00Z", "reason": "engagement_completed", "attesting_person_id": attester},
            {"from_status": "closed", "to_status": "open", "effective_at": "2026-03-01T00:00:00Z",
             "recorded_at": "2026-03-02T00:00:00Z", "reason": "new_matter_event", "attesting_person_id": attester}
        ],
        "opened_at":"2026-01-01T00:00:00Z", "closed_at":null,
        "source_state":"initialized", "drive_folder_id":"synthetic-folder", "onboarding":{"filed":true,"documents":[
            {"id":Uuid::now_v7(),"visibility":"internal"}
        ]}, "missing":[]});
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{id}/status")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&payload))
        .expect(2)
        .mount(&server)
        .await;
    let output = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args(["project", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
        payload
    );
    let status = Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(root.path())
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .args(["project", "status"])
        .assert()
        .success();
    let output = String::from_utf8_lossy(&status.get_output().stdout);
    let opened = output.find("none -> open").expect("initial opening");
    let closed = output.find("open -> closed").expect("first closure");
    let reopened = output.find("closed -> open").expect("reopening");
    assert!(opened < closed && closed < reopened);
    assert!(output.contains("effective: 2026-02-01T00:00:00Z"));
    assert!(output.contains("recorded: 2026-02-02T00:00:00Z"));
    assert!(output.contains("reason: engagement_completed"));
    assert!(output.contains("reason: new_matter_event"));
    assert!(output.contains(&format!("attester: {attester}")));
    assert!(
        output.contains("drive folder: https://drive.google.com/drive/folders/synthetic-folder")
    );
    server.verify().await;
}

#[test]
fn status_outside_a_checkout_requires_a_project_code() {
    Command::cargo_bin("navigator")
        .unwrap()
        .current_dir(TempDir::new().unwrap().path())
        .args(["project", "status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Project code"));
}

/// Run the public command against each Project's authorized billing response.
async fn fleet_status(reports: &[(&str, ResponseTemplate)], json: bool) -> std::process::Output {
    use serde_json::json;
    let server = MockServer::start().await;
    let mut projects = Vec::new();
    let get = |route: String| {
        Mock::given(method("GET"))
            .and(path(route))
            .and(header("authorization", "Bearer fixture-token"))
    };
    for (code, billing) in reports {
        let id = Uuid::new_v4();
        projects.push(json!({"id": id, "code": code, "name": code, "status": "open"}));
        let route = format!("/app/api/projects/{id}");
        for (suffix, response) in [
            ("billing", billing.clone()),
            (
                "status",
                ResponseTemplate::new(200).set_body_json(json!({})),
            ),
            (
                "deadlines",
                ResponseTemplate::new(200).set_body_json(json!([])),
            ),
        ] {
            get(format!("{route}/{suffix}"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
        }
    }
    get("/app/api/lawyer/deadline-projects".into())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"projects": projects})))
        .mount(&server)
        .await;
    get("/app/api/onboarding-agreements".into())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/api/calendars/access"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"project_calendar":{"readable":true}})),
        )
        .mount(&server)
        .await;

    let credentials = tempfile::NamedTempFile::new().unwrap();
    let host = server.uri();
    std::fs::write(credentials.path(), json!({"hosts": {&host: {
        "token": "fixture-token", "person_email": "lawyer@example.com", "role": "lawyer", "expires_at": i64::MAX
    }}}).to_string()).unwrap();
    let mut command = Command::cargo_bin("navigator").unwrap();
    command
        .env("NAVIGATOR_CREDENTIALS_FILE", credentials.path())
        .args(["project", "status", "--all", "--host", &host]);
    if json {
        command.arg("--json");
    }
    command.output().unwrap()
}

#[tokio::test]
async fn fleet_lists_daily_billing_exceptions_once_per_project_and_identity() {
    use serde_json::json;
    let id = Uuid::new_v4();
    let claim = |project: &str, amount| {
        json!({
            "project": project, "id": id, "benefit": "Synthetic consultation",
            "performed_on": "2026-01-03", "status": "approved", "next_action": null,
            "approved_outstanding_cents": amount, "net_cash_received_cents": 0
        })
    };
    let time = |project: &str| {
        json!({
            "project": project, "id": id, "performed_on": "2026-01-02",
            "minutes": 12, "description": "synthetic-change", "status": "pending_review"
        })
    };
    let report = |project: &str| {
        json!({
            "needs_my_action": [], "my_time": [], "project_time": [time(project)],
            "verified_projected_fees": [], "unverified_fees": [], "contingency": [],
            "other_claims": [claim(project, 3075)]
        })
    };
    let first = "sample-alpha";
    let second = "sample-beta";
    let mut alpha = report(first);
    let mut actionable = claim(first, 3075);
    actionable["next_action"] = json!("Check payment");
    alpha["needs_my_action"] = json!([
        {"project": first, "annotation_id": id, "change_id": "synthetic-change",
         "performed_on": "2026-01-02", "next_action": "Review staged time"},
        {"project": first, "next_action": "Review completed time for a claim"}, actionable
    ]);
    let mut beta = report(second);
    let mut cleared = claim(second, 0);
    cleared["id"] = json!(Uuid::new_v4());
    cleared["benefit"] = json!("Settled consultation");
    beta["other_claims"].as_array_mut().unwrap().push(cleared);
    let ok = |body| ResponseTemplate::new(200).set_body_json(body);
    let result = fleet_status(&[(first, ok(alpha)), (second, ok(beta))], false).await;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = String::from_utf8(result.stdout).unwrap();
    let (_, list) = text
        .split_once("Daily billing exceptions\n")
        .expect("billing worklist");
    let rows: Vec<_> = list
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(rows.len(), 5, "{list}");
    for project in [first, second] {
        assert!(rows.iter().any(|r| r.contains(project)
            && r.contains("Pending time")
            && r.contains("2026-01-02")));
        assert!(rows
            .iter()
            .any(|r| r.contains(project) && r.contains("$30.75") && r.contains("2026-01-03")));
    }
    for action in [
        "Review staged time",
        "Review completed time for a claim",
        "Check payment",
    ] {
        assert!(rows.iter().any(|r| r.contains(first) && r.contains(action)));
    }
    assert!(!list.contains("Settled consultation"));
}

#[tokio::test]
async fn fleet_reports_empty_billing_and_distinguishes_a_failed_read() {
    let empty = serde_json::to_value(model::billing_report::BillingReport::default()).unwrap();
    let ok = ResponseTemplate::new(200).set_body_json(&empty);
    let mut reports = vec![("sample-clear", ok)];
    let complete = fleet_status(&reports, false).await;
    assert!(complete.status.success());
    assert!(String::from_utf8(complete.stdout)
        .unwrap()
        .contains("Daily billing exceptions\n  None"));
    reports.push(("sample-denied", ResponseTemplate::new(403)));
    let partial = fleet_status(&reports, false).await;
    assert_eq!(partial.status.code(), Some(1));
    let text = String::from_utf8(partial.stdout).unwrap();
    let (_, list) = text.split_once("Daily billing exceptions\n").unwrap();
    assert!(list
        .lines()
        .any(|r| r.contains("sample-denied") && r.contains("unavailable")));
    assert!(!list.contains("  None"));
    let json = fleet_status(&reports, true).await;
    assert_eq!(json.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["complete"], false);
    assert_eq!(report["blockers"]["billing read failed"], 1);
    assert_eq!(report["projects"][0]["billing"], empty);
    assert!(report["projects"][1]["billing"]["error"]
        .as_str()
        .unwrap()
        .contains("403"));
    assert!(report.get("billing_exceptions").is_none());
}
