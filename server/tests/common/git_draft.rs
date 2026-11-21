//! Populate only task-owned repositories, using the image's own Git in image CI.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, ensure, Context};
use serde_json::Value;

const REPO_ROOT: &str = "/tmp/navigator-image-repos";
const OWNER_LABEL: &str = "com.neonlaw.navigator.browser-fixture";
const OWNER_VALUE: &str = "git-runtime";

pub enum DraftRepository {
    Host(repos::RepoStore),
    Image {
        container: String,
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
                let fixture = Self::Image {
                    container,
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

    fn exec(&self, args: &[&str], input: Option<&[u8]>) -> anyhow::Result<String> {
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
        Ok(String::from_utf8(output(&mut command, input)?)?
            .trim()
            .to_owned())
    }

    /// Commit a real template revision and hash the compiled bytes the API must import.
    pub fn commit_template_revision(
        &mut self,
        project_code: &str,
        draft_code: &str,
        revision: &str,
    ) -> anyhow::Result<(String, String)> {
        let source = include_str!("../../../templates/notations/neon_law/onboarding__letter.md");
        let body = format!("{source}\nSynthetic Git revision {revision}.\n");
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
        ensure!(
            [project_code, draft_code]
                .iter()
                .all(|code| !code.is_empty()
                    && code
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')),
            "unsafe synthetic fixture code"
        );
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
