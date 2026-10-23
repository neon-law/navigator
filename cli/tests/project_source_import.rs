//! `navigator project import-source` and `navigator project scaffold
//! --resume/--no-repository` as processes against a fake deployment. The
//! stored login is process-global configuration, so these run the binary
//! rather than mutating the test process's environment.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CODE: &str = "sample-import";

fn credentials(dir: &TempDir, host: &str, person_email: Option<&str>) -> std::path::PathBuf {
    let path = dir.path().join("navigator.json");
    fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "hosts": {
                host: {
                    "token": "test-token",
                    "person_email": person_email,
                    "role": "admin",
                    "expires_at": i64::MAX
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn command(credentials: &Path) -> Command {
    let mut command = Command::cargo_bin("navigator").unwrap();
    command.env_remove("GITHUB_REPOSITORY");
    command.env_remove("GITHUB_TOKEN");
    command.env_remove("NAVIGATOR_GITHUB_TOKEN");
    command.env("NAVIGATOR_CREDENTIALS_FILE", credentials);
    command
}

fn git(dir: &Path, args: &[&str]) {
    assert!(std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success());
}

fn checkout() -> TempDir {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init", "--quiet"]);
    fs::write(dir.path().join("navigator.yaml"), "version: 1\n").unwrap();
    fs::write(dir.path().join("README.md"), "# Synthetic matter\n").unwrap();
    dir
}

async fn mount_projects(server: &MockServer, project_id: Uuid) {
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": project_id, "code": CODE }
        ])))
        .mount(server)
        .await;
}

async fn mount_import(server: &MockServer, project_id: Uuid, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/api/projects/{project_id}/source-imports"
        )))
        .and(header("authorization", "Bearer test-token"))
        .and(header("content-type", "application/zip"))
        .respond_with(response)
        .mount(server)
        .await;
}

struct Run {
    success: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(command: &mut Command) -> Run {
    let output = command.output().unwrap();
    Run {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn import(credentials: &Path, host: &str, dir: &Path, extra: &[&str]) -> Run {
    run(command(credentials)
        .args(["project", "import-source", CODE, "--host", host, "--dir"])
        .arg(dir)
        .args(extra))
}

#[tokio::test(flavor = "multi_thread")]
async fn import_source_uploads_the_canonical_zip_under_a_tree_derived_key() {
    let server = MockServer::start().await;
    let host = server.uri();
    let login = TempDir::new().unwrap();
    let credentials = credentials(&login, &host, Some("lawyer@example.com"));
    let project_id = Uuid::now_v7();
    mount_projects(&server, project_id).await;
    mount_import(
        &server,
        project_id,
        ResponseTemplate::new(202).set_body_json(serde_json::json!({"id": "import-1"})),
    )
    .await;
    let checkout = checkout();

    let first = import(&credentials, &host, checkout.path(), &[]);
    assert!(first.success, "{}", first.stderr);
    assert!(
        first
            .stdout
            .contains("Source import import-1 recorded (2 files"),
        "{}",
        first.stdout
    );
    let second = import(&credentials, &host, checkout.path(), &[]);
    assert!(second.success, "{}", second.stderr);

    let keys: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .map(operation_key)
        .collect();
    assert_eq!(keys.len(), 2);
    assert!(keys[0].starts_with(&format!("{CODE}-")), "{keys:?}");
    assert_eq!(
        keys[0], keys[1],
        "rerunning the same tree joins the same import"
    );
}

fn operation_key(request: &Request) -> String {
    request
        .url
        .query_pairs()
        .find(|(key, _)| key == "operation_key")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn import_source_waits_for_the_commit_under_an_explicit_key() {
    let server = MockServer::start().await;
    let host = server.uri();
    let login = TempDir::new().unwrap();
    let credentials = credentials(&login, &host, Some("lawyer@example.com"));
    let project_id = Uuid::now_v7();
    mount_projects(&server, project_id).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/api/projects/{project_id}/source-imports"
        )))
        .and(query_param("operation_key", "retry-7"))
        .respond_with(
            ResponseTemplate::new(202).set_body_json(serde_json::json!({"id": "import-7"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/source-imports/import-7"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "state": "committed",
            "commit_sha": "0123abcd",
            "repository_url": "https://forge.example.test/firm/sample-import"
        })))
        .mount(&server)
        .await;
    let checkout = checkout();

    let waited = import(
        &credentials,
        &host,
        checkout.path(),
        &["--operation-key", "retry-7", "--wait"],
    );
    assert!(waited.success, "{}", waited.stderr);
    assert!(
        waited
            .stdout
            .contains("Committed 0123abcd to https://forge.example.test/firm/sample-import."),
        "{}",
        waited.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn import_source_surfaces_a_refusal_found_while_waiting() {
    let server = MockServer::start().await;
    let host = server.uri();
    let login = TempDir::new().unwrap();
    let credentials = credentials(&login, &host, Some("lawyer@example.com"));
    let project_id = Uuid::now_v7();
    mount_projects(&server, project_id).await;
    mount_import(
        &server,
        project_id,
        ResponseTemplate::new(202).set_body_json(serde_json::json!({"id": "import-9"})),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{project_id}/source-imports/import-9"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "state": "refused",
            "last_error": "repository already has commits"
        })))
        .mount(&server)
        .await;
    let checkout = checkout();

    let refused = import(&credentials, &host, checkout.path(), &["--wait"]);
    assert_eq!(refused.code, Some(2));
    assert!(
        refused
            .stderr
            .contains("source import refused: repository already has commits"),
        "{}",
        refused.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn import_source_treats_a_github_forge_and_an_initialized_source_as_done() {
    for (error, expected) in [
        ("source_import_requires_forgejo", "forge is GitHub"),
        ("source_already_initialized", "already initialized"),
    ] {
        let server = MockServer::start().await;
        let host = server.uri();
        let login = TempDir::new().unwrap();
        let credentials = credentials(&login, &host, Some("lawyer@example.com"));
        let project_id = Uuid::now_v7();
        mount_projects(&server, project_id).await;
        mount_import(
            &server,
            project_id,
            ResponseTemplate::new(409).set_body_json(serde_json::json!({"error": error})),
        )
        .await;
        let checkout = checkout();

        let outcome = import(&credentials, &host, checkout.path(), &["--wait"]);
        assert!(outcome.success, "{error}: {}", outcome.stderr);
        assert!(outcome.stdout.contains(expected), "{}", outcome.stdout);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn import_source_quotes_the_servers_refusal_and_exits_two() {
    let server = MockServer::start().await;
    let host = server.uri();
    let login = TempDir::new().unwrap();
    let credentials = credentials(&login, &host, Some("lawyer@example.com"));
    let project_id = Uuid::now_v7();
    mount_projects(&server, project_id).await;
    mount_import(
        &server,
        project_id,
        ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "error": "invalid_source",
            "detail": "documents/ holds legal bytes"
        })),
    )
    .await;
    let checkout = checkout();

    let refused = import(&credentials, &host, checkout.path(), &[]);
    assert_eq!(refused.code, Some(2));
    assert!(
        refused.stderr.contains("documents/ holds legal bytes"),
        "{}",
        refused.stderr
    );
}

#[test]
fn import_source_refuses_a_directory_that_is_not_a_checkout_before_any_request() {
    let login = TempDir::new().unwrap();
    // No deployment answers here; the refusal must come first.
    let credentials = credentials(&login, "http://127.0.0.1:9", None);
    let plain = TempDir::new().unwrap();
    fs::write(plain.path().join("navigator.yaml"), "version: 1\n").unwrap();

    let refused = import(&credentials, "http://127.0.0.1:9", plain.path(), &[]);
    assert_eq!(refused.code, Some(2));
    assert!(
        refused.stderr.contains("is not a Git checkout"),
        "{}",
        refused.stderr
    );
}

fn scaffold(credentials: &Path, host: &str, dir: &Path, extra: &[&str]) -> Run {
    run(command(credentials)
        .args(["project", "scaffold", "--host", host, "--dir"])
        .arg(dir)
        .args(extra))
}

/// Resume an interrupted open into an empty directory: the tree is rebuilt
/// with the given client DRI and no repository is requested.
fn prepare_scaffold(credentials: &Path, host: &str, dir: &Path) {
    let missing = scaffold(
        credentials,
        host,
        dir,
        &["--resume", CODE, "--no-repository"],
    );
    assert_eq!(missing.code, Some(2));
    assert!(
        missing.stderr.contains("--client-email"),
        "{}",
        missing.stderr
    );
    let rebuilt = scaffold(
        credentials,
        host,
        dir,
        &[
            "--resume",
            CODE,
            "--no-repository",
            "--client-email",
            "client@example.com",
        ],
    );
    assert!(rebuilt.success, "{}", rebuilt.stderr);
    let manifest = fs::read_to_string(dir.join("navigator.yaml")).unwrap();
    assert!(
        manifest.contains("client_dri: client@example.com"),
        "{manifest}"
    );
}

async fn mount_reconcile(server: &MockServer, project_id: Uuid, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(format!("/app/api/project-surfaces/{project_id}")))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(response)
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn resume_writes_a_repository_less_tree_then_provisions_and_imports_it() {
    let server = MockServer::start().await;
    let host = server.uri();
    let login = TempDir::new().unwrap();
    let credentials = credentials(&login, &host, Some("lawyer@example.com"));
    let project_id = Uuid::now_v7();
    mount_projects(&server, project_id).await;
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("sample-import");

    // An interrupted open, resumed without a repository: the tree is written
    // with the lawyer from the login, and no provisioning door is called.
    prepare_scaffold(&credentials, &host, &dir);
    let local = scaffold(
        &credentials,
        &host,
        &dir,
        &["--resume", CODE, "--no-repository"],
    );
    assert!(local.success, "{}", local.stderr);
    assert!(
        local.stdout.contains("stays repository-less"),
        "{}",
        local.stdout
    );
    let manifest = fs::read_to_string(dir.join("navigator.yaml")).unwrap();
    assert!(manifest.contains(&format!("name: {CODE}")), "{manifest}");
    assert!(
        manifest.contains("lawyer_dri: lawyer@example.com"),
        "{manifest}"
    );
    assert!(dir.join(".git").exists());
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|request| request.method.as_str() == "GET"));

    // Resuming in the existing scaffold repairs nothing, then provisions the
    // repository and sends the tree as the first commit.
    mount_reconcile(
        &server,
        project_id,
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": CODE,
            "documents_prefix": format!("projects/{CODE}/documents"),
            "drive_folder_id": null,
            "drive_status": "skipped",
            "drive_detail": "Drive is disabled on this deployment",
            "repository_url": "https://forge.example.test/firm/sample-import",
            "repository_status": "created",
            "repository_detail": "created on the firm forge"
        })),
    )
    .await;
    mount_import(
        &server,
        project_id,
        ResponseTemplate::new(202).set_body_json(serde_json::json!({"id": "import-3"})),
    )
    .await;
    let provisioned = scaffold(&credentials, &host, &dir, &["--resume", CODE]);
    assert!(provisioned.success, "{}", provisioned.stderr);
    for expected in [
        "repository: Created (https://forge.example.test/firm/sample-import)",
        "repository detail: created on the firm forge",
        "drive: Skipped",
        "Source import import-3 recorded",
    ] {
        assert!(
            provisioned.stdout.contains(expected),
            "missing {expected:?} in {}",
            provisioned.stdout
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_reports_a_provisioning_failure_with_the_retry_command() {
    let server = MockServer::start().await;
    let host = server.uri();
    let login = TempDir::new().unwrap();
    let credentials = credentials(&login, &host, Some("lawyer@example.com"));
    let project_id = Uuid::now_v7();
    mount_projects(&server, project_id).await;
    mount_reconcile(
        &server,
        project_id,
        ResponseTemplate::new(403).set_body_string("This endpoint requires the admin tier."),
    )
    .await;
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("sample-import");
    prepare_scaffold(&credentials, &host, &dir);

    let failed = scaffold(&credentials, &host, &dir, &["--resume", CODE]);
    assert_eq!(failed.code, Some(2));
    assert!(
        failed
            .stderr
            .contains(&format!("navigator project scaffold --resume {CODE}")),
        "{}",
        failed.stderr
    );
    assert!(
        dir.join("navigator.yaml").exists(),
        "the tree survives a failed provision"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_refuses_a_project_the_deployment_does_not_list() {
    let server = MockServer::start().await;
    let host = server.uri();
    let login = TempDir::new().unwrap();
    let credentials = credentials(&login, &host, Some("lawyer@example.com"));
    mount_projects(&server, Uuid::now_v7()).await;
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("other");

    let refused = scaffold(
        &credentials,
        &host,
        &dir,
        &[
            "--resume",
            "other-matter",
            "--client-email",
            "client@example.com",
        ],
    );
    assert_eq!(refused.code, Some(2));
    assert!(
        refused.stderr.contains("is not open on this deployment"),
        "{}",
        refused.stderr
    );
    assert!(!dir.exists(), "nothing is written for an unknown Project");

    let invalid = scaffold(&credentials, &host, &dir, &["--resume", "Not A Code"]);
    assert_eq!(invalid.code, Some(2));
    assert!(
        invalid.stderr.contains("invalid immutable Project code"),
        "{}",
        invalid.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_needs_a_login_that_names_the_lawyer() {
    let server = MockServer::start().await;
    let host = server.uri();
    let login = TempDir::new().unwrap();
    let credentials = credentials(&login, &host, None);
    mount_projects(&server, Uuid::now_v7()).await;
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("sample-import");

    let refused = scaffold(
        &credentials,
        &host,
        &dir,
        &[
            "--resume",
            CODE,
            "--no-repository",
            "--client-email",
            "client@example.com",
        ],
    );
    assert_eq!(refused.code, Some(2));
    assert!(
        refused.stderr.contains("no lawyer identity"),
        "{}",
        refused.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_scaffold_cancels_without_attestation_and_refuses_a_taken_code() {
    let server = MockServer::start().await;
    let host = server.uri();
    let login = TempDir::new().unwrap();
    let credentials = credentials(&login, &host, Some("lawyer@example.com"));
    mount_projects(&server, Uuid::now_v7()).await;
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("fresh");

    let cancelled = run(command(&credentials)
        .args(["project", "scaffold", "--host", &host, "--dir"])
        .arg(&dir)
        .write_stdin(
            "Synthetic Matter\nfresh-matter\nPat Example\npat@example.com\n\nNevada\nno\n",
        ));
    assert!(cancelled.success, "{}", cancelled.stderr);
    assert!(
        cancelled.stderr.contains("cancelled before writes"),
        "{}",
        cancelled.stderr
    );
    assert!(!dir.exists());

    let taken = run(command(&credentials)
        .args(["project", "scaffold", "--host", &host, "--dir"])
        .arg(&dir)
        .write_stdin(format!(
            "Synthetic Matter\n{CODE}\nPat Example\npat@example.com\nExample LLC\nyes\n"
        )));
    assert_eq!(taken.code, Some(2));
    assert!(
        taken.stderr.contains(&format!("use --resume {CODE}")),
        "{}",
        taken.stderr
    );
    assert!(!dir.exists());
}
