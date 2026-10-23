//! `navigator project import-source`: send a checkout's headless source to
//! the Project's Forgejo repository as its first commit.
//!
//! The CLI builds the archive with the same rules the server applies
//! ([`rules::source_archive`]): it reads the files Git would track (respecting
//! `.gitignore`), refuses the tree locally if any rule fails, and uploads the
//! deterministic canonical ZIP. The operation key defaults to the Project code
//! and the tree digest, so rerunning after an interruption joins the same
//! import instead of starting another.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use rules::source_archive::{self, CanonicalTree};

/// Read the files Git would track under `directory`, as UTF-8 text.
pub(crate) fn tree_from_checkout(directory: &Path) -> Result<CanonicalTree> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(directory)
        .output()
        .context("list the checkout's files with git")?;
    if !output.status.success() {
        bail!("{} is not a Git checkout", directory.display());
    }
    let mut files = BTreeMap::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let path = std::str::from_utf8(raw).context("a tracked path is not UTF-8")?;
        let text = std::fs::read_to_string(directory.join(path))
            .with_context(|| format!("{path} is not UTF-8 text"))?;
        files.insert(path.to_string(), text);
    }
    CanonicalTree::from_files(files).map_err(|error| anyhow!("{error}"))
}

/// The default operation key: the code and the first 16 hex digits of the
/// tree digest, so the same tree retries the same import.
pub(crate) fn default_operation_key(code: &str, tree: &CanonicalTree) -> String {
    format!("{code}-{}", &tree.digest()[..16])
}

/// `navigator project import-source <code> [--dir] [--operation-key] [--wait]`.
pub async fn run(
    host: Option<&str>,
    code: &str,
    directory: &Path,
    operation_key: Option<&str>,
    wait: bool,
) -> ExitCode {
    match run_inner(host, code, directory, operation_key, wait).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::from(2)
        }
    }
}

async fn run_inner(
    host: Option<&str>,
    code: &str,
    directory: &Path,
    operation_key: Option<&str>,
    wait: bool,
) -> Result<ExitCode> {
    let tree = tree_from_checkout(directory)?;
    let (base, token) = crate::remote::resolve(host)?;
    import_tree(&base, &token, code, &tree, operation_key, wait).await
}

/// Upload `tree` to the deployment at `base` and, with `wait`, poll until the
/// import commits or is refused.
async fn import_tree(
    base: &str,
    token: &str,
    code: &str,
    tree: &CanonicalTree,
    operation_key: Option<&str>,
    wait: bool,
) -> Result<ExitCode> {
    let archive = source_archive::write_canonical_zip(tree).map_err(|error| anyhow!("{error}"))?;
    let key = operation_key.map_or_else(|| default_operation_key(code, tree), str::to_string);
    let project_id = crate::projects::surfaces::resolve_project_id(base, token, code).await?;
    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{base}/app/api/projects/{project_id}/source-imports"
        ))
        .query(&[("operation_key", key.as_str())])
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/zip")
        .body(archive)
        .send()
        .await
        .context("POST source import")?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or_default();
    if status == reqwest::StatusCode::CONFLICT && body["error"] == "source_import_requires_forgejo"
    {
        println!(
            "Source import skipped: this deployment's forge is GitHub; push the checkout instead."
        );
        return Ok(ExitCode::SUCCESS);
    }
    if status == reqwest::StatusCode::CONFLICT && body["error"] == "source_already_initialized" {
        println!("Source is already initialized for {code}; nothing to import.");
        return Ok(ExitCode::SUCCESS);
    }
    if !status.is_success() {
        bail!(
            "source import refused: {status}: {}",
            body["detail"]
                .as_str()
                .or(body["error"].as_str())
                .unwrap_or("")
        );
    }
    let import_id = body["id"].as_str().unwrap_or_default().to_string();
    println!(
        "Source import {import_id} recorded ({} files, tree {}).",
        tree.files.len(),
        tree.digest()
    );
    if !wait {
        return Ok(ExitCode::SUCCESS);
    }
    let status_url = format!("{base}/app/api/projects/{project_id}/source-imports/{import_id}");
    for _ in 0..120 {
        let current: serde_json::Value = client
            .get(&status_url)
            .bearer_auth(token)
            .send()
            .await
            .context("GET source import")?
            .json()
            .await
            .context("parse source import")?;
        match current["state"].as_str() {
            Some("committed") => {
                println!(
                    "Committed {} to {}.",
                    current["commit_sha"].as_str().unwrap_or_default(),
                    current["repository_url"].as_str().unwrap_or_default()
                );
                return Ok(ExitCode::SUCCESS);
            }
            Some("refused") => bail!(
                "source import refused: {}",
                current["last_error"].as_str().unwrap_or_default()
            ),
            _ => tokio::time::sleep(Duration::from_secs(5)).await,
        }
    }
    bail!("source import {import_id} is still pending; check again later")
}

#[cfg(test)]
mod tests {
    use super::{default_operation_key, import_tree, tree_from_checkout};
    use rules::source_archive::CanonicalTree;
    use std::collections::BTreeMap;
    use std::process::{Command, ExitCode};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PROJECT: &str = "7f1c2d3e-0000-4000-8000-000000000001";
    const IMPORTS: &str = "/app/api/projects/7f1c2d3e-0000-4000-8000-000000000001/source-imports";

    /// Every file a fresh scaffold writes must be importable: a root the
    /// scaffold adds (a license text, say) has to join the headless layout.
    #[test]
    fn a_fresh_scaffold_is_a_valid_headless_source_tree() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("sample-import");
        crate::projects::repository::emit_scaffold(
            &directory,
            "sample-import",
            "navigator.example.test",
            "lawyer@example.com",
            "client@example.com",
        )
        .unwrap();
        if !directory.join(".git").exists() {
            assert!(Command::new("git")
                .arg("init")
                .arg("-q")
                .current_dir(&directory)
                .status()
                .unwrap()
                .success());
        }
        let tree = tree_from_checkout(&directory).unwrap();
        for license in ["LICENSE", "LICENSE-MIT", "LICENSE-APACHE-2"] {
            assert!(tree.files.contains_key(license), "{license} missing");
        }
    }

    fn tree() -> CanonicalTree {
        CanonicalTree::from_files(BTreeMap::from([(
            "navigator.yaml".to_string(),
            "version: 1\n".to_string(),
        )]))
        .unwrap()
    }

    /// A fake deployment that lists one visible Project and answers the
    /// import POST with `status` and `body`.
    async fn deployment(status: u16, body: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": PROJECT, "code": "matter-one" }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(IMPORTS))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        server
    }

    async fn status_poll(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(format!("{IMPORTS}/imp-1")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn an_accepted_import_records_under_the_default_operation_key() {
        let tree = tree();
        let server = deployment(202, serde_json::json!({ "id": "imp-1" })).await;
        let code = import_tree(&server.uri(), "token", "matter-one", &tree, None, false)
            .await
            .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
        let requests = server.received_requests().await.unwrap();
        let upload = requests
            .iter()
            .find(|request| request.method.as_str() == "POST")
            .unwrap();
        let key = default_operation_key("matter-one", &tree);
        assert!(upload
            .url
            .query_pairs()
            .any(|(name, value)| name == "operation_key" && value == key));
        assert_eq!(
            rules::source_archive::canonicalize(&upload.body).unwrap(),
            tree,
            "the upload is the canonical archive of the checkout"
        );
    }

    #[tokio::test]
    async fn waiting_reports_the_commit_once_the_import_lands() {
        let server = deployment(202, serde_json::json!({ "id": "imp-1" })).await;
        status_poll(
            &server,
            serde_json::json!({
                "state": "committed",
                "commit_sha": "abc123",
                "repository_url": "https://forge.example/neon-law/matter-one",
            }),
        )
        .await;
        let code = import_tree(
            &server.uri(),
            "token",
            "matter-one",
            &tree(),
            Some("k"),
            true,
        )
        .await
        .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[tokio::test]
    async fn waiting_surfaces_a_refused_import() {
        let server = deployment(202, serde_json::json!({ "id": "imp-1" })).await;
        status_poll(
            &server,
            serde_json::json!({ "state": "refused", "last_error": "tree changed" }),
        )
        .await;
        let error = import_tree(
            &server.uri(),
            "token",
            "matter-one",
            &tree(),
            Some("k"),
            true,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("tree changed"), "{error}");
    }

    #[tokio::test]
    async fn a_github_forge_or_an_initialized_source_is_a_clean_no_op() {
        for reason in [
            "source_import_requires_forgejo",
            "source_already_initialized",
        ] {
            let server = deployment(409, serde_json::json!({ "error": reason })).await;
            let code = import_tree(&server.uri(), "token", "matter-one", &tree(), None, true)
                .await
                .unwrap();
            assert_eq!(code, ExitCode::SUCCESS, "{reason}");
        }
    }

    #[tokio::test]
    async fn a_refused_upload_names_the_server_reason() {
        let server = deployment(
            422,
            serde_json::json!({ "error": "invalid_source", "detail": "drafts/a.pdf is binary" }),
        )
        .await;
        let error = import_tree(&server.uri(), "token", "matter-one", &tree(), None, false)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("drafts/a.pdf is binary"),
            "{error}"
        );
        let server = deployment(403, serde_json::json!({ "error": "forbidden" })).await;
        let error = import_tree(&server.uri(), "token", "matter-one", &tree(), None, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("forbidden"), "{error}");
    }

    #[test]
    fn a_directory_outside_git_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let error = tree_from_checkout(dir.path()).unwrap_err();
        assert!(error.to_string().contains("not a Git checkout"), "{error}");
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn a_checkout_becomes_the_tree_the_server_accepts_and_ignored_files_stay_out() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet"]);
        std::fs::write(dir.path().join("navigator.yaml"), "version: 1\n").unwrap();
        std::fs::create_dir_all(dir.path().join("drafts")).unwrap();
        std::fs::write(dir.path().join("drafts/a.md"), "a\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "notes/\n").unwrap();
        std::fs::create_dir_all(dir.path().join("notes")).unwrap();
        std::fs::write(dir.path().join("notes/private.md"), "x").unwrap();
        let tree = tree_from_checkout(dir.path()).unwrap();
        assert_eq!(
            tree.files.keys().cloned().collect::<Vec<_>>(),
            [".gitignore", "drafts/a.md", "navigator.yaml"]
        );
        let bytes = rules::source_archive::write_canonical_zip(&tree).unwrap();
        assert_eq!(rules::source_archive::canonicalize(&bytes).unwrap(), tree);
        let key = default_operation_key("matter-one", &tree);
        assert_eq!(key, default_operation_key("matter-one", &tree));
        assert!(key.starts_with("matter-one-"));

        std::fs::write(dir.path().join("drafts/scan.pdf"), b"%PDF\0binary").unwrap();
        assert!(
            tree_from_checkout(dir.path()).is_err(),
            "legal bytes never leave as source"
        );
    }

    #[test]
    fn a_plain_directory_is_refused_and_a_changed_tree_gets_a_new_key() {
        let plain = tempfile::tempdir().unwrap();
        let error = tree_from_checkout(plain.path()).unwrap_err();
        assert!(
            error.to_string().contains("is not a Git checkout"),
            "{error}"
        );

        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet"]);
        std::fs::write(dir.path().join("navigator.yaml"), "version: 1\n").unwrap();
        let before = tree_from_checkout(dir.path()).unwrap();
        std::fs::write(dir.path().join("README.md"), "# Matter\n").unwrap();
        let after = tree_from_checkout(dir.path()).unwrap();
        let key = default_operation_key("matter-one", &before);
        assert_eq!(key.len(), "matter-one-".len() + 16);
        assert_ne!(key, default_operation_key("matter-one", &after));
        assert_ne!(key, default_operation_key("matter-two", &before));
    }
}
