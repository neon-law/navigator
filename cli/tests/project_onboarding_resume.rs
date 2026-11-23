//! Resume the published onboarding letter through the real CLI.

use std::{fs, path::Path, process::Command};

use uuid::Uuid;
use wiremock::matchers::{body_string, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn publish_project_fixture(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    fs::write(
        root.join("navigator.yaml"),
        "project:\n  host: navigator.example.test\n  name: example\n  client_dri: client@example.test\n",
    )
    .unwrap();
    fs::create_dir(root.join("drafts")).unwrap();
    fs::write(
        root.join("drafts/onboarding__letter.md"),
        "---\nkind: onboarding\ntitle: Engagement letter\n---\nAn authored engagement letter.\n",
    )
    .unwrap();
    git(root, &["add", "navigator.yaml", "drafts"]);
    git(
        root,
        &[
            "-c",
            "user.name=Test Contributor",
            "-c",
            "user.email=contributor@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Add engagement letter",
        ],
    );
    git(root, &["remote", "add", "origin", "."]);
}

#[tokio::test(flavor = "multi_thread")]
async fn onboarding_fills_the_existing_notation_without_opening_another() {
    let server = MockServer::start().await;
    let host = server.uri();
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    publish_project_fixture(root);

    let credentials = root.join("navigator.json");
    fs::write(
        &credentials,
        serde_json::to_vec(&serde_json::json!({
            "hosts": { &host: { "token": "test-token", "role": "lawyer", "expires_at": i64::MAX } }
        }))
        .unwrap(),
    )
    .unwrap();
    let answers = root.join("answers.yaml");
    fs::write(&answers, "custom_single_choice__governing_law: nevada\n").unwrap();

    let project_id = Uuid::new_v4();
    let notation_id = Uuid::new_v4();
    let step = format!("/app/lawyer/notations/{notation_id}/step");
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "example"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/notation-inventory"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "template_code": "onboarding__letter",
                "notation_id": notation_id,
                "current_state": "BEGIN",
                "workflow_states": ["BEGIN", "lawyer_review"],
                "state_entered_at": {}
            }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(step.clone()))
        .and(query_param("format", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "question": {"code": "custom_single_choice__governing_law", "prompt": "Which law governs?", "answer_type": "single_choice"}
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(step.clone()))
        .and(body_string(
            "value=nevada&_nav_question_code=custom_single_choice__governing_law",
        ))
        .respond_with(ResponseTemplate::new(303))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(step))
        .and(query_param("format", "json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"question": null})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/projects/example/notations/new"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let output = Command::new(env!("CARGO_BIN_EXE_navigator"))
        .args(["project", "onboard", "--host", &host, "--answers-from"])
        .arg(&answers)
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 questions prefilled"));
}
