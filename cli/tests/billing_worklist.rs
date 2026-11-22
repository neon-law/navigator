//! The lawyer's personal billing report remains personal inside a Project checkout.

use assert_cmd::Command;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn personal_worklist_inside_checkout_and_project_report_use_distinct_scopes() {
    let server = MockServer::start().await;
    let host = server.uri();
    let checkout = TempDir::new().unwrap();
    let project_id = uuid::Uuid::new_v4();
    std::fs::write(
        checkout.path().join("navigator.yaml"),
        format!("project:\n  host: other-site.invalid\n  name: {project_id}\n"),
    )
    .unwrap();
    let credentials = checkout.path().join("credentials.json");
    std::fs::write(
        &credentials,
        json!({"hosts": {&host: {"token": "test-token", "expires_at": i64::MAX}}}).to_string(),
    )
    .unwrap();

    let mut report = json!({
        "needs_my_action": [{
            "project": "sample-litigation", "annotation_id": uuid::Uuid::new_v4(),
            "change_id": "synthetic-change", "performed_on": "2026-11-20",
            "next_action": "Review your staged Jujutsu time on the Project's Billing tab"
        }], "my_time": [], "verified_projected_fees": [], "unverified_fees": [],
        "other_claims": [], "contingency": [{
            "project": "sample-litigation", "fee_agreement_notation_id": uuid::Uuid::new_v4(),
            "effective_on": "2026-01-01", "costs_deducted": "before_fee", "recovery_basis": "no_estimate",
            "tiers": [{"stage": "Trial", "fee_bps": 3333}], "stale_estimate": false, "outcomes": []
        }]
    });
    Mock::given(method("GET"))
        .and(path("/app/api/billing"))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&report))
        .expect(1)
        .mount(&server)
        .await;
    report["project_time"] = json!([]);
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/billing")))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&report))
        .expect(1)
        .mount(&server)
        .await;

    let pages = checkout.path().join("pages");
    std::fs::create_dir(&pages).unwrap();
    let run = |args: &[&str]| {
        let output = Command::cargo_bin("navigator")
            .unwrap()
            .args(args)
            .current_dir(&pages)
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let personal = run(&["billing"]);
    assert!(personal.starts_with("Needs my action"));
    assert!(personal.find("synthetic-change").unwrap() < personal.find("My time").unwrap());
    assert!(personal.find("My time").unwrap() < personal.find("Contingency fees").unwrap());
    assert!(personal.contains("Trial") && personal.contains("33.33%"));
    let project = run(&["project", "billing", "--host", &host, "--json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&project).unwrap()["project_time"],
        json!([])
    );
    server.verify().await;
}
