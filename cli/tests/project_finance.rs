//! `navigator project finance` starts and reads the finance aggregate run through
//! the authenticated, matter-scoped API. It never writes the run's input.
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Fixture {
    server: MockServer,
    root: TempDir,
    credentials: std::path::PathBuf,
    id: Uuid,
}

async fn fixture() -> Fixture {
    let server = MockServer::start().await;
    let root = TempDir::new().unwrap();
    let host = server.uri();
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
            {"id": id, "code": "sample-finance"}
        ])))
        .mount(&server)
        .await;
    Fixture {
        server,
        root,
        credentials,
        id,
    }
}

impl Fixture {
    fn navigator(&self, args: &[&str]) -> Command {
        let mut command = Command::cargo_bin("navigator").unwrap();
        command
            .current_dir(self.root.path())
            .env("NAVIGATOR_CREDENTIALS_FILE", &self.credentials)
            .args(["project", "finance"])
            .args(args)
            .args(["sample-finance", "--host", &self.server.uri()]);
        command
    }
}

#[tokio::test]
async fn run_posts_with_an_explicit_zero_content_length_and_prints_the_run_id() {
    let f = fixture().await;
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{}/finance/run", f.id)))
        .and(header("authorization", "Bearer test-token"))
        .and(header("content-length", "0"))
        .respond_with(
            ResponseTemplate::new(202).set_body_json(serde_json::json!({"run_id": "manual-abc"})),
        )
        .expect(1)
        .mount(&f.server)
        .await;
    f.navigator(&["run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("manual-abc"));
    f.server.verify().await;
}

#[tokio::test]
async fn run_reports_a_deployment_without_workflows() {
    let f = fixture().await;
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{}/finance/run", f.id)))
        .respond_with(ResponseTemplate::new(503))
        .mount(&f.server)
        .await;
    f.navigator(&["run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("503"));
}

#[tokio::test]
async fn show_emits_the_stored_report_as_json() {
    let f = fixture().await;
    let report = serde_json::json!({"source": "workflow", "run_id": "manual-abc",
        "computed_at": "2026-10-01T00:00:00Z", "totals": {"matters": 3, "billed_cents": 900}});
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{}/finance", f.id)))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&report))
        .expect(1)
        .mount(&f.server)
        .await;
    let out = f
        .navigator(&["show", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out).unwrap(),
        report
    );
}

#[tokio::test]
async fn show_prints_a_readable_summary_without_json() {
    let f = fixture().await;
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{}/finance", f.id)))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"source": "workflow", "run_id": "manual-abc",
                "computed_at": "2026-10-01T00:00:00Z",
                "totals": {"matters": 3, "billed_cents": 900}}),
        ))
        .mount(&f.server)
        .await;
    f.navigator(&["show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("run_id: manual-abc"))
        .stdout(predicate::str::contains("billed_cents: 900"));
}

#[tokio::test]
async fn show_says_no_run_is_stored_on_a_404() {
    let f = fixture().await;
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{}/finance", f.id)))
        .respond_with(ResponseTemplate::new(404))
        .mount(&f.server)
        .await;
    f.navigator(&["show"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no run stored yet"));
}
