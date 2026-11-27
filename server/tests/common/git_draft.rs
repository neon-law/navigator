//! Populate only task-owned repositories, using the image's own Git in image CI.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, ensure, Context};
use serde_json::Value;

const REPO_ROOT: &str = "/tmp/navigator-image-repos";
const OWNER_LABEL: &str = "com.neonlaw.navigator.browser-fixture";
const OWNER_VALUE: &str = "git-runtime";
const OPERATION_LABEL: &str = "com.neonlaw.navigator.browser-operation";

pub enum DraftRepository {
    Host(repos::RepoStore),
    Image {
        container: String,
        image: String,
        volume: String,
        operation: String,
        head: Option<String>,
    },
}

fn output(command: &mut Command, input: Option<&[u8]>) -> anyhow::Result<Vec<u8>> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command
        .spawn()
        .context("start synthetic repository fixture command")?;
    if let Some(bytes) = input {
        child
            .stdin
            .take()
            .context("fixture stdin")?
            .write_all(bytes)?;
    }
    let result = child.wait_with_output()?;
    // Do not echo inspect output, container configuration or application logs.
    ensure!(
        result.status.success(),
        "synthetic repository fixture command failed: {}",
        result.status
    );
    Ok(result.stdout)
}

fn inspect(kind: &str, id: &str) -> anyhow::Result<Value> {
    let bytes = output(Command::new("docker").args([kind, "inspect", id]), None)?;
    let rows: Vec<Value> = serde_json::from_slice(&bytes)?;
    ensure!(
        rows.len() == 1,
        "fixture inspection must identify exactly one resource"
    );
    rows.into_iter().next().context("fixture inspect row")
}

fn hex(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|tail| tail.len() == 64 && tail.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn readonly_fixture_binds(mounts: &[Value], environment: &[Value]) -> anyhow::Result<()> {
    let configured = |key: &str| -> anyhow::Result<Option<&str>> {
        let mut values = environment.iter().filter_map(|entry| {
            entry
                .as_str()?
                .split_once('=')
                .filter(|(name, _)| *name == key)
                .map(|(_, value)| value)
        });
        let value = values.next();
        ensure!(
            values.next().is_none(),
            "duplicate fixture mount configuration"
        );
        Ok(value)
    };
    let certificate = configured("NAVIGATOR_POSTGRES_ROOT_CERT")?;
    let samples = configured("NAVIGATOR_SAMPLE_PROJECTS_DIR")?;
    for mount in mounts.iter().filter(|mount| mount["Type"] == "bind") {
        let destination = mount["Destination"]
            .as_str()
            .context("bind destination metadata")?;
        let sample_path = std::path::Path::new(destination);
        let samples_under_native = sample_path.starts_with("/native")
            && sample_path != std::path::Path::new("/native")
            && sample_path.components().all(|part| {
                !matches!(
                    part,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            });
        let allowed = (destination == "/native/ca.crt" && certificate == Some(destination))
            || (samples == Some(destination)
                && samples_under_native
                && destination != "/native/ca.crt");
        ensure!(
            allowed && mount["RW"] == false,
            "image fixture permits only configured read-only CA and sample binds"
        );
    }
    Ok(())
}

fn checked_operation(container: &Value, volume: &Value) -> anyhow::Result<String> {
    let operation = container["Config"]["Labels"][OPERATION_LABEL]
        .as_str()
        .context("owned image operation label")?;
    ensure!(
        !uuid::Uuid::parse_str(operation)?.is_nil(),
        "image operation must be nonnil"
    );
    ensure!(
        volume["Labels"][OPERATION_LABEL] == operation,
        "image and repository volume operations differ"
    );
    Ok(operation.to_owned())
}

fn require_restart_binding(
    current: &Value,
    container: &str,
    image: &str,
    volume: &str,
    operation: &str,
) -> anyhow::Result<()> {
    ensure!(
        current["Id"] == container && current["Image"] == image,
        "restart image identity changed"
    );
    ensure!(
        current["Config"]["Labels"][OWNER_LABEL] == OWNER_VALUE
            && current["Config"]["Labels"][OPERATION_LABEL] == operation,
        "restart image ownership changed"
    );
    ensure!(
        current["Mounts"]
            .as_array()
            .is_some_and(
                |mounts| mounts.iter().any(|mount| mount["Destination"] == REPO_ROOT
                    && mount["Type"] == "volume"
                    && mount["Name"] == volume
                    && mount["RW"] == true)
            ),
        "restart repository volume changed"
    );
    Ok(())
}

fn validate_fixture_codes(project_code: &str, draft_code: &str) -> anyhow::Result<()> {
    ensure!(
        store::projects::is_valid_code(project_code),
        "unsafe synthetic Project code"
    );
    ensure!(
        rules::is_snake_case(draft_code),
        "synthetic Draft code must be snake_case"
    );
    Ok(())
}

fn template_revision_source(revision: &str) -> String {
    let source = include_str!("../../../templates/notations/neon_law/onboarding__letter.md");
    format!("{source}\nSynthetic Git revision {revision}.\n")
}

impl DraftRepository {
    pub fn configured() -> anyhow::Result<Self> {
        let container = std::env::var("NAV_BROWSER_IMAGE_CONTAINER").ok();
        let image = std::env::var("NAV_BROWSER_IMAGE_ID").ok();
        let root = std::env::var("NAV_BROWSER_IMAGE_REPO_ROOT").ok();
        match (container, image, root) {
            (None, None, None) => Ok(Self::Host(repos::RepoStore::from_env()?)),
            (Some(container), Some(image), Some(root)) => {
                ensure!(
                    hex(&container, "") && hex(&image, "sha256:"),
                    "image fixture requires full container and immutable image IDs"
                );
                ensure!(
                    root == REPO_ROOT,
                    "image fixture repository root is not the dedicated fixture path"
                );
                let inspected = inspect("container", &container)?;
                ensure!(
                    inspected["Id"] == container && inspected["Image"] == image,
                    "fixture container identity changed"
                );
                ensure!(
                    inspected["State"]["Running"] == true,
                    "fixture container must be running"
                );
                ensure!(
                    inspected["Config"]["Labels"][OWNER_LABEL] == OWNER_VALUE,
                    "container is not owned by this fixture harness"
                );
                let configured_user = inspected["Config"]["User"].as_str().unwrap_or_default();
                ensure!(
                    matches!(configured_user, "65532:65532" | "nonroot:nonroot"),
                    "image must retain the nonroot user"
                );
                let image_metadata = inspect("image", &image)?;
                ensure!(
                    image_metadata["Os"] == "linux" && image_metadata["Architecture"] == "amd64",
                    "image fixture requires Linux/amd64"
                );
                let env = inspected["Config"]["Env"]
                    .as_array()
                    .context("container environment metadata")?;
                ensure!(
                    env.iter().any(|entry| entry.as_str()
                        == Some("NAVIGATOR_GIT_REPO_ROOT=/tmp/navigator-image-repos")),
                    "application repository root differs from fixture root"
                );
                let mounts = inspected["Mounts"]
                    .as_array()
                    .context("container mount metadata")?;
                readonly_fixture_binds(mounts, env)?;
                let mount = mounts
                    .iter()
                    .find(|mount| mount["Destination"] == REPO_ROOT)
                    .context("dedicated repository volume")?;
                ensure!(
                    mount["Type"] == "volume" && mount["RW"] == true,
                    "repository fixture requires a writable named volume"
                );
                let volume = inspect("volume", mount["Name"].as_str().context("volume name")?)?;
                ensure!(
                    volume["Labels"][OWNER_LABEL] == OWNER_VALUE,
                    "repository volume is not owned by this fixture harness"
                );
                let operation = checked_operation(&inspected, &volume)?;
                let volume_name = mount["Name"]
                    .as_str()
                    .context("owned repository volume name")?
                    .to_owned();
                let fixture = Self::Image {
                    container,
                    image,
                    volume: volume_name,
                    operation,
                    head: None,
                };
                ensure!(
                    fixture.exec(&["id", "-u"], None)? == "65532",
                    "fixture command has the wrong UID"
                );
                ensure!(
                    fixture.exec(&["stat", "-c", "%u:%g:%a", REPO_ROOT], None)?
                        == "65532:65532:700",
                    "repository volume must already be privately initialized by UID/GID65532"
                );
                Ok(fixture)
            }
            _ => bail!("partial image repository fixture configuration"),
        }
    }

    /// Only image mode proves an application restart; host mode retains ordinary readback scope.
    pub async fn restart_for_readback(&self, base: &str) -> anyhow::Result<()> {
        let Self::Image {
            container,
            image,
            volume,
            operation,
            ..
        } = self
        else {
            return Ok(());
        };
        let before = self.restart_binding()?;
        ensure!(
            before["State"]["Running"] == true,
            "restart requires the owned running image"
        );
        let started = before["State"]["StartedAt"]
            .as_str()
            .context("image start time")?
            .to_owned();
        output(
            Command::new("docker").args(["stop", "--time", "120", container]),
            None,
        )?;
        let stopped = self.restart_binding()?;
        ensure!(
            stopped["State"]["Running"] == false && stopped["State"]["ExitCode"] == 0,
            "owned image must complete graceful shutdown before restart"
        );
        output(Command::new("docker").args(["start", container]), None)?;
        self.wait_for_restarted_image(base).await?;
        let after = self.restart_binding()?;
        ensure!(
            after["State"]["StartedAt"]
                .as_str()
                .is_some_and(|value| value != started),
            "owned image must have a new actual process start"
        );
        let Self::Image {
            container: actual,
            image: actual_image,
            volume: actual_volume,
            operation: actual_operation,
            ..
        } = Self::configured()?
        else {
            bail!("image fixture context disappeared during restart");
        };
        ensure!(
            actual == *container
                && actual_image == *image
                && actual_volume == *volume
                && actual_operation == *operation,
            "restart cannot replace its container, image or owned repository volume"
        );
        Ok(())
    }

    fn restart_binding(&self) -> anyhow::Result<Value> {
        let Self::Image {
            container,
            image,
            volume,
            operation,
            ..
        } = self
        else {
            bail!("host mode has no owned image restart capability");
        };
        let current = inspect("container", container)?;
        require_restart_binding(&current, container, image, volume, operation)?;
        let mounted = inspect("volume", volume)?;
        ensure!(
            mounted["Labels"][OWNER_LABEL] == OWNER_VALUE
                && checked_operation(&current, &mounted)? == *operation,
            "restart volume ownership changed"
        );
        Ok(current)
    }

    async fn wait_for_restarted_image(&self, base: &str) -> anyhow::Result<()> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            ensure!(
                self.restart_binding()?["State"]["Running"] == true,
                "restarted image exited before readiness"
            );
            if http
                .get(format!("{base}/app/readyz"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "restarted image did not become ready"
            );
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        let version: Value = http
            .get(format!("{base}/version"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure!(
            version["commit_full"]
                == std::env::var("GITHUB_SHA").context("expected image source SHA")?
                && version["release"]
                    == std::env::var("EXPECTED_IMAGE_RELEASE").context("expected image release")?,
            "restarted image version must retain exact baked release and source identity"
        );
        Ok(())
    }

    pub fn assert_existing_revision(
        &self,
        project: &str,
        draft: &str,
        commit: &str,
        digest: &str,
    ) -> anyhow::Result<()> {
        validate_fixture_codes(project, draft)?;
        let (head, source) = match self {
            Self::Host(repo) => {
                let head = repo
                    .head_oid_code(project)?
                    .context("persisted repository HEAD")?;
                let path = format!("drafts/{draft}.md");
                let source = repo
                    .read_head_tree_code(project)?
                    .into_iter()
                    .find(|(name, _)| name == &path)
                    .context("persisted Draft source")?
                    .1;
                (head, String::from_utf8(source)?)
            }
            Self::Image { .. } => {
                self.restart_binding()?;
                let path = format!("{REPO_ROOT}/{project}.git");
                (
                    self.exec(
                        &["/usr/bin/git", "-C", &path, "rev-parse", "refs/heads/main"],
                        None,
                    )?,
                    self.read_source(&path, draft)?,
                )
            }
        };
        ensure!(
            head == commit,
            "persisted repository HEAD changed across restart/readback"
        );
        let compiled = rules::clauses::compile(&source).map_err(anyhow::Error::msg)?;
        ensure!(
            store::assets::sha256_hex(compiled.as_bytes()) == digest,
            "actual persisted repository bytes must retain the imported compiled hash"
        );
        Ok(())
    }

    fn read_source(&self, path: &str, draft: &str) -> anyhow::Result<String> {
        Ok(String::from_utf8(self.exec_bytes(
            &[
                "/usr/bin/git",
                "-C",
                path,
                "show",
                &format!("refs/heads/main:drafts/{draft}.md"),
            ],
            None,
        )?)?)
    }

    fn exec(&self, args: &[&str], input: Option<&[u8]>) -> anyhow::Result<String> {
        Ok(String::from_utf8(self.exec_bytes(args, input)?)?
            .trim()
            .to_owned())
    }

    fn exec_bytes(&self, args: &[&str], input: Option<&[u8]>) -> anyhow::Result<Vec<u8>> {
        let Self::Image { container, .. } = self else {
            bail!("image fixture command in host mode")
        };
        let mut command = Command::new("docker");
        command.args(["exec", "-i", "--user", "65532:65532"]);
        for value in [
            "GIT_CONFIG_GLOBAL=/dev/null",
            "GIT_CONFIG_SYSTEM=/dev/null",
            "GIT_AUTHOR_NAME=Synthetic Browser",
            "GIT_AUTHOR_EMAIL=synthetic-browser@example.invalid",
            "GIT_COMMITTER_NAME=Synthetic Browser",
            "GIT_COMMITTER_EMAIL=synthetic-browser@example.invalid",
        ] {
            command.args(["--env", value]);
        }
        command.arg(container).args(args);
        output(&mut command, input)
    }

    /// Commit a real template revision and hash the compiled bytes the API must import.
    pub fn commit_template_revision(
        &mut self,
        project_code: &str,
        draft_code: &str,
        revision: &str,
    ) -> anyhow::Result<(String, String)> {
        let body = template_revision_source(revision);
        let commit = self.commit(project_code, draft_code, &body)?;
        let compiled = rules::clauses::compile(&body).map_err(anyhow::Error::msg)?;
        let digest = store::assets::sha256_hex(compiled.as_bytes());
        Ok((commit, digest))
    }

    pub fn commit(
        &mut self,
        project_code: &str,
        draft_code: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        // These codes are generated by this test; never turn caller-supplied paths into Git arguments.
        validate_fixture_codes(project_code, draft_code)?;
        let relative = format!("drafts/{draft_code}.md");
        match self {
            Self::Host(repo) => Ok(repo.commit_as_code(
                project_code,
                repos::Author {
                    name: "Synthetic Browser",
                    email: "synthetic-browser@example.invalid",
                },
                "Synthetic browser Draft",
                &[(relative.as_str(), body.as_bytes())],
            )?),
            Self::Image { head, .. } => {
                let parent = head.clone();
                let path = format!("{REPO_ROOT}/{project_code}.git");
                if parent.is_none() {
                    self.exec(&["test", "!", "-e", &path], None)?;
                    self.exec(
                        &["git", "init", "--bare", "--initial-branch=main", &path],
                        None,
                    )?;
                    self.exec(&["chmod", "700", &path], None)?;
                }
                let blob = self.exec(
                    &[
                        "git",
                        "-C",
                        &path,
                        "hash-object",
                        "-w",
                        "--no-filters",
                        "--stdin",
                    ],
                    Some(body.as_bytes()),
                )?;
                let tree = self.exec(
                    &["git", "-C", &path, "mktree"],
                    Some(format!("100644 blob {blob}\t{draft_code}.md\n").as_bytes()),
                )?;
                let root = self.exec(
                    &["git", "-C", &path, "mktree"],
                    Some(format!("040000 tree {tree}\tdrafts\n").as_bytes()),
                )?;
                let mut args = vec![
                    "git",
                    "-C",
                    &path,
                    "commit-tree",
                    &root,
                    "-m",
                    "Synthetic browser Draft",
                ];
                if let Some(parent) = parent.as_deref() {
                    args.extend(["-p", parent]);
                }
                let commit = self.exec(&args, None)?;
                self.exec(
                    &["git", "-C", &path, "update-ref", "refs/heads/main", &commit],
                    None,
                )?;
                if let Self::Image { head, .. } = self {
                    *head = Some(commit.clone());
                }
                Ok(commit)
            }
        }
    }
}

pub async fn request(
    browser: &fantoccini::Client,
    path: &str,
    post: Option<(Value, &str)>,
) -> (u64, Value) {
    let (body, token) = post.map_or((Value::Null, ""), |(body, token)| (body, token));
    let result = browser
        .execute_async(
            "const [path, body, token, done] = arguments; \
         const options = {credentials: 'same-origin', redirect: 'manual'}; \
         if (body !== null) { options.method = 'POST'; \
           options.headers = {'Content-Type': 'application/json', 'X-CSRF-Token': token}; \
           options.body = JSON.stringify(body); } \
         fetch(path, options).then(async response => { \
           const text = await response.text(); let json = null; \
           try { json = JSON.parse(text); } catch (_) {} \
           done({status: response.status, body: json}); \
         }).catch(() => done({status: 0, body: null}));",
            vec![path.into(), body, token.into()],
        )
        .await
        .expect("actual same-origin browser API request");
    (
        result["status"].as_u64().expect("HTTP status"),
        result["body"].clone(),
    )
}

pub async fn assert_access_guards(
    browser: &fantoccini::Client,
    unrelated_inventory: &str,
    unrelated_create: &str,
    create: &str,
    payload: &Value,
    csrf: &str,
) {
    assert_eq!(request(browser, unrelated_inventory, None).await.0, 404);
    assert_eq!(
        request(browser, unrelated_create, Some((payload.clone(), csrf)))
            .await
            .0,
        404
    );
    assert_eq!(
        request(browser, create, Some((payload.clone(), "")))
            .await
            .0,
        403
    );
}

pub async fn assert_notation_provenance(
    browser: &fantoccini::Client,
    id: &str,
    commit: &str,
    digest: &str,
) -> Value {
    let (status, notation) = request(browser, &format!("/app/api/notations/{id}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(notation["template_source_commit_sha"], commit);
    assert_eq!(notation["template_body_sha256"], digest);
    notation
}

pub async fn assert_unrelated_project_has_no_notations(
    db: &store::application_store::ApplicationStore,
    project: uuid::Uuid,
) {
    assert!(store::notations::list_by_project(db, project)
        .await
        .unwrap()
        .is_empty());
}

pub async fn assert_retained_import(
    browser: &fantoccini::Client,
    inventory: &str,
    id: &str,
    commit: &str,
    digest: &str,
) {
    let (status, rows) = request(browser, inventory, None).await;
    assert_eq!(status, 200);
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|row| row["notation_id"] == id && row["template_stale"] == false),
        "authenticated inventory retains the exact imported revision after restart/readback"
    );
    assert_notation_provenance(browser, id, commit, digest).await;
}

pub async fn assert_stale_revision_preserved(
    browser: &fantoccini::Client,
    inventory: &str,
    id: &str,
    commit: &str,
    digest: &str,
) {
    let (status, rows) = request(browser, inventory, None).await;
    assert_eq!(status, 200);
    assert!(rows
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["notation_id"] == id && row["template_stale"] == true));
    assert_notation_provenance(browser, id, commit, digest).await;
}

pub async fn open_notation(
    browser: &fantoccini::Client,
    path: &str,
    payload: Value,
    csrf: &str,
) -> Value {
    let (status, opened) = request(browser, path, Some((payload, csrf))).await;
    assert_eq!(
        status, 201,
        "synthetic repository revision must open a Notation: {opened:?}"
    );
    opened
}

pub async fn csrf(browser: &fantoccini::Client) -> String {
    browser
        .goto(&format!("{}/app/profile", features::webdriver::base_url()))
        .await
        .unwrap();
    browser
        .wait()
        .at_most(std::time::Duration::from_secs(10))
        .for_element(fantoccini::Locator::Css(
            "#profile-avatar form input[name='_csrf']",
        ))
        .await
        .expect("authenticated profile CSRF field")
        .attr("value")
        .await
        .unwrap()
        .expect("profile CSRF token")
}

#[test]
fn image_repository_fixture_allows_only_configured_readonly_support_binds() {
    let environment = vec![
        Value::String("NAVIGATOR_POSTGRES_ROOT_CERT=/native/ca.crt".into()),
        Value::String("NAVIGATOR_SAMPLE_PROJECTS_DIR=/native/sample-projects".into()),
    ];
    let support = serde_json::json!([
        {"Type": "bind", "Destination": "/native/ca.crt", "RW": false},
        {"Type": "bind", "Destination": "/native/sample-projects", "RW": false}
    ]);
    assert!(readonly_fixture_binds(support.as_array().unwrap(), &environment).is_ok());
    for destination in [
        REPO_ROOT,
        "/tmp",
        "/",
        "/usr/bin/git",
        "/lib",
        "/workspace",
        "/native",
        "/native/../tmp",
    ] {
        let mounts = serde_json::json!([{"Type": "bind", "Destination": destination, "RW": false}]);
        assert!(
            readonly_fixture_binds(mounts.as_array().unwrap(), &environment).is_err(),
            "unexpected bind: {destination}"
        );
    }
    let writable =
        serde_json::json!([{"Type": "bind", "Destination": "/native/sample-projects", "RW": true}]);
    assert!(readonly_fixture_binds(writable.as_array().unwrap(), &environment).is_err());
    assert!(readonly_fixture_binds(support.as_array().unwrap(), &[]).is_err());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_binding_refuses_replaced_images_foreign_operations_and_changed_volumes() {
        let operation = uuid::Uuid::from_u128(1).simple().to_string();
        let current = serde_json::json!({
            "Id":"owned-container", "Image":"owned-image",
            "Config":{"Labels":{OWNER_LABEL:OWNER_VALUE, OPERATION_LABEL:operation}},
            "Mounts":[{"Destination":REPO_ROOT,"Type":"volume","Name":"owned-volume","RW":true}],
        });
        let volume =
            serde_json::json!({"Labels":{OWNER_LABEL:OWNER_VALUE,OPERATION_LABEL:operation}});
        assert_eq!(checked_operation(&current, &volume).unwrap(), operation);
        assert!(require_restart_binding(
            &current,
            "owned-container",
            "owned-image",
            "owned-volume",
            &operation
        )
        .is_ok());
        let mut changed = current.clone();
        changed["Image"] = "foreign-image".into();
        assert!(require_restart_binding(
            &changed,
            "owned-container",
            "owned-image",
            "owned-volume",
            &operation
        )
        .is_err());
        let mut changed = current.clone();
        changed["Config"]["Labels"][OPERATION_LABEL] =
            uuid::Uuid::from_u128(2).simple().to_string().into();
        assert!(require_restart_binding(
            &changed,
            "owned-container",
            "owned-image",
            "owned-volume",
            &operation
        )
        .is_err());
        let mut changed = current.clone();
        changed["Mounts"][0]["Name"] = "foreign-volume".into();
        assert!(require_restart_binding(
            &changed,
            "owned-container",
            "owned-image",
            "owned-volume",
            &operation
        )
        .is_err());
        let mut foreign = volume.clone();
        foreign["Labels"][OPERATION_LABEL] = uuid::Uuid::from_u128(2).simple().to_string().into();
        assert!(checked_operation(&current, &foreign).is_err());
        let mut missing = current;
        missing["Config"]["Labels"][OPERATION_LABEL] = Value::Null;
        assert!(checked_operation(&missing, &volume).is_err());
    }

    #[test]
    fn repository_fixture_codes_preserve_distinct_project_and_draft_contracts() {
        assert!(validate_fixture_codes("synthetic-runtime", "synthetic_git_fixture1").is_ok());
        assert!(validate_fixture_codes("synthetic_runtime", "synthetic_git_fixture1").is_err());
        assert!(validate_fixture_codes("synthetic-runtime", "synthetic-git-fixture1").is_err());
        assert!(validate_fixture_codes("../synthetic", "synthetic_git_fixture1").is_err());
        assert!(validate_fixture_codes("synthetic-runtime", "../synthetic_git_fixture1").is_err());
    }

    #[test]
    fn actual_repository_revision_passes_blocking_template_rules_before_browser_import() {
        for revision in ["one", "two"] {
            let file = rules::SourceFile {
                path: "synthetic_git_fixture1.md".into(),
                contents: template_revision_source(revision),
            };
            let violations = rules::blocking_violations_without_checkout(&file);
            assert!(
                violations.is_empty(),
                "synthetic revision {revision} must import: {violations:?}"
            );
            let mut invalid = file;
            invalid.path = "synthetic-git-fixture1.md".into();
            assert!(
                rules::blocking_violations_without_checkout(&invalid)
                    .iter()
                    .any(|violation| violation.code == "N103"),
                "the prior invalid basename must be refused by the actual template rule"
            );
        }
    }
}
