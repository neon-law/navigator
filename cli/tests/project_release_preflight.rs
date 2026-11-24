use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;

fn git(dir: &Path, args: &[&str]) -> String {
    cli::projects::lifecycle::git_text(dir, args).unwrap()
}

#[test]
fn preflight_names_the_commit_and_ignores_working_copy_repairs() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-b", "main"]);
    std::fs::write(
        dir.join("navigator.yaml"),
        format!(
            "version: '{}'\nproject: sample\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    std::fs::write(dir.join("LICENSE"), "Sample\n").unwrap();
    std::fs::write(
        dir.join("README.md"),
        "# Sample\n\nSee [terms](LICENSE).   \n",
    )
    .unwrap();
    git(dir, &["add", "."]);
    let commit = || {
        git(
            dir,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "sample",
            ],
        )
    };
    commit();
    git(dir, &["rm", "LICENSE"]);
    commit();
    let sha = git(dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("README.md"), "# Unrelated staged change\n").unwrap();
    git(dir, &["add", "README.md"]);
    let before = git(dir, &["diff", "--cached"]);
    let run = || {
        let result = Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(dir)
            .env("GIT_INDEX_FILE", dir.join(".git/index"))
            .env("NAVIGATOR_RELEASE_TAG", env!("CARGO_PKG_VERSION"))
            .args(["release", "preflight", "--revision", &sha])
            .assert()
            .failure();
        serde_json::from_slice::<Value>(&result.get_output().stdout).unwrap()
    };
    let report = run();
    assert_eq!(report["revision"], sha);
    assert_eq!(report["repository"]["status"], "failed");
    assert!(report["repository"]["diagnostics"]
        .to_string()
        .contains("LICENSE"));
    assert!(report["local_repairs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path == "README.md"));
    assert_eq!(report["live_documents"]["status"], "unavailable");
    assert_eq!(run(), report);
    assert_eq!(git(dir, &["diff", "--cached"]), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preflight_reports_the_live_pointer_missing_from_the_commit() {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let project = uuid::Uuid::now_v7();
    let asset = uuid::Uuid::now_v7();
    let slug = "filings/notice.pdf";
    for (route, response) in [
        (
            "/app/api/projects".to_string(),
            json!([{"id":project, "code":"sample"}]),
        ),
        (
            format!("/app/api/projects/{project}/documents/integrity"),
            json!({"assets":[{
                "asset_id":asset,"slug":slug,"exists":true,"recorded_size":18,"size_bytes":18
            }]}),
        ),
        (
            format!("/app/api/projects/{project}/documents/revisions"),
            json!({"kind":"filing","revisions":[{
                "version":1,"asset_id":asset,"created_at":"2026-09-05T12:00:00Z","sha256":"a".repeat(64),
                "size_bytes":18,"filename":"notice.pdf","visibility":"internal","operative":true
            }]}),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .mount(&server)
            .await;
    }
    let scratch = tempfile::tempdir().unwrap();
    let repo = scratch.path().join("sample");
    std::fs::create_dir(&repo).unwrap();
    let credentials = scratch.path().join("credentials.json");
    std::fs::write(
        &credentials,
        json!({"hosts":{(server.uri()):{"token":"test-token","expires_at":i64::MAX}}}).to_string(),
    )
    .unwrap();
    git(&repo, &["init", "-b", "main"]);
    std::fs::write(
        repo.join("navigator.yaml"),
        format!(
            "version: '{}'\nproject:\n  name: sample\n  host: {}\n",
            env!("CARGO_PKG_VERSION"),
            server.uri()
        ),
    )
    .unwrap();
    git(&repo, &["add", "navigator.yaml"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "Sample",
        ],
    );
    let run = || {
        let result = Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(&repo)
            .env("NAVIGATOR_RELEASE_TAG", env!("CARGO_PKG_VERSION"))
            .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
            .args(["release", "preflight"])
            .assert()
            .failure();
        serde_json::from_slice::<Value>(&result.get_output().stdout).unwrap()
    };
    let report = run();
    assert_eq!(
        report["live_documents"]["repairs"][0]["path"], "blobs/filings/notice.pdf.yaml",
        "{report}"
    );
    assert_eq!(report, run());
    assert!(git(&repo, &["status", "--porcelain"]).is_empty());
    assert!(!repo.join("blobs").exists());
}
