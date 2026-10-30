//! End-to-end coverage for `navigator project sync` against its authorized
//! API.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use assert_cmd::Command;
use base64::Engine as _;
use predicates::prelude::*;
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn base64_of(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    store::documents::sha256_hex(bytes)
}

fn navigator() -> Command {
    let mut command = Command::cargo_bin("navigator").unwrap();
    command.env_remove("GITHUB_REPOSITORY");
    command
}

fn write(root: &Path, relative: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn manifest(root: &Path, host: &str) {
    write(
        root,
        "navigator.yaml",
        format!("project: acme\nhost: {host}\n"),
    );
}

fn credentials(root: &Path, host: &str) -> std::path::PathBuf {
    let path = root.join("credentials.json");
    write(
        root,
        "credentials.json",
        serde_json::to_vec(&serde_json::json!({
            "hosts": {
                host: {
                    "token": "test-token",
                    "person_email": "lawyer@example.com",
                    "role": "lawyer",
                    "expires_at": i64::MAX
                }
            }
        }))
        .unwrap(),
    );
    path
}

fn pointer(asset_id: Uuid) -> serde_json::Value {
    pointer_with_sha(
        asset_id,
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        18,
    )
}

fn pointer_with_sha(asset_id: Uuid, sha256: &str, size_bytes: i64) -> serde_json::Value {
    serde_json::json!({
        "kind": "filing",
        "visibility": "internal",
        "current_version": {
            "version": 1,
            "asset_id": asset_id,
            "created_at": "2026-09-05T12:00:00Z",
            "sha256": sha256,
            "size_bytes": size_bytes
        }
    })
}

/// An Authority-backed pointer as `project sync` writes it (LAW-82): empty
/// `kind`, `authority_id` set, the asset id naming the Authority's archived
/// asset rather than a Project document.
fn authority_pointer(asset_id: Uuid, authority_id: Uuid) -> serde_json::Value {
    let mut pointer = pointer(asset_id);
    pointer["kind"] = serde_json::Value::String(String::new());
    pointer["authority_id"] = serde_json::Value::String(authority_id.to_string());
    pointer
}

async fn mount_project_lookup(server: &MockServer, project_id: Uuid) {
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "acme"}
        ])))
        .expect(1)
        .mount(server)
        .await;
}

fn init_git_checkout(root: &Path) {
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
}

fn live_document(asset_id: Uuid, bytes: &[u8], slug: &str) -> serde_json::Value {
    serde_json::json!({
        "id": asset_id,
        "storage_key": format!("projects/acme/documents/{}", sha256(bytes)),
        "secondary_storage_key": null,
        "content_type": "application/pdf",
        "byte_size": bytes.len(),
        "sha256_hex": sha256(bytes),
        "project_id": Uuid::now_v7(),
        "filename": "motion.pdf",
        "kind": "filing",
        "source": "upload",
        "received_at": null,
        "description": null,
        "visibility": "internal",
        "slug": slug,
        "published_at": "2026-09-23T12:00:00Z",
        "source_message_id": null,
        "source_sender": null,
        "source_received_at": null,
        "source_subject": null,
        "metadata": null,
        "inserted_at": "2026-09-23T12:00:00Z",
        "updated_at": "2026-09-23T12:00:00Z"
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn project_sync_discovers_unpointed_documents_and_a_second_run_skips_them() {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = TempDir::new().unwrap();
    manifest(root.path(), &host);
    init_git_checkout(root.path());
    let credential_path = credentials(root.path(), &host);
    let project_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    let bytes = b"live motion bytes";
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "acme"}
        ])))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![live_document(
            asset_id,
            bytes,
            "pleadings/motion.pdf",
        )]))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/projects/acme/documents/{asset_id}/download"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let checkout = root.path().to_path_buf();
    let first_credentials = credential_path.clone();
    tokio::task::spawn_blocking(move || {
        navigator()
            .current_dir(checkout)
            .env("NAVIGATOR_CREDENTIALS_FILE", first_credentials)
            .args(["project", "sync"])
            .timeout(Duration::from_secs(180))
            .assert()
            .success()
            .stdout(predicate::str::contains("1 added, 0 updated, 0 skipped"));
    })
    .await
    .unwrap();
    assert_eq!(
        fs::read(
            root.path()
                .join("documents/Docket Filings/pleadings/motion.pdf")
        )
        .unwrap(),
        bytes
    );
    assert!(root
        .path()
        .join("blobs/pleadings/motion.pdf.yaml")
        .is_file());
    assert!(!root.path().join("blobs/pleadings/motion.pdf.yml").exists());
    let git_status = std::process::Command::new("git")
        .args([
            "status",
            "--short",
            "--untracked-files=all",
            "--",
            "documents",
            "blobs",
            ".gitignore",
        ])
        .current_dir(root.path())
        .output()
        .unwrap();
    let git_status = String::from_utf8(git_status.stdout).unwrap();
    assert!(
        git_status.lines().any(|line| line.ends_with(" .gitignore")),
        "sync writes the canonical root .gitignore into a checkout with none: {git_status}"
    );
    assert!(!git_status.contains("documents/"), "{git_status}");
    assert!(git_status.contains("motion.pdf.yaml"), "{git_status}");
    assert!(
        !git_status.lines().any(|line| line.ends_with("motion.pdf")),
        "raw bytes must stay out of Git status: {git_status}"
    );

    let checkout = root.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        navigator()
            .current_dir(checkout)
            .env("NAVIGATOR_CREDENTIALS_FILE", credential_path)
            .args(["project", "sync"])
            .timeout(Duration::from_secs(180))
            .assert()
            .success()
            .stdout(predicate::str::contains("0 added, 0 updated, 1 skipped"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn project_sync_dry_run_writes_nothing() {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = TempDir::new().unwrap();
    manifest(root.path(), &host);
    let credential_path = credentials(root.path(), &host);
    let project_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    mount_project_lookup(&server, project_id).await;
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![live_document(
            asset_id,
            b"preview bytes",
            "pleadings/preview.pdf",
        )]))
        .expect(1)
        .mount(&server)
        .await;

    let checkout = root.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        navigator()
            .current_dir(checkout)
            .env("NAVIGATOR_CREDENTIALS_FILE", credential_path)
            .args(["project", "sync", "--dry-run"])
            .timeout(Duration::from_secs(180))
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "would add documents/pleadings/preview.pdf",
            ));
    })
    .await
    .unwrap();
    assert!(!root.path().join("documents").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn project_sync_reports_forbidden_download_as_could_not_read() {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = TempDir::new().unwrap();
    manifest(root.path(), &host);
    init_git_checkout(root.path());
    let credential_path = credentials(root.path(), &host);
    let project_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    mount_project_lookup(&server, project_id).await;
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![live_document(
            asset_id,
            b"private bytes",
            "privileged/private.pdf",
        )]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/projects/acme/documents/{asset_id}/download"
        )))
        .respond_with(ResponseTemplate::new(403).set_body_string("not on your lens"))
        .mount(&server)
        .await;

    let checkout = root.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        navigator()
            .current_dir(checkout)
            .env("NAVIGATOR_CREDENTIALS_FILE", credential_path)
            .args(["project", "sync"])
            .timeout(Duration::from_secs(180))
            .assert()
            .failure()
            .stderr(predicate::str::contains("could not read"))
            .stderr(predicate::str::contains("missing").not());
    })
    .await
    .unwrap();
    assert!(!root
        .path()
        .join("blobs/privileged/private.pdf.yaml")
        .exists());
}

/// A committed Authority pointer whose digest and size describe `bytes`, as
/// `project sync` writes it for a capture under `documents/cases` or
/// `documents/rules`.
fn authority_pointer_for(bytes: &[u8], authority_id: Uuid) -> serde_json::Value {
    let mut pointer = authority_pointer(Uuid::now_v7(), authority_id);
    pointer["current_version"]["sha256"] = serde_json::Value::String(sha256(bytes));
    pointer["current_version"]["size_bytes"] = serde_json::json!(bytes.len());
    pointer
}

async fn mount_empty_project_inventory(server: &MockServer, project_id: Uuid) {
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(server)
        .await;
}

/// LAW-93: a fresh checkout carries only the committed Authority pointers;
/// `project sync` fetches each one's bytes from the Authority archive door,
/// leaves them out of Git, and a second run fetches nothing.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_hydrates_authority_pointers_into_a_fresh_checkout() {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = TempDir::new().unwrap();
    manifest(root.path(), &host);
    init_git_checkout(root.path());
    let credential_path = credentials(root.path(), &host);
    let project_id = Uuid::now_v7();
    mount_empty_project_inventory(&server, project_id).await;

    let case_bytes = b"synthetic opinion bytes";
    let rule_bytes = b"synthetic statute bytes";
    let case_id = Uuid::now_v7();
    let rule_id = Uuid::now_v7();
    write(
        root.path(),
        "blobs/cases/roe.pdf.yaml",
        serde_yaml::to_string(&authority_pointer_for(case_bytes, case_id)).unwrap(),
    );
    write(
        root.path(),
        "blobs/rules/statute.pdf.yaml",
        serde_yaml::to_string(&authority_pointer_for(rule_bytes, rule_id)).unwrap(),
    );
    for (id, bytes) in [(case_id, &case_bytes[..]), (rule_id, &rule_bytes[..])] {
        Mock::given(method("GET"))
            .and(path(format!("/app/api/authorities/{id}/archive")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
            .expect(1)
            .mount(&server)
            .await;
    }

    for _ in 0..2 {
        let checkout = root.path().to_path_buf();
        let credentials = credential_path.clone();
        tokio::task::spawn_blocking(move || {
            navigator()
                .current_dir(checkout)
                .env("NAVIGATOR_CREDENTIALS_FILE", credentials)
                .args(["project", "sync"])
                .timeout(Duration::from_secs(180))
                .assert()
                .success();
        })
        .await
        .unwrap();
    }

    assert_eq!(
        fs::read(root.path().join("documents/cases/roe.pdf")).unwrap(),
        case_bytes
    );
    assert_eq!(
        fs::read(root.path().join("documents/rules/statute.pdf")).unwrap(),
        rule_bytes
    );
    let git_status = std::process::Command::new("git")
        .args([
            "status",
            "--short",
            "--untracked-files=all",
            "--",
            "documents",
        ])
        .current_dir(root.path())
        .output()
        .unwrap();
    let git_status = String::from_utf8(git_status.stdout).unwrap();
    assert!(
        !git_status
            .lines()
            .any(|line| line.ends_with("roe.pdf") || line.ends_with("statute.pdf")),
        "authority bytes must stay out of Git status: {git_status}"
    );
}

/// An archive whose bytes do not match the pointer's committed digest is
/// never written.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_refuses_authority_bytes_that_do_not_match_the_pointer() {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = TempDir::new().unwrap();
    manifest(root.path(), &host);
    init_git_checkout(root.path());
    let credential_path = credentials(root.path(), &host);
    mount_empty_project_inventory(&server, Uuid::now_v7()).await;
    let authority_id = Uuid::now_v7();
    write(
        root.path(),
        "blobs/cases/roe.pdf.yaml",
        serde_yaml::to_string(&authority_pointer_for(b"expected bytes", authority_id)).unwrap(),
    );
    Mock::given(method("GET"))
        .and(path(format!("/app/api/authorities/{authority_id}/archive")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"tampered bytes".to_vec()))
        .mount(&server)
        .await;

    let checkout = root.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        navigator()
            .current_dir(checkout)
            .env("NAVIGATOR_CREDENTIALS_FILE", credential_path)
            .args(["project", "sync"])
            .timeout(Duration::from_secs(180))
            .assert()
            .failure()
            .stderr(predicate::str::contains("documents/cases/roe.pdf"))
            .stderr(predicate::str::contains("sha256 mismatch"));
    })
    .await
    .unwrap();
    assert!(!root.path().join("documents/cases/roe.pdf").exists());
}

/// `--dry-run` names the pending Authority copies and writes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_dry_run_lists_authority_copies_without_fetching() {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = TempDir::new().unwrap();
    manifest(root.path(), &host);
    init_git_checkout(root.path());
    let credential_path = credentials(root.path(), &host);
    mount_empty_project_inventory(&server, Uuid::now_v7()).await;
    write(
        root.path(),
        "blobs/cases/roe.pdf.yaml",
        serde_yaml::to_string(&authority_pointer_for(b"bytes", Uuid::now_v7())).unwrap(),
    );

    let checkout = root.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        navigator()
            .current_dir(checkout)
            .env("NAVIGATOR_CREDENTIALS_FILE", credential_path)
            .args(["project", "sync", "--dry-run"])
            .timeout(Duration::from_secs(180))
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "would fetch documents/cases/roe.pdf",
            ));
    })
    .await
    .unwrap();
    assert!(!root.path().join("documents/cases/roe.pdf").exists());
}

/// A committed pointer for one revision of a document, as `project sync`
/// writes it from `live_document`.
fn committed_pointer(
    asset_id: Uuid,
    bytes: &[u8],
    version: usize,
    previous: Option<Uuid>,
    visibility: &str,
) -> serde_json::Value {
    let mut pointer = pointer_with_sha(
        asset_id,
        &sha256(bytes),
        i64::try_from(bytes.len()).unwrap(),
    );
    pointer["visibility"] = serde_json::Value::String(visibility.to_string());
    pointer["current_version"]["version"] = serde_json::json!(version);
    pointer["current_version"]["created_at"] = "2026-09-23T12:00:00Z".into();
    if let Some(previous) = previous {
        pointer["previous_version"] = serde_json::Value::String(previous.to_string());
    }
    pointer
}

/// A checkout holding `local` beside the committed `pointer` for `slug`.
/// Whether `slug` keeps its bytes at `documents/<slug>`: a sibling folder
/// beside the matter folders (LAW-189).
fn is_sibling(slug: &str) -> bool {
    [
        "onboarding/",
        "offboarding/",
        "invoices/",
        "cases/",
        "rules/",
    ]
    .iter()
    .any(|folder| slug.starts_with(folder))
}

/// Where `pointer` (a JSON pointer) keeps the bytes of `slug`, below the
/// checkout: its matter folder, or the slug itself in a sibling folder.
fn bytes_path_for(slug: &str, pointer: &serde_json::Value) -> String {
    if is_sibling(slug) {
        return format!("documents/{slug}");
    }
    let folder = match pointer["folder"].as_str() {
        Some(folder) => store::document_pointers::MatterFolder::parse(folder).unwrap(),
        None => store::document_pointers::MatterFolder::for_kind(
            pointer["kind"].as_str().unwrap_or_default(),
        ),
    };
    format!("documents/{}/{slug}", folder.as_str())
}

/// A checkout holding `local` in its matter folder beside the committed
/// `pointer` for `slug` under `blobs/`.
fn write_pair(root: &Path, slug: &str, local: Option<&[u8]>, pointer: &serde_json::Value) {
    if let Some(local) = local {
        write(root, &bytes_path_for(slug, pointer), local);
    }
    write(
        root,
        &format!("blobs/{slug}.yaml"),
        serde_yaml::to_string(pointer).unwrap(),
    );
}

/// Every revision of one document as the inventory lists it, newest first.
fn live_chain(slug: &str, revisions: &[(Uuid, &[u8])]) -> Vec<serde_json::Value> {
    revisions
        .iter()
        .map(|(id, bytes)| live_document(*id, bytes, slug))
        .collect()
}

async fn mount_inventory(server: &MockServer, project_id: Uuid, documents: Vec<serde_json::Value>) {
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": project_id, "code": "acme"}
        ])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(200).set_body_json(documents))
        .mount(server)
        .await;
}

async fn mount_download(server: &MockServer, asset_id: Uuid, bytes: &[u8]) {
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/projects/acme/documents/{asset_id}/download"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .mount(server)
        .await;
}

/// Accept every upload to the Project's collection, answering with the
/// pointer a real revision upload returns. Returns nothing: callers count the
/// requests through `server.received_requests()`.
async fn mount_revision_receipt(
    server: &MockServer,
    project_id: Uuid,
    asset_id: Uuid,
    bytes: &[u8],
    version: usize,
    previous: Option<Uuid>,
) {
    Mock::given(method("POST"))
        .and(path(format!("/app/api/projects/{project_id}/documents")))
        .respond_with(ResponseTemplate::new(201).set_body_json(committed_pointer(
            asset_id, bytes, version, previous, "internal",
        )))
        .mount(server)
        .await;
}

async fn writes(server: &MockServer) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.method != wiremock::http::Method::GET)
        .collect()
}

async fn project_sync_output(
    root: &Path,
    credential_path: &Path,
    args: &[&str],
) -> std::process::Output {
    let checkout = root.to_path_buf();
    let credentials = credential_path.to_path_buf();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    tokio::task::spawn_blocking(move || {
        navigator()
            .current_dir(checkout)
            .env("NAVIGATOR_CREDENTIALS_FILE", credentials)
            .args(["project", "sync"])
            .args(args)
            .timeout(Duration::from_secs(180))
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

struct Checkout {
    server: MockServer,
    root: TempDir,
    credentials: std::path::PathBuf,
    project_id: Uuid,
}

async fn checkout() -> Checkout {
    let server = MockServer::start().await;
    let host = server.uri();
    let root = TempDir::new().unwrap();
    manifest(root.path(), &host);
    init_git_checkout(root.path());
    let credentials = credentials(root.path(), &host);
    Checkout {
        server,
        root,
        credentials,
        project_id: Uuid::now_v7(),
    }
}

impl Checkout {
    async fn sync(&self, args: &[&str]) -> (std::process::Output, String, String) {
        let output = project_sync_output(self.root.path(), &self.credentials, args).await;
        let stdout = String::from_utf8(output.stdout.clone()).unwrap();
        let stderr = String::from_utf8(output.stderr.clone()).unwrap();
        (output, stdout, stderr)
    }

    /// The local bytes of `slug`: in the matter folder its committed pointer
    /// names, else at `documents/<slug>` where nothing has moved them yet.
    fn local(&self, slug: &str) -> Vec<u8> {
        let root = self.root.path();
        let laid_out = fs::read_to_string(root.join(format!("blobs/{slug}.yaml")))
            .ok()
            .filter(|_| !is_sibling(slug))
            .and_then(|raw| store::document_pointers::DocumentPointer::from_yaml(&raw).ok())
            .map(|pointer| {
                root.join("documents")
                    .join(pointer.matter_folder().as_str())
                    .join(slug)
            })
            .filter(|path| path.is_file())
            .or_else(|| {
                store::document_pointers::MatterFolder::ALL
                    .iter()
                    .map(|folder| root.join("documents").join(folder.as_str()).join(slug))
                    .find(|path| path.is_file())
            });
        fs::read(laid_out.unwrap_or_else(|| root.join("documents").join(slug))).unwrap()
    }

    fn pointer(&self, slug: &str) -> store::document_pointers::DocumentPointer {
        store::document_pointers::DocumentPointer::from_yaml(
            &fs::read_to_string(self.root.path().join(format!("blobs/{slug}.yaml"))).unwrap(),
        )
        .unwrap()
    }
}

const FILED: &[u8] = b"the filed bytes";
const NEWER: &[u8] = b"the newer live bytes";
const EDITED: &[u8] = b"the filed bytes, edited by a lawyer";

/// LAW-110: one dry run names the different things the old `would update`
/// covered, and ENG-861 adds what each would ask.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_dry_run_names_every_action_and_writes_nothing() {
    let fx = checkout().await;
    let ids: Vec<Uuid> = (0..6).map(|_| Uuid::now_v7()).collect();
    let newer = Uuid::now_v7();
    let mut inventory = Vec::new();
    inventory.extend(live_chain("a/fetch.pdf", &[(ids[0], FILED)]));
    inventory.extend(live_chain("b/rewrite.pdf", &[(ids[1], FILED)]));
    inventory.extend(live_chain("c/edited.pdf", &[(ids[2], FILED)]));
    inventory.extend(live_chain(
        "d/behind.pdf",
        &[(newer, NEWER), (ids[3], FILED)],
    ));
    inventory.extend(live_chain(
        "e/conflict.pdf",
        &[(newer, NEWER), (ids[4], FILED)],
    ));
    mount_inventory(&fx.server, fx.project_id, inventory).await;
    let root = fx.root.path();
    write_pair(
        root,
        "b/rewrite.pdf",
        Some(FILED),
        &committed_pointer(ids[1], FILED, 1, None, "client"),
    );
    write_pair(
        root,
        "c/edited.pdf",
        Some(EDITED),
        &committed_pointer(ids[2], FILED, 1, None, "internal"),
    );
    write_pair(
        root,
        "d/behind.pdf",
        Some(FILED),
        &committed_pointer(ids[3], FILED, 1, None, "internal"),
    );
    write_pair(
        root,
        "e/conflict.pdf",
        Some(EDITED),
        &committed_pointer(ids[4], FILED, 1, None, "internal"),
    );
    write(root, "documents/f/new.pdf", b"a brand new file");
    write_pair(
        root,
        "g/orphan.pdf",
        None,
        &committed_pointer(ids[5], FILED, 1, None, "internal"),
    );

    let (output, stdout, _) = fx.sync(&["--dry-run"]).await;

    assert!(output.status.success(), "{stdout}");
    for expected in [
        "would add documents/a/fetch.pdf",
        "would rewrite pointer documents/b/rewrite.pdf",
        "would ask to upload local edits as a new revision of c/edited.pdf",
        "would ask to overwrite the local copy with live revision 2: documents/d/behind.pdf",
        "conflict documents/e/conflict.pdf: would ask which side wins (local edits are overwritten if live wins)",
        "would ask once to upload 1 new document(s):",
        "documents/f/new.pdf",
        "cannot reconcile blobs/g/orphan.pdf.yaml",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?} in:\n{stdout}");
    }
    assert!(!stdout.contains("would update"), "{stdout}");
    assert!(writes(&fx.server).await.is_empty());
    assert_eq!(fx.local("c/edited.pdf"), EDITED);
    assert!(!root.join("documents/a/fetch.pdf").exists());
}

/// Rows 1 and 2 of the table: matching bytes need nothing, absent bytes
/// download, and a second run is a no-op that writes nothing to the cloud.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_downloads_absent_bytes_and_a_repeat_run_changes_nothing() {
    let fx = checkout().await;
    let (matching, absent) = (Uuid::now_v7(), Uuid::now_v7());
    let mut inventory = live_chain("a/matching.pdf", &[(matching, FILED)]);
    inventory.extend(live_chain("b/absent.pdf", &[(absent, NEWER)]));
    mount_inventory(&fx.server, fx.project_id, inventory).await;
    mount_download(&fx.server, absent, NEWER).await;
    let root = fx.root.path();
    write_pair(
        root,
        "a/matching.pdf",
        Some(FILED),
        &committed_pointer(matching, FILED, 1, None, "internal"),
    );
    write_pair(
        root,
        "b/absent.pdf",
        None,
        &committed_pointer(absent, NEWER, 1, None, "internal"),
    );

    let (first, stdout, _) = fx.sync(&[]).await;
    assert!(first.status.success(), "{stdout}");
    assert!(stdout.contains("0 added, 1 updated, 1 skipped"), "{stdout}");
    assert_eq!(fx.local("b/absent.pdf"), NEWER);

    let (second, stdout, _) = fx.sync(&[]).await;
    assert!(second.status.success(), "{stdout}");
    assert!(stdout.contains("0 added, 0 updated, 2 skipped"), "{stdout}");
    assert!(
        writes(&fx.server).await.is_empty(),
        "no run may write to the cloud"
    );
}

/// Row 3: untouched bytes and a newer live revision. The overwrite needs a
/// yes; `--yes` gives it, and the pointer follows the live revision.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_replaces_an_untouched_copy_with_a_newer_live_revision_only_when_approved() {
    let fx = checkout().await;
    let (old, newer) = (Uuid::now_v7(), Uuid::now_v7());
    mount_inventory(
        &fx.server,
        fx.project_id,
        live_chain("d/behind.pdf", &[(newer, NEWER), (old, FILED)]),
    )
    .await;
    mount_download(&fx.server, newer, NEWER).await;
    write_pair(
        fx.root.path(),
        "d/behind.pdf",
        Some(FILED),
        &committed_pointer(old, FILED, 1, None, "internal"),
    );

    let (refused, _, stderr) = fx.sync(&[]).await;
    assert!(!refused.status.success());
    assert!(stderr.contains("documents/d/behind.pdf"), "{stderr}");
    assert!(stderr.contains("--yes"), "{stderr}");
    assert_eq!(
        fx.local("d/behind.pdf"),
        FILED,
        "no TTY and no --yes overwrites nothing"
    );

    let (approved, stdout, _) = fx.sync(&["--yes"]).await;
    assert!(approved.status.success(), "{stdout}");
    assert_eq!(fx.local("d/behind.pdf"), NEWER);
    assert_eq!(fx.pointer("d/behind.pdf").current_version.asset_id, newer);
    assert_eq!(fx.pointer("d/behind.pdf").current_version.version, 2);
}

/// Row 4: edited bytes on the live pointer upload as a new revision of the
/// same slug, keep the local bytes and the recorded external id, and the next
/// run is a no-op.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_uploads_edited_bytes_as_a_new_revision_and_keeps_the_envelope_id() {
    let fx = checkout().await;
    let (current, revision) = (Uuid::now_v7(), Uuid::now_v7());
    let envelope = Uuid::now_v7();
    // Before the upload the live chain has one revision; after, two.
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": fx.project_id, "code": "acme"}
        ])))
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{}/documents",
            fx.project_id
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(live_chain(
            "onboarding/letter_signed.pdf",
            &[(current, FILED)],
        )))
        .up_to_n_times(1)
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{}/documents",
            fx.project_id
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(live_chain(
            "onboarding/letter_signed.pdf",
            &[(revision, EDITED), (current, FILED)],
        )))
        .mount(&fx.server)
        .await;
    mount_revision_receipt(
        &fx.server,
        fx.project_id,
        revision,
        EDITED,
        2,
        Some(current),
    )
    .await;
    let mut pointer = committed_pointer(current, FILED, 1, None, "internal");
    pointer["kind"] = "onboarding".into();
    pointer["docusign_envelope_id"] = envelope.to_string().into();
    write_pair(
        fx.root.path(),
        "onboarding/letter_signed.pdf",
        Some(EDITED),
        &pointer,
    );

    let (output, stdout, stderr) = fx.sync(&["--yes"]).await;

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("1 uploaded"), "{stdout}");
    let posts = writes(&fx.server).await;
    assert_eq!(posts.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(
        body["slug"], "onboarding/letter_signed.pdf",
        "a revision, never a new document"
    );
    assert_eq!(body["kind"], "onboarding", "kind comes from the pointer");
    assert_eq!(body["content_base64"], base64_of(EDITED));
    assert_eq!(
        fx.local("onboarding/letter_signed.pdf"),
        EDITED,
        "local bytes are kept"
    );
    let written = fx.pointer("onboarding/letter_signed.pdf");
    assert_eq!(written.current_version.asset_id, revision);
    assert_eq!(written.current_version.version, 2);
    assert_eq!(written.docusign_envelope_id, Some(envelope));

    let (again, stdout, _) = fx.sync(&["--yes"]).await;
    assert!(again.status.success(), "{stdout}");
    assert!(stdout.contains("0 added, 0 updated, 1 skipped"), "{stdout}");
    assert_eq!(
        writes(&fx.server).await.len(),
        1,
        "the repeat run wrote nothing"
    );
}

/// Row 5: a conflict is never decided for the person. `--yes` and a missing
/// terminal both leave both sides untouched and exit non-zero.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_never_resolves_a_conflict_without_a_person() {
    let fx = checkout().await;
    let (old, newer) = (Uuid::now_v7(), Uuid::now_v7());
    mount_inventory(
        &fx.server,
        fx.project_id,
        live_chain("e/conflict.pdf", &[(newer, NEWER), (old, FILED)]),
    )
    .await;
    mount_download(&fx.server, newer, NEWER).await;
    write_pair(
        fx.root.path(),
        "e/conflict.pdf",
        Some(EDITED),
        &committed_pointer(old, FILED, 1, None, "internal"),
    );

    for args in [vec![], vec!["--yes"]] {
        let (output, _, stderr) = fx.sync(&args).await;
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr.contains("documents/e/conflict.pdf"), "{stderr}");
        assert!(stderr.contains("choose a side"), "{stderr}");
        assert_eq!(fx.local("e/conflict.pdf"), EDITED);
        assert_eq!(fx.pointer("e/conflict.pdf").current_version.asset_id, old);
        assert!(writes(&fx.server).await.is_empty());
    }
}

/// Row 6: a new file with no pointer and no live slug uploads as a new
/// document, after one yes for the whole batch.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_uploads_new_files_as_new_documents_after_one_batch_yes() {
    let fx = checkout().await;
    let created = Uuid::now_v7();
    mount_inventory(&fx.server, fx.project_id, Vec::new()).await;
    mount_revision_receipt(
        &fx.server,
        fx.project_id,
        created,
        b"a brand new file",
        1,
        None,
    )
    .await;
    write(
        fx.root.path(),
        "documents/memos/new.pdf",
        b"a brand new file",
    );

    let (refused, _, stderr) = fx.sync(&[]).await;
    assert!(!refused.status.success());
    assert!(stderr.contains("new document(s)"), "{stderr}");
    assert!(writes(&fx.server).await.is_empty());

    let (approved, stdout, stderr) = fx.sync(&["--yes"]).await;
    assert!(approved.status.success(), "{stdout}{stderr}");
    let posts = writes(&fx.server).await;
    assert_eq!(posts.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(body["slug"], "memos/new.pdf");
    assert_eq!(fx.local("memos/new.pdf"), b"a brand new file");
    assert_eq!(
        fx.pointer("memos/new.pdf").current_version.asset_id,
        created
    );
}

/// Row 7: no pointer but the live document exists: write the pointer and
/// download. Bytes that already differ are a conflict, not a download.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_writes_the_pointer_for_a_live_document_and_protects_unpointed_bytes() {
    let fx = checkout().await;
    let (fresh, held) = (Uuid::now_v7(), Uuid::now_v7());
    let mut inventory = live_chain("a/fresh.pdf", &[(fresh, FILED)]);
    inventory.extend(live_chain("b/held.pdf", &[(held, FILED)]));
    mount_inventory(&fx.server, fx.project_id, inventory).await;
    mount_download(&fx.server, fresh, FILED).await;
    mount_download(&fx.server, held, FILED).await;
    write(fx.root.path(), "documents/b/held.pdf", EDITED);

    let (output, _, stderr) = fx.sync(&["--yes"]).await;

    assert!(!output.status.success(), "the unpointed edit is a conflict");
    assert!(stderr.contains("documents/b/held.pdf"), "{stderr}");
    assert_eq!(fx.local("a/fresh.pdf"), FILED);
    assert_eq!(fx.pointer("a/fresh.pdf").current_version.asset_id, fresh);
    assert_eq!(fx.local("b/held.pdf"), EDITED);
    assert!(!fx.root.path().join("blobs/b/held.pdf.yaml").exists());
}

/// An earlier run uploaded the edit and died before it wrote the pointer.
/// The next run sees local bytes equal to the live revision and finishes the
/// publication without a second upload.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_finishes_an_interrupted_upload_without_uploading_again() {
    let fx = checkout().await;
    let (old, uploaded) = (Uuid::now_v7(), Uuid::now_v7());
    mount_inventory(
        &fx.server,
        fx.project_id,
        live_chain("c/edited.pdf", &[(uploaded, EDITED), (old, FILED)]),
    )
    .await;
    write_pair(
        fx.root.path(),
        "c/edited.pdf",
        Some(EDITED),
        &committed_pointer(old, FILED, 1, None, "internal"),
    );
    write(
        fx.root.path(),
        "documents/drafts/untouched.pdf",
        b"unrelated bytes",
    );

    let (output, stdout, _) = fx.sync(&["--only", "documents/c"]).await;

    assert!(output.status.success(), "{stdout}");
    assert!(writes(&fx.server).await.is_empty());
    let pointer = fx.pointer("c/edited.pdf");
    assert_eq!(pointer.current_version.asset_id, uploaded);
    assert_eq!(pointer.current_version.version, 2);
    assert_eq!(fx.local("c/edited.pdf"), EDITED);
    assert_eq!(fx.local("drafts/untouched.pdf"), b"unrelated bytes");
    assert!(!fx
        .root
        .path()
        .join("blobs/drafts/untouched.pdf.yaml")
        .exists());
}

/// A refused inventory is not an empty one: nothing is uploaded over what
/// could not be read.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_does_not_read_a_refused_inventory_as_empty() {
    for status in [401, 403] {
        let fx = checkout().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": fx.project_id, "code": "acme"}
            ])))
            .mount(&fx.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{}/documents",
                fx.project_id
            )))
            .respond_with(ResponseTemplate::new(status).set_body_string("not on your lens"))
            .mount(&fx.server)
            .await;
        write(
            fx.root.path(),
            "documents/memos/new.pdf",
            b"a brand new file",
        );

        let (output, _, stderr) = fx.sync(&["--yes"]).await;

        assert!(!output.status.success(), "{status}");
        assert!(
            stderr.contains("could not read the live inventory"),
            "{stderr}"
        );
        assert!(
            writes(&fx.server).await.is_empty(),
            "{status} must not read as absent"
        );
    }
}

/// A 403 on one item's download skips that item and publishes the rest; a
/// 403 on an upload leaves the local bytes and the pointer as they were.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_skips_an_item_it_cannot_read_or_write_and_finishes_the_others() {
    let fx = checkout().await;
    let (denied, readable, edited) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let mut inventory = live_chain("a/denied.pdf", &[(denied, FILED)]);
    inventory.extend(live_chain("b/readable.pdf", &[(readable, NEWER)]));
    inventory.extend(live_chain("c/edited.pdf", &[(edited, FILED)]));
    mount_inventory(&fx.server, fx.project_id, inventory).await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/projects/acme/documents/{denied}/download"
        )))
        .respond_with(ResponseTemplate::new(403).set_body_string("not on your lens"))
        .mount(&fx.server)
        .await;
    mount_download(&fx.server, readable, NEWER).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/api/projects/{}/documents",
            fx.project_id
        )))
        .respond_with(ResponseTemplate::new(403).set_body_string("read only"))
        .mount(&fx.server)
        .await;
    write_pair(
        fx.root.path(),
        "c/edited.pdf",
        Some(EDITED),
        &committed_pointer(edited, FILED, 1, None, "internal"),
    );

    let (output, _, stderr) = fx.sync(&["--yes"]).await;

    assert!(!output.status.success());
    assert!(
        stderr.contains("documents/a/denied.pdf: could not read"),
        "{stderr}"
    );
    assert!(
        stderr.contains("documents/c/edited.pdf: could not write"),
        "{stderr}"
    );
    assert!(!fx.root.path().join("documents/a/denied.pdf").exists());
    assert!(!fx.root.path().join("blobs/a/denied.pdf.yaml").exists());
    assert_eq!(
        fx.local("b/readable.pdf"),
        NEWER,
        "the readable item still published"
    );
    assert_eq!(fx.local("c/edited.pdf"), EDITED);
    assert_eq!(fx.pointer("c/edited.pdf").current_version.asset_id, edited);
}

fn git_in(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=test@example.test"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn head_commit(root: &Path) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Commit a pointer for `slug`, then delete it in a second commit, as a
/// lawyer removing `blobs/<slug>.yaml` would. Returns the deleting commit.
fn commit_then_remove_pointer(root: &Path, slug: &str, pointer: &serde_json::Value) -> String {
    write(
        root,
        &format!("blobs/{slug}.yaml"),
        serde_yaml::to_string(pointer).unwrap(),
    );
    git_in(root, &["add", "blobs"]);
    git_in(root, &["commit", "--quiet", "-m", "file the pointer"]);
    git_in(root, &["rm", "--quiet", &format!("blobs/{slug}.yaml")]);
    git_in(root, &["commit", "--quiet", "-m", "remove the pointer"]);
    head_commit(root)
}

async fn mount_soft_delete(server: &MockServer, project_id: Uuid, asset_id: Uuid, status: u16) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/api/projects/{project_id}/documents/{asset_id}/delete"
        )))
        .respond_with(if status == 200 {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "slug": "memos/gone.pdf", "revisions": 1,
                "deleted_at": "2026-10-02T12:00:00Z",
                "purge_after": "2027-03-31T12:00:00Z", "changed": true
            }))
        } else {
            ResponseTemplate::new(status).set_body_string("read only")
        })
        .mount(server)
        .await;
}

fn delete_posts(requests: &[wiremock::Request]) -> Vec<&wiremock::Request> {
    requests
        .iter()
        .filter(|request| request.url.path().ends_with("/delete"))
        .collect()
}

/// ENG-862: a removed pointer is a deletion request. A dry run offers it and
/// writes nothing, and no flag but `--yes-delete` ever carries it out.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_never_deletes_without_yes_delete() {
    let fx = checkout().await;
    let id = Uuid::now_v7();
    mount_inventory(
        &fx.server,
        fx.project_id,
        live_chain("memos/gone.pdf", &[(id, FILED)]),
    )
    .await;
    mount_soft_delete(&fx.server, fx.project_id, id, 200).await;
    let commit = commit_then_remove_pointer(
        fx.root.path(),
        "memos/gone.pdf",
        &committed_pointer(id, FILED, 1, None, "internal"),
    );

    let (dry, stdout, _) = fx.sync(&["--dry-run"]).await;
    assert!(dry.status.success(), "{stdout}");
    assert!(
        stdout.contains("would ask to soft-delete documents/memos/gone.pdf"),
        "{stdout}"
    );
    assert!(stdout.contains(&commit[..7]), "{stdout}");
    assert!(delete_posts(&writes(&fx.server).await).is_empty());

    for args in [vec![], vec!["--yes"]] {
        let (output, _, stderr) = fx.sync(&args).await;
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr.contains("documents/memos/gone.pdf"), "{stderr}");
        assert!(stderr.contains("--yes-delete"), "{stderr}");
        assert!(
            delete_posts(&writes(&fx.server).await).is_empty(),
            "{args:?} must not delete"
        );
    }
}

/// A chain with no pointer-removal history is still live input to sync. The
/// gate contract writes its missing pointer again; it cannot retire the chain
/// without first manufacturing the removal commit the chain never had.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_cannot_retire_a_chain_that_never_had_a_pointer() {
    let fx = checkout().await;
    let id = Uuid::now_v7();
    mount_inventory(
        &fx.server,
        fx.project_id,
        live_chain("strays/accidental.pdf", &[(id, FILED)]),
    )
    .await;
    write(fx.root.path(), "documents/strays/accidental.pdf", FILED);

    let (output, stdout, stderr) = fx.sync(&["--yes-delete"]).await;

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("1 updated"), "{stdout}");
    assert!(
        fx.root
            .path()
            .join("blobs/strays/accidental.pdf.yaml")
            .exists(),
        "sync writes the pointer for the still-live chain"
    );
    assert!(delete_posts(&writes(&fx.server).await).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_sync_detects_deletion_only_under_the_selected_path() {
    let fx = checkout().await;
    let selected_id = Uuid::now_v7();
    let outside_id = Uuid::now_v7();
    let mut inventory = live_chain("memos/selected.pdf", &[(selected_id, FILED)]);
    inventory.extend(live_chain("drafts/outside.pdf", &[(outside_id, FILED)]));
    mount_inventory(&fx.server, fx.project_id, inventory).await;
    mount_soft_delete(&fx.server, fx.project_id, selected_id, 200).await;
    commit_then_remove_pointer(
        fx.root.path(),
        "memos/selected.pdf",
        &committed_pointer(selected_id, FILED, 1, None, "internal"),
    );
    commit_then_remove_pointer(
        fx.root.path(),
        "drafts/outside.pdf",
        &committed_pointer(outside_id, FILED, 1, None, "internal"),
    );

    let (output, stdout, stderr) = fx
        .sync(&["--only", "documents/memos", "--yes-delete"])
        .await;
    assert!(output.status.success(), "{stdout}{stderr}");
    let posts = writes(&fx.server).await;
    assert_eq!(delete_posts(&posts).len(), 1);
    assert!(delete_posts(&posts)[0]
        .url
        .path()
        .contains(&selected_id.to_string()));
    assert!(!fx
        .root
        .path()
        .join("blobs/drafts/outside.pdf.yaml")
        .exists());
}

/// A confirmed deletion records the person's session and the commit that
/// removed the pointer; the next run asks nothing and does not upload the
/// leftover bytes as a new document.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_soft_deletes_with_the_removing_commit_and_then_stays_quiet() {
    let fx = checkout().await;
    let id = Uuid::now_v7();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": fx.project_id, "code": "acme"}
        ])))
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{}/documents",
            fx.project_id
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(live_chain("memos/gone.pdf", &[(id, FILED)])),
        )
        .up_to_n_times(1)
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{}/documents",
            fx.project_id
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(&fx.server)
        .await;
    mount_soft_delete(&fx.server, fx.project_id, id, 200).await;
    let commit = commit_then_remove_pointer(
        fx.root.path(),
        "memos/gone.pdf",
        &committed_pointer(id, FILED, 1, None, "internal"),
    );
    write(fx.root.path(), "documents/memos/gone.pdf", FILED);

    let (output, stdout, stderr) = fx.sync(&["--yes-delete"]).await;

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("1 soft-deleted"), "{stdout}");
    let requests = writes(&fx.server).await;
    let deletes = delete_posts(&requests);
    assert_eq!(deletes.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&deletes[0].body).unwrap();
    assert_eq!(body["commit_sha"], commit);
    assert_eq!(
        fx.local("memos/gone.pdf"),
        FILED,
        "local bytes are never touched"
    );

    let (again, stdout, _) = fx.sync(&["--yes-delete", "--yes"]).await;
    assert!(again.status.success(), "{stdout}");
    assert!(
        stdout.contains("its pointer was removed in"),
        "the leftover bytes are reported, not uploaded: {stdout}"
    );
    assert_eq!(
        writes(&fx.server).await.len(),
        1,
        "no second delete, no upload"
    );
}

/// Restoring brings the document back into the inventory. The next sync
/// writes its pointer again, without asking about the deletion a second time.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_writes_the_pointer_back_after_a_restore() {
    let fx = checkout().await;
    let id = Uuid::now_v7();
    Mock::given(method("GET"))
        .and(path("/app/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": fx.project_id, "code": "acme"}
        ])))
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/app/api/projects/{}/documents",
            fx.project_id
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(live_chain("memos/gone.pdf", &[(id, FILED)])),
        )
        .mount(&fx.server)
        .await;
    mount_soft_delete(&fx.server, fx.project_id, id, 200).await;
    mount_download(&fx.server, id, FILED).await;
    commit_then_remove_pointer(
        fx.root.path(),
        "memos/gone.pdf",
        &committed_pointer(id, FILED, 1, None, "internal"),
    );
    let (deleted, stdout, _) = fx.sync(&["--yes-delete"]).await;
    assert!(deleted.status.success(), "{stdout}");
    assert_eq!(delete_posts(&writes(&fx.server).await).len(), 1);

    // The inventory still lists the document, as it does once it is restored.
    let (restored, stdout, stderr) = fx.sync(&[]).await;

    assert!(restored.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("1 added"), "{stdout}");
    assert_eq!(fx.pointer("memos/gone.pdf").current_version.asset_id, id);
    assert_eq!(fx.local("memos/gone.pdf"), FILED);
    assert_eq!(
        delete_posts(&writes(&fx.server).await).len(),
        1,
        "asked once"
    );
}

/// A refused delete leaves the live document alone and is asked about again.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_reports_a_refused_deletion_and_asks_again_next_time() {
    let fx = checkout().await;
    let id = Uuid::now_v7();
    mount_inventory(
        &fx.server,
        fx.project_id,
        live_chain("memos/gone.pdf", &[(id, FILED)]),
    )
    .await;
    mount_soft_delete(&fx.server, fx.project_id, id, 403).await;
    commit_then_remove_pointer(
        fx.root.path(),
        "memos/gone.pdf",
        &committed_pointer(id, FILED, 1, None, "internal"),
    );

    let (output, _, stderr) = fx.sync(&["--yes-delete"]).await;

    assert!(!output.status.success());
    assert!(
        stderr.contains("documents/memos/gone.pdf: could not write"),
        "{stderr}"
    );
    let (dry, stdout, _) = fx.sync(&["--dry-run"]).await;
    assert!(dry.status.success());
    assert!(stdout.contains("would ask to soft-delete"), "{stdout}");
}

/// Guards: a `closed_repository` archive, a moved pointer, and a document that
/// never had a pointer are never offered for deletion.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_never_offers_to_delete_an_archive_a_moved_pointer_or_a_new_document() {
    let fx = checkout().await;
    let (archive, moved, fresh) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let mut archive_row = live_document(archive, b"zip bytes", "closed_repository.zip");
    archive_row["kind"] = "closed_repository".into();
    let mut inventory = vec![archive_row];
    inventory.extend(live_chain("old/moved.pdf", &[(moved, FILED)]));
    inventory.extend(live_chain("never/pointed.pdf", &[(fresh, FILED)]));
    mount_inventory(&fx.server, fx.project_id, inventory).await;
    let mut archive_pointer = committed_pointer(archive, b"zip bytes", 1, None, "internal");
    archive_pointer["kind"] = "closed_repository".into();
    commit_then_remove_pointer(fx.root.path(), "closed_repository.zip", &archive_pointer);
    commit_then_remove_pointer(
        fx.root.path(),
        "old/moved.pdf",
        &committed_pointer(moved, FILED, 1, None, "internal"),
    );
    // The same pointer reappears at a new path: a move, not a deletion.
    write_pair(
        fx.root.path(),
        "new/moved.pdf",
        None,
        &committed_pointer(moved, FILED, 1, None, "internal"),
    );

    let (output, stdout, _) = fx.sync(&["--dry-run"]).await;

    assert!(output.status.success(), "{stdout}");
    assert!(!stdout.contains("soft-delete"), "{stdout}");
    assert!(
        stdout.contains("would add documents/closed_repository.zip"),
        "the archive's pointer is written back: {stdout}"
    );
    assert!(
        stdout.contains("cannot reconcile documents/old/moved.pdf: its pointer moved"),
        "{stdout}"
    );
    assert!(
        stdout.contains("would add documents/never/pointed.pdf"),
        "{stdout}"
    );
}

/// ENG-860: the folder decides the kind of a new file `project sync` uploads.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_infers_the_kind_of_a_new_file_from_its_folder() {
    let fx = checkout().await;
    mount_inventory(&fx.server, fx.project_id, Vec::new()).await;
    mount_revision_receipt(
        &fx.server,
        fx.project_id,
        Uuid::now_v7(),
        b"an agreement",
        1,
        None,
    )
    .await;
    write(
        fx.root.path(),
        "documents/agreements/nda.pdf",
        b"an agreement",
    );

    let (output, stdout, stderr) = fx.sync(&["--yes"]).await;
    assert!(output.status.success(), "{stdout}{stderr}");
    let posts = writes(&fx.server).await;
    let body: serde_json::Value = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(body["kind"], "agreement");
}

/// ENG-860: an invoice whose filename breaks the rule is reported and never
/// uploaded.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_refuses_an_invoice_filename_that_breaks_the_rule() {
    let fx = checkout().await;
    mount_inventory(&fx.server, fx.project_id, Vec::new()).await;
    write(fx.root.path(), "documents/invoices/bill.pdf", b"an invoice");

    let (output, stdout, stderr) = fx.sync(&["--yes"]).await;
    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("skip "), "{stdout}");
    assert!(writes(&fx.server).await.is_empty());
}

/// ENG-860: a capture under `documents/cases/**` with its `.authority.yaml`
/// sidecar becomes an Authority through `project sync`, not a Project document.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_files_a_case_capture_as_an_authority() {
    let fx = checkout().await;
    let authority_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    let bytes = b"synthetic capture bytes";
    mount_inventory(&fx.server, fx.project_id, Vec::new()).await;
    write(fx.root.path(), "documents/cases/roe.html", bytes);
    write(
        fx.root.path(),
        "documents/cases/roe.html.authority.yaml",
        "class: case_law\n\
         citation: \"410 U.S. 113 (1973)\"\n\
         title: Roe v. Wade\n\
         canonical_url: https://example.test/roe\n\
         checked_on: \"2026-09-05\"\n",
    );
    Mock::given(method("POST"))
        .and(path("/app/api/authorities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": authority_id,
            "class": "case_law",
            "citation": "410 U.S. 113 (1973)",
            "short_cite": null,
            "title": "Roe v. Wade",
            "publisher": null,
            "issued_on": null,
            "canonical_url": "https://example.test/roe",
            "checked_on": "2026-09-05",
            "archived_asset_id": asset_id,
            "inserted_at": "2026-09-16T00:00:00Z",
            "updated_at": "2026-09-16T00:00:00Z",
        })))
        .expect(1)
        .mount(&fx.server)
        .await;

    let (output, stdout, stderr) = fx.sync(&["--yes"]).await;
    assert!(output.status.success(), "{stdout}{stderr}");
    let pointer = fx.pointer("cases/roe.html");
    assert_eq!(pointer.authority_id, Some(authority_id));
    assert_eq!(pointer.kind, "");
    assert_eq!(fx.local("cases/roe.html"), bytes);
    assert!(!fx
        .root
        .path()
        .join("documents/cases/roe.html.authority.yaml")
        .exists());
    let (again, stdout, stderr) = fx.sync(&["--yes"]).await;
    assert!(again.status.success(), "{stdout}{stderr}");
    assert!(
        writes(&fx.server)
            .await
            .iter()
            .all(|request| request.url.path() == "/app/api/authorities"),
        "no Project document was filed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_sync_files_only_the_selected_authority_and_preserves_other_bytes() {
    let fx = checkout().await;
    mount_inventory(&fx.server, fx.project_id, Vec::new()).await;
    write(
        fx.root.path(),
        "documents/cases/example.html",
        b"case capture",
    );
    write(
        fx.root.path(),
        "documents/cases/example.html.authority.yaml",
        "class: case_law\ncitation: '10 U.S. 10'\ntitle: Example\n",
    );
    write(
        fx.root.path(),
        "documents/drafts/build.html",
        b"ignored build bytes",
    );
    let authority_id = Uuid::now_v7();
    let asset_id = Uuid::now_v7();
    Mock::given(method("POST"))
        .and(path("/app/api/authorities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": authority_id, "class": "case_law", "citation": "10 U.S. 10",
            "short_cite": null, "title": "Example", "publisher": null,
            "issued_on": null, "canonical_url": null, "checked_on": null,
            "archived_asset_id": asset_id,
            "inserted_at": "2026-09-16T00:00:00Z", "updated_at": "2026-09-16T00:00:00Z"
        })))
        .expect(1)
        .mount(&fx.server)
        .await;
    let (output, stdout, stderr) = fx.sync(&["--only", "documents/cases", "--yes"]).await;
    assert!(output.status.success(), "{stdout}{stderr}");
    assert_eq!(
        fx.pointer("cases/example.html").authority_id,
        Some(authority_id)
    );
    assert!(!fx
        .root
        .path()
        .join("documents/cases/example.html.authority.yaml")
        .exists());
    assert_eq!(fx.local("drafts/build.html"), b"ignored build bytes");
    assert!(!fx.root.path().join("blobs/drafts/build.html.yaml").exists());
    assert_eq!(writes(&fx.server).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_sync_rejects_traversal_before_network_or_writes() {
    let fx = checkout().await;
    let (output, _, stderr) = fx.sync(&["--only", "documents/../outside", "--yes"]).await;
    assert!(!output.status.success());
    assert!(stderr.contains("traversal"), "{stderr}");
    assert!(fx.server.received_requests().await.unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn scoped_sync_rejects_a_symlinked_selected_directory() {
    let fx = checkout().await;
    let outside = TempDir::new().unwrap();
    fs::create_dir_all(fx.root.path().join("documents")).unwrap();
    fs::create_dir_all(fx.root.path().join("blobs")).unwrap();
    std::os::unix::fs::symlink(outside.path(), fx.root.path().join("documents/cases")).unwrap();
    let (output, _, stderr) = fx.sync(&["--only", "documents/cases", "--yes"]).await;
    assert!(!output.status.success());
    assert!(stderr.contains("symlink"), "{stderr}");
    assert!(fx.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_sync_downloads_only_selected_live_document() {
    let fx = checkout().await;
    let selected_id = Uuid::now_v7();
    let outside_id = Uuid::now_v7();
    let mut inventory = live_chain("cases/selected.pdf", &[(selected_id, FILED)]);
    inventory.extend(live_chain("drafts/outside.pdf", &[(outside_id, NEWER)]));
    mount_inventory(&fx.server, fx.project_id, inventory).await;
    mount_download(&fx.server, selected_id, FILED).await;
    write_pair(
        fx.root.path(),
        "drafts/outside.pdf",
        Some(EDITED),
        &committed_pointer(outside_id, FILED, 1, None, "internal"),
    );
    let outside_pointer = fs::read(fx.root.path().join("blobs/drafts/outside.pdf.yaml")).unwrap();

    let (output, stdout, stderr) = fx.sync(&["--only", "documents/cases", "--yes"]).await;
    assert!(output.status.success(), "{stdout}{stderr}");
    assert_eq!(fx.local("cases/selected.pdf"), FILED);
    assert_eq!(fx.local("drafts/outside.pdf"), EDITED);
    assert_eq!(
        fs::read(fx.root.path().join("blobs/drafts/outside.pdf.yaml")).unwrap(),
        outside_pointer
    );
    assert!(writes(&fx.server).await.is_empty());
    assert!(fx
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|request| !request.url.path().contains(&outside_id.to_string())));
}

/// An Authority capture is filed only with a sidecar whose class belongs in
/// its folder. A missing sidecar, case law under `rules/`, and a statute under
/// `cases/` are each reported and never uploaded.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_reports_a_missing_or_misfiled_authority_sidecar() {
    let fx = checkout().await;
    mount_inventory(&fx.server, fx.project_id, Vec::new()).await;
    let root = fx.root.path();
    write(root, "documents/cases/bare.html", b"synthetic capture");
    write(root, "documents/rules/roe.html", b"synthetic capture");
    write(
        root,
        "documents/rules/roe.html.authority.yaml",
        "class: case_law\ncitation: 410 U.S. 113 (1973)\ntitle: Roe v. Wade\n",
    );
    write(
        root,
        "documents/cases/nrs-86-201.html",
        b"synthetic capture",
    );
    write(
        root,
        "documents/cases/nrs-86-201.html.authority.yaml",
        "class: statute\ncitation: NRS 86.201\ntitle: Nevada LLC formation\n",
    );

    let (_, stdout, stderr) = fx.sync(&["--yes"]).await;
    let report = format!("{stdout}{stderr}");
    assert!(report.contains("needs a sidecar"), "{report}");
    assert!(
        report.contains("belongs under documents/cases/, not documents/rules/"),
        "{report}"
    );
    assert!(
        report.contains("belongs under documents/rules/, not documents/cases/"),
        "{report}"
    );
    assert!(writes(&fx.server).await.is_empty(), "nothing was filed");
}

/// Upload receipts are checked before a pointer is written: a receipt with
/// the wrong digest, the wrong byte count, or no pointer at all leaves the
/// local bytes in place and writes no pointer.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_rejects_a_receipt_that_does_not_match_the_uploaded_bytes() {
    let size = i64::try_from(FILED.len()).unwrap();
    for receipt in [
        ResponseTemplate::new(201).set_body_json(pointer_with_sha(
            Uuid::now_v7(),
            &"0".repeat(64),
            size,
        )),
        ResponseTemplate::new(201).set_body_json(pointer_with_sha(
            Uuid::now_v7(),
            &sha256(FILED),
            size + 1,
        )),
        ResponseTemplate::new(201).set_body_string("not a pointer"),
    ] {
        let fx = checkout().await;
        mount_inventory(&fx.server, fx.project_id, Vec::new()).await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{}/documents",
                fx.project_id
            )))
            .respond_with(receipt)
            .expect(1)
            .mount(&fx.server)
            .await;
        write(fx.root.path(), "documents/pleadings/receipt.pdf", FILED);

        let (output, stdout, stderr) = fx.sync(&["--yes"]).await;
        assert!(!output.status.success(), "{stdout}{stderr}");
        assert_eq!(fx.local("pleadings/receipt.pdf"), FILED);
        assert!(!fx
            .root
            .path()
            .join("blobs/pleadings/receipt.pdf.yaml")
            .exists());
    }
}

/// Downloads publish as one transaction: when the second target cannot be
/// written, the first is rolled back, and a retry publishes both.
#[tokio::test(flavor = "multi_thread")]
#[cfg(unix)]
async fn project_sync_rolls_back_every_download_when_one_cannot_be_published() {
    let fx = checkout().await;
    let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
    let mut inventory = live_chain("a/first.pdf", &[(first, FILED)]);
    inventory.extend(live_chain("b/second.pdf", &[(second, NEWER)]));
    mount_inventory(&fx.server, fx.project_id, inventory).await;
    mount_download(&fx.server, first, FILED).await;
    mount_download(&fx.server, second, NEWER).await;
    let root = fx.root.path();
    write_pair(
        root,
        "a/first.pdf",
        None,
        &committed_pointer(first, FILED, 1, None, "internal"),
    );
    write_pair(
        root,
        "b/second.pdf",
        None,
        &committed_pointer(second, NEWER, 1, None, "internal"),
    );
    let locked = root.join("documents/Docket Filings/b");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

    let (failed, stdout, stderr) = fx.sync(&["--yes"]).await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!failed.status.success(), "{stdout}{stderr}");
    assert!(
        !root.join("documents/Docket Filings/a/first.pdf").exists(),
        "rolled back"
    );
    assert!(!root.join("documents/Docket Filings/b/second.pdf").exists());

    let (retry, stdout, stderr) = fx.sync(&["--yes"]).await;
    assert!(retry.status.success(), "{stdout}{stderr}");
    assert_eq!(fx.local("a/first.pdf"), FILED);
    assert_eq!(fx.local("b/second.pdf"), NEWER);
}

/// A `documents/` that is a symlink out of the checkout is refused, and the
/// directory it points at is left exactly as it was.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_refuses_a_symlinked_documents_root() {
    let fx = checkout().await;
    mount_inventory(&fx.server, fx.project_id, Vec::new()).await;
    let outside = TempDir::new().unwrap();
    write(outside.path(), "pleadings/outside.pdf", b"outside bytes");
    symlink(outside.path(), fx.root.path().join("documents")).unwrap();

    let (output, stdout, stderr) = fx.sync(&["--yes"]).await;
    assert!(!output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("symlink"), "{stderr}");
    assert_eq!(
        fs::read(outside.path().join("pleadings/outside.pdf")).unwrap(),
        b"outside bytes"
    );
    assert!(!outside.path().join(".gitignore").exists());
    assert!(writes(&fx.server).await.is_empty());
}

/// `project sync` is the one document verb; the retired `site` spellings are
/// refused by the parser instead of running.
#[test]
fn site_sync_and_site_pull_are_gone() {
    for verb in ["sync", "pull"] {
        navigator()
            .args(["site", verb, "--dry-run"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

/// LAW-189: a fresh checkout gets every matter folder under `documents/`,
/// and `blobs/`, even with no documents.
/// LAW-215: a fresh checkout gets the extended matter folder tree, which
/// creates `Discovery (Served)` and the seven `Work Product` subfolders and
/// leaves the bare `Discovery` folder to a migration.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_gives_a_fresh_checkout_the_matter_folder_tree() {
    let fx = checkout().await;
    mount_inventory(&fx.server, fx.project_id, Vec::new()).await;

    let (output, stdout, stderr) = fx.sync(&[]).await;

    assert!(output.status.success(), "{stdout}{stderr}");
    let root = fx.root.path();
    for folder in [
        "documents/Client Production",
        "documents/Correspondence (Served)",
        "documents/Discovery (Served)",
        "documents/Docket Filings",
        "documents/Court Rules",
        "documents/Engagement",
        "documents/Work Product",
        "documents/Work Product/Client Calls (Work Product)",
        "documents/Work Product/Correspondence (Work Product)",
        "documents/Work Product/Discovery (Work Product)",
        "documents/Work Product/Motions (Work Product)",
        "documents/Work Product/Pleadings (Work Product)",
        "documents/Work Product/Research (Work Product)",
        "documents/Work Product/Stipulations (Work Product)",
        "blobs",
    ] {
        assert!(root.join(folder).is_dir(), "missing {folder}/");
    }
    assert!(!root.join("documents/Discovery").exists());
    assert!(writes(&fx.server).await.is_empty());
}

/// LAW-189: a Project synced before the matter folders moves every pointer
/// to `blobs/` byte for byte and its bytes into the folder its kind names,
/// reporting each move. Nothing is uploaded, and a second run moves nothing.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_migrates_a_flat_project_without_changing_a_checksum_or_asset_id() {
    let fx = checkout().await;
    let (complaint, memo) = (Uuid::now_v7(), Uuid::now_v7());
    let mut inventory = live_chain("complaint.pdf", &[(complaint, FILED)]);
    let mut memo_live = live_document(memo, NEWER, "memos/analysis.pdf");
    memo_live["kind"] = "memo".into();
    inventory.push(memo_live);
    mount_inventory(&fx.server, fx.project_id, inventory).await;
    let root = fx.root.path();
    let complaint_pointer = committed_pointer(complaint, FILED, 1, None, "internal");
    let mut memo_pointer = committed_pointer(memo, NEWER, 1, None, "internal");
    memo_pointer["kind"] = "memo".into();
    for (slug, bytes, pointer) in [
        ("complaint.pdf", FILED, &complaint_pointer),
        ("memos/analysis.pdf", NEWER, &memo_pointer),
    ] {
        write(root, &format!("documents/{slug}"), bytes);
        write(
            root,
            &format!("documents/{slug}.yaml"),
            serde_yaml::to_string(pointer).unwrap(),
        );
    }
    write(
        root,
        "documents/.gitignore",
        "*\n!*/\n!*.yaml\n!.gitignore\n",
    );
    let before = [
        fs::read(root.join("documents/complaint.pdf.yaml")).unwrap(),
        fs::read(root.join("documents/memos/analysis.pdf.yaml")).unwrap(),
    ];

    let (output, stdout, stderr) = fx.sync(&[]).await;

    assert!(output.status.success(), "{stdout}{stderr}");
    for expected in [
        "moved 2 pointer(s) from documents/ to blobs/",
        "moved documents/complaint.pdf -> documents/Docket Filings/complaint.pdf",
        "moved documents/memos/analysis.pdf -> documents/Work Product/memos/analysis.pdf",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
    assert_eq!(
        fs::read(root.join("blobs/complaint.pdf.yaml")).unwrap(),
        before[0],
        "the pointer is moved, never rewritten"
    );
    assert_eq!(
        fs::read(root.join("blobs/memos/analysis.pdf.yaml")).unwrap(),
        before[1]
    );
    assert_eq!(
        fx.pointer("complaint.pdf").current_version.asset_id,
        complaint
    );
    assert_eq!(
        fx.pointer("complaint.pdf").current_version.sha256,
        sha256(FILED)
    );
    assert_eq!(
        fx.pointer("memos/analysis.pdf").current_version.asset_id,
        memo
    );
    assert_eq!(
        fs::read(root.join("documents/Docket Filings/complaint.pdf")).unwrap(),
        FILED
    );
    assert_eq!(
        fs::read(root.join("documents/Work Product/memos/analysis.pdf")).unwrap(),
        NEWER
    );
    assert!(
        !root.join("documents/memos").exists(),
        "the emptied folder is removed"
    );
    assert!(!root.join("documents/.gitignore").exists());
    assert!(stdout.contains("0 added, 0 updated, 2 skipped"), "{stdout}");
    assert!(
        writes(&fx.server).await.is_empty(),
        "a migration uploads nothing"
    );

    let (again, stdout, stderr) = fx.sync(&[]).await;
    assert!(again.status.success(), "{stdout}{stderr}");
    assert!(!stdout.contains("moved "), "{stdout}");
}

/// LAW-189: the offline one-shot reports every move, moves pointers
/// unchanged, and is idempotent.
#[test]
fn migrate_documents_moves_flat_pointers_offline_and_is_idempotent() {
    let root = TempDir::new().unwrap();
    manifest(root.path(), "https://navigator.example.test");
    let pointer = committed_pointer(Uuid::now_v7(), FILED, 1, None, "internal");
    write(root.path(), "documents/complaint.pdf", FILED);
    write(
        root.path(),
        "documents/complaint.pdf.yaml",
        serde_yaml::to_string(&pointer).unwrap(),
    );
    let before = fs::read(root.path().join("documents/complaint.pdf.yaml")).unwrap();
    let migrate = |extra: &[&str]| {
        String::from_utf8(
            navigator()
                .current_dir(root.path())
                .args(["project", "repository", "migrate-documents"])
                .args(extra)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
        .unwrap()
    };

    let preview = migrate(&["--dry-run"]);
    assert!(
        preview.contains("would move documents/complaint.pdf.yaml -> blobs/complaint.pdf.yaml"),
        "{preview}"
    );
    assert!(root.path().join("documents/complaint.pdf.yaml").exists());

    let first = migrate(&[]);
    assert!(first.contains("2 file(s) moved"), "{first}");
    assert_eq!(
        fs::read(root.path().join("blobs/complaint.pdf.yaml")).unwrap(),
        before
    );
    assert_eq!(
        fs::read(root.path().join("documents/Docket Filings/complaint.pdf")).unwrap(),
        FILED
    );
    assert!(root.path().join("documents/Discovery (Served)").is_dir());

    let second = migrate(&[]);
    assert!(second.contains("0 file(s) moved"), "{second}");
}

/// LAW-215: the offline one-shot refiles a five-folder checkout into the
/// extended tree — bare `Discovery` pointers onto `Discovery (Served)` with
/// their bytes, kind-default moves onto the new `Work Product` subfolders —
/// without changing an asset id or checksum, lists the bytes it cannot
/// place, and is idempotent.
#[test]
fn migrate_folders_reslugs_the_five_folder_tree_offline_and_is_idempotent() {
    let root = TempDir::new().unwrap();
    manifest(root.path(), "https://navigator.example.test");
    let mut served = committed_pointer(Uuid::now_v7(), FILED, 1, None, "internal");
    served["kind"] = "unclassified".into();
    served["folder"] = "Discovery".into();
    let mut memo = committed_pointer(Uuid::now_v7(), NEWER, 1, None, "internal");
    memo["kind"] = "memo".into();
    write(
        root.path(),
        "blobs/set-one/responses.pdf.yaml",
        serde_yaml::to_string(&served).unwrap(),
    );
    write(
        root.path(),
        "documents/Discovery/set-one/responses.pdf",
        FILED,
    );
    write(
        root.path(),
        "blobs/memo.pdf.yaml",
        serde_yaml::to_string(&memo).unwrap(),
    );
    write(root.path(), "documents/Work Product/memo.pdf", NEWER);
    // Bytes no pointer names: the migration lists them, never guesses.
    write(root.path(), "documents/Discovery/orphan.pdf", FILED);

    let migrate = |extra: &[&str]| {
        String::from_utf8(
            navigator()
                .current_dir(root.path())
                .args(["project", "repository", "migrate-folders"])
                .args(extra)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
        .unwrap()
    };

    let preview = migrate(&["--dry-run"]);
    assert!(
        preview.contains("would refile blobs/set-one/responses.pdf.yaml"),
        "{preview}"
    );
    assert!(
        preview
            .contains("would move documents/Discovery/set-one/responses.pdf -> documents/Discovery (Served)/set-one/responses.pdf"),
        "{preview}"
    );
    assert!(
        fs::read_to_string(
            root.path()
                .join("documents/Discovery/set-one/responses.pdf")
        )
        .is_ok(),
        "a dry run touches nothing"
    );
    assert!(
        fs::read_to_string(root.path().join("blobs/set-one/responses.pdf.yaml"))
            .unwrap()
            .contains("folder: Discovery\n"),
        "a dry run rewrites nothing"
    );

    let first = migrate(&[]);
    assert!(
        first.contains("1 pointer(s) refiled, 2 file(s) moved"),
        "{first}"
    );
    assert!(
        first.contains("unplaced documents/Discovery/orphan.pdf"),
        "{first}"
    );

    let refiled = fs::read_to_string(root.path().join("blobs/set-one/responses.pdf.yaml")).unwrap();
    assert!(
        !refiled.contains("folder: Discovery\n")
            && refiled.contains("folder: Discovery (Served)\n"),
        "{refiled}"
    );
    let reslug: serde_json::Value = serde_yaml::from_str(&refiled).unwrap();
    assert_eq!(
        reslug["current_version"]["asset_id"], served["current_version"]["asset_id"],
        "the asset id never changes"
    );
    assert_eq!(
        reslug["current_version"]["sha256"], served["current_version"]["sha256"],
        "the checksum never changes"
    );
    assert_eq!(
        fs::read(
            root.path()
                .join("documents/Discovery (Served)/set-one/responses.pdf")
        )
        .unwrap(),
        FILED
    );
    assert_eq!(
        fs::read(
            root.path()
                .join("documents/Work Product/Research (Work Product)/memo.pdf")
        )
        .unwrap(),
        NEWER,
        "a memo's bytes follow the kind's new default"
    );
    assert_eq!(
        fs::read(root.path().join("documents/Discovery/orphan.pdf")).unwrap(),
        FILED,
        "unplaced bytes stay where the lawyer can see them"
    );

    let second = migrate(&[]);
    assert!(
        second.contains("0 pointer(s) refiled, 0 file(s) moved"),
        "{second}"
    );
}

/// LAW-189: a file dropped into a matter folder is uploaded with that
/// folder's kind under the slug below it; its pointer lands in `blobs/`,
/// naming the folder only when the kind does not, and the bytes stay put.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_uploads_a_file_dropped_into_a_matter_folder_with_its_kind() {
    let fx = checkout().await;
    let created = Uuid::now_v7();
    mount_inventory(&fx.server, fx.project_id, Vec::new()).await;
    let bytes = b"responses to the first set";
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/api/projects/{}/documents",
            fx.project_id
        )))
        .respond_with(ResponseTemplate::new(201).set_body_json({
            let mut pointer = committed_pointer(created, bytes, 1, None, "internal");
            pointer["kind"] = "unclassified".into();
            pointer
        }))
        .mount(&fx.server)
        .await;
    write(
        fx.root.path(),
        "documents/Discovery/set-one/responses.pdf",
        bytes,
    );

    let (output, stdout, stderr) = fx.sync(&["--yes"]).await;

    assert!(output.status.success(), "{stdout}{stderr}");
    let posts = writes(&fx.server).await;
    assert_eq!(posts.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(body["slug"], "set-one/responses.pdf");
    assert_eq!(body["kind"], "unclassified");
    let pointer = fx.pointer("set-one/responses.pdf");
    assert_eq!(
        pointer.folder,
        Some(store::document_pointers::MatterFolder::Discovery)
    );
    assert_eq!(pointer.current_version.asset_id, created);
    assert_eq!(fx.local("set-one/responses.pdf"), bytes);
}

/// A file dropped into Client Production is filed as an exhibit, and its
/// pointer needs no `folder:` because the kind already names the folder.
#[tokio::test(flavor = "multi_thread")]
async fn project_sync_files_client_production_as_an_exhibit() {
    let fx = checkout().await;
    mount_inventory(&fx.server, fx.project_id, Vec::new()).await;
    let bytes = b"a client screenshot";
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/api/projects/{}/documents",
            fx.project_id
        )))
        .respond_with(ResponseTemplate::new(201).set_body_json({
            let mut pointer = committed_pointer(Uuid::now_v7(), bytes, 1, None, "internal");
            pointer["kind"] = "exhibit".into();
            pointer
        }))
        .mount(&fx.server)
        .await;
    write(
        fx.root.path(),
        "documents/Client Production/screenshot.png",
        bytes,
    );

    let (output, stdout, stderr) = fx.sync(&["--yes"]).await;

    assert!(output.status.success(), "{stdout}{stderr}");
    let posts = writes(&fx.server).await;
    let body: serde_json::Value = serde_json::from_slice(&posts[0].body).unwrap();
    assert_eq!(body["kind"], "exhibit");
    let raw = fs::read_to_string(fx.root.path().join("blobs/screenshot.png.yaml")).unwrap();
    assert!(!raw.contains("folder"), "{raw}");
}
