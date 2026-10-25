//! `navigator project docket ingest`, run as the binary against synthetic
//! CM/ECF notices. The free-look link and the deployment are both a local
//! mock server, so no test reaches a court or a real deployment.

use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nef")
        .join(name)
}

/// A Project checkout: a Git work tree with a `navigator.yaml`.
fn checkout(manifest: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("navigator.yaml"), manifest).unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(init.success());
    dir
}

/// A credentials file logged in to `base`.
fn credentials(dir: &Path, base: &str) -> PathBuf {
    let path = dir.join("navigator.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "hosts": {
                base: {
                    "token": "test-token",
                    "person_email": "lawyer@neonlaw.com",
                    "role": "lawyer",
                    "expires_at": i64::MAX,
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn ingest(cwd: &Path, credentials: &Path, args: &[&str]) -> Output {
    std::process::Command::new(cargo_bin("navigator"))
        .args(["project", "docket", "ingest"])
        .args(args)
        .current_dir(cwd)
        .env("NAVIGATOR_CREDENTIALS_FILE", credentials)
        .output()
        .expect("run navigator project docket ingest")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_dry_run_prints_the_plan_and_writes_nothing() {
    let dir = checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 9:26-cv-00042\n");
    let creds = tempfile::tempdir().unwrap();
    let credentials = creds.path().join("absent.json");

    let output = ingest(
        dir.path(),
        &credentials,
        &[
            fixture("district-motion.eml").to_str().unwrap(),
            fixture("bankruptcy-transfer.eml").to_str().unwrap(),
            "--dry-run",
        ],
    );

    assert!(output.status.success(), "{output:?}");
    let out = stdout(&output);
    assert!(
        out.contains(
            "D.I. 12 · pleading · DKT 0012 - MOTION to Dismiss for Failure to State a Claim by Defendant"
        ),
        "{out}"
    );
    assert!(out.contains("D.I. 12-1 · no free-look link"), "{out}");
    assert!(out.contains("not a docket of this Project"), "{out}");
    assert!(
        !out.contains("26-10042"),
        "another docket's case number never reaches the output: {out}"
    );
    assert!(
        out.contains("service list: 3 person(s), 3 relationship log row(s)"),
        "{out}"
    );
    assert!(
        out.contains("gus.gavel@example.com · Gus Gavel is on the service list in 9:26-cv-00042-XYZ, per D.I. 12"),
        "{out}"
    );
    assert!(out.contains("nothing was downloaded"), "{out}");
    assert!(!dir.path().join("documents").exists());
}

/// A notice whose free-look link points at `base`, the mock court.
fn local_notice(dir: &Path, base: &str) -> PathBuf {
    let template = std::fs::read_to_string(fixture("bankruptcy-transfer.eml")).unwrap();
    let notice = template.replace("https://ecf.example.com", base);
    let path = dir.join("local-transfer.eml");
    std::fs::write(&path, notice).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn apply_mode_downloads_once_files_the_document_and_stages_its_pointer() {
    let server = MockServer::start().await;
    let project_id = uuid::Uuid::now_v7();
    Mock::given(method("GET"))
        .and(path("/doc1/0310009"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.7 synthetic".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "kind": "pleading",
            "visibility": "internal",
            "current_version": {
                "version": 1,
                "asset_id": uuid::Uuid::now_v7(),
                "created_at": "2026-03-05T12:00:00Z",
                "sha256": "0".repeat(64),
                "size_bytes": 18
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/app/api/seed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "person", "created": 3, "updated": 0, "unchanged": 0, "records": []
        })))
        .mount(&server)
        .await;
    // One docket entry per notice, through the docket door; a re-run reads
    // the same entry number back rather than duplicating it.
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{project_id}/docket")))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": uuid::Uuid::now_v7(),
            "entry_number": "29",
            "kind": "other"
        })))
        .expect(2)
        .mount(&server)
        .await;

    let dir = checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 26-10042\n");
    let creds = tempfile::tempdir().unwrap();
    let credentials = credentials(creds.path(), &server.uri());
    let notice = local_notice(creds.path(), &server.uri());
    let args = [notice.to_str().unwrap(), "--host", &server.uri()];

    let first = tokio::task::spawn_blocking({
        let cwd = dir.path().to_path_buf();
        let credentials = credentials.clone();
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            ingest(&cwd, &credentials, &args)
        }
    })
    .await
    .unwrap();
    assert!(first.status.success(), "{first:?}");
    let filed = "DKT 0029 - Transfer of Claim. Transfer Agreement 3001 (e) 2 Transferor- Acme Lending Co (Claim No. 7) To Harbor Recovery Fund. File.pdf";
    let bytes = dir.path().join("documents/Docket Filings").join(filed);
    assert_eq!(std::fs::read(&bytes).unwrap(), b"%PDF-1.7 synthetic");
    assert!(dir.path().join(format!("blobs/{filed}.yaml")).is_file());
    assert!(!dir.path().join("documents/.gitignore").exists());
    assert!(std::fs::read_to_string(dir.path().join(".gitignore"))
        .unwrap()
        .lines()
        .any(|line| line == "/documents/"));

    let second = tokio::task::spawn_blocking({
        let cwd = dir.path().to_path_buf();
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            ingest(&cwd, &credentials, &args)
        }
    })
    .await
    .unwrap();
    assert!(second.status.success(), "{second:?}");

    let seeds: Vec<serde_json::Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/app/api/seed")
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert!(!seeds.is_empty(), "the service list is persisted");
    let entries: Vec<serde_json::Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/docket"))
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(entries.len(), 2, "one docket entry per notice per run");
    assert_eq!(entries[0]["entry_number"], "29");
    assert_eq!(entries[0]["slug"], filed);
    assert!(
        stdout(&first).contains("recorded docket entry D.I. 29"),
        "{first:?}"
    );
    assert!(
        seeds
            .iter()
            .all(|seed| seed["model"] == "person" || seed["model"] == "relationship_log"),
        "a service-list person never gets a person_project_role: {seeds:?}"
    );
    assert!(seeds.iter().all(|seed| seed["overwrite"] == false));
    assert!(
        stdout(&second).contains(&format!("already filed as {filed}")),
        "{second:?}"
    );
}

/// LAW-133: the four docket verbs print the row the portal stored, and a
/// `--dry-run` write prints its request and sends nothing.
mod verbs {
    use assert_cmd::Command;
    use serde_json::{json, Value};
    use tempfile::TempDir;
    use uuid::Uuid;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn run(
        root: &std::path::Path,
        credentials: &std::path::Path,
        host: &str,
        args: &[&str],
    ) -> Value {
        let mut full = args.to_vec();
        full.extend(["--project", "acme", "--host", host]);
        let stdout = Command::cargo_bin("navigator")
            .unwrap()
            .current_dir(root)
            .env("NAVIGATOR_CREDENTIALS_FILE", credentials)
            .args(full)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&stdout).unwrap()
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn list_add_link_and_supersede_print_the_stored_row_and_dry_run_sends_nothing() {
        let server = MockServer::start().await;
        let root = TempDir::new().unwrap();
        std::fs::write(
            root.path().join("navigator.yaml"),
            "host: navigator.example.com\nproject: acme\ndockets:\n  - 9:26-cv-00042\n",
        )
        .unwrap();
        let host = server.uri();
        let project_id = Uuid::now_v7();
        let entry_id = Uuid::now_v7();
        let hearing_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let credentials = root.path().join("credentials.json");
        std::fs::write(
            &credentials,
            json!({"hosts":{host.clone():{
                "token":"test-token", "person_email":"lawyer@example.com", "role":"lawyer",
                "expires_at":i64::MAX
            }}})
            .to_string(),
        )
        .unwrap();

        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        let listed = json!({ "cases": [{"id": Uuid::now_v7(), "docket_number": "9:26-cv-00042"}], "entries": [] });
        Mock::given(method("GET"))
            .and(path(format!("/app/api/projects/{project_id}/docket")))
            .respond_with(ResponseTemplate::new(200).set_body_json(&listed))
            .expect(1)
            .mount(&server)
            .await;
        let created = json!({ "id": entry_id, "entry_number": "12", "kind": "motion" });
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/docket")))
            .and(body_json(json!({
                "docket_number": "9:26-cv-00042",
                "entry_number": "12",
                "kind": "motion",
                "title": "MOTION to Dismiss",
                "filed_or_served_on": "2026-03-04",
                "slug": "docket/9-26-cv-00042/di-12-motion.pdf"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(&created))
            .expect(1)
            .mount(&server)
            .await;
        let linked = json!({ "id": entry_id, "document_asset_id": document_id });
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/docket/{entry_id}/link-document"
            )))
            .and(body_json(json!({ "document_id": document_id })))
            .respond_with(ResponseTemplate::new(200).set_body_json(&linked))
            .expect(1)
            .mount(&server)
            .await;
        let continued =
            json!({ "id": Uuid::now_v7(), "entry_number": "14", "supersedes": hearing_id });
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project_id}/docket/{hearing_id}/supersede"
            )))
            .and(body_json(json!({
                "entry_number": "14",
                "scheduled_on": "2027-04-01T09:00:00-07:00"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(&continued))
            .expect(1)
            .mount(&server)
            .await;

        let root = root.path();
        assert_eq!(
            run(root, &credentials, &host, &["project", "docket", "list"]),
            listed
        );
        let add = [
            "project",
            "docket",
            "add",
            "--entry-number",
            "12",
            "--kind",
            "motion",
            "--title",
            "MOTION to Dismiss",
            "--filed-on",
            "2026-03-04",
            "--slug",
            "docket/9-26-cv-00042/di-12-motion.pdf",
        ];
        let mut dry = add.to_vec();
        dry.push("--dry-run");
        let planned = run(root, &credentials, &host, &dry);
        assert_eq!(planned["entry_number"], "12");
        assert_eq!(planned["docket_number"], "9:26-cv-00042");
        assert_eq!(run(root, &credentials, &host, &add), created);
        let document = document_id.to_string();
        assert_eq!(
            run(
                root,
                &credentials,
                &host,
                &[
                    "project",
                    "docket",
                    "link-document",
                    &entry_id.to_string(),
                    "--document",
                    &document,
                ],
            ),
            linked
        );
        assert_eq!(
            run(
                root,
                &credentials,
                &host,
                &[
                    "project",
                    "docket",
                    "supersede",
                    &hearing_id.to_string(),
                    "--entry-number",
                    "14",
                    "--scheduled-on",
                    "2027-04-01T09:00:00-07:00",
                ],
            ),
            continued
        );
        // One POST per write: the dry run added none.
        server.verify().await;
        let posts = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method == "POST")
            .count();
        assert_eq!(posts, 3, "a dry run makes no POST");
    }
}
