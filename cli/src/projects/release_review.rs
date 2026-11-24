//! Publish prepared signed release topics from immediate Project checkouts.
use super::{
    forge::{ReviewRef, ReviewRequest},
    forge_forgejo::ForgejoForge,
    forge_github::GitHubForge,
    lifecycle::{git, git_text, github_origin},
    workspace, workspace_root, workspace_skills,
};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

#[derive(Deserialize)]
pub(super) struct Source {
    archived: bool,
    default_branch: String,
}

/// The review-only operation has no merge or deployment capability.
#[async_trait]
pub(super) trait ReleaseForge: Send + Sync {
    async fn source(&self) -> Result<Source>;
    async fn draft(&self, request: ReviewRequest) -> Result<(ReviewRef, bool)>;
}

pub(crate) async fn run(dir: Option<&Path>, tag: &str) -> Result<bool> {
    crate::release::parse(tag)?;
    let root = workspace_root::selected(dir)?;
    ensure!(root.is_dir(), "workspace is not a directory");
    let mut rows = BTreeMap::new();
    let mut complete = true;
    for checkout in workspace_skills::project_directories(&root)? {
        let outcome = match review_checkout(&checkout, tag).await {
            Ok(row) => row,
            Err(err) => {
                complete = false;
                json!({"status":"error","detail":format!("{err:#}")})
            }
        };
        rows.insert(
            checkout
                .file_name()
                .context("missing Project directory name")?
                .to_string_lossy()
                .into_owned(),
            outcome,
        );
    }
    println!("{}", json!({"release": tag, "projects": rows}));
    Ok(complete)
}

async fn review_checkout(dir: &Path, tag: &str) -> Result<Value> {
    ensure!(
        dir.join(".git").exists()
            && Path::new(&git_text(dir, &["rev-parse", "--show-toplevel"])?)
                == dir.canonicalize()?,
        "the Project must have its own checkout"
    );
    let origin = git_text(dir, &["remote", "get-url", "origin"])?;
    workspace::verify_checkout_binding(dir, &origin).await?;
    let forge: Box<dyn ReleaseForge> = if github_origin(dir, &origin).await.is_some() {
        Box::new(GitHubForge::from_checkout(dir).await?)
    } else {
        Box::new(ForgejoForge::from_checkout(dir)?)
    };
    review(dir, tag, &origin, forge.as_ref()).await
}

fn pin_at(dir: &Path, sha: &str) -> Result<String> {
    let yaml = git_text(dir, &["show", &format!("{sha}:navigator.yaml")])?;
    rules::project_manifest::pinned_version(&yaml)
        .map_err(anyhow::Error::msg)?
        .context("Project manifest must name a release")
}

async fn review(dir: &Path, tag: &str, origin: &str, forge: &dyn ReleaseForge) -> Result<Value> {
    let source = forge.source().await?;
    if source.archived {
        return Ok(json!({"status":"archived"}));
    }
    let heads = git_text(dir, &["ls-remote", "--heads", "--", origin])?;
    if heads.is_empty() {
        return Ok(json!({"status":"empty"}));
    }
    let branch = format!("release/{tag}");
    let topic = format!("refs/heads/{branch}");
    let base = format!("refs/heads/{}", source.default_branch);
    ensure!(topic != base, "release topic is the default branch");
    let refs: BTreeMap<_, _> = heads
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(sha, name)| (name, sha))
        .collect();
    let base_sha = *refs
        .get(base.as_str())
        .context("default branch is missing")?;
    let remote = refs.get(topic.as_str()).copied();
    // Fetch objects without updating a checkout, bookmark, tracking ref or FETCH_HEAD.
    let fetch = |name| {
        git_text(
            dir,
            &[
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                "--refmap=",
                origin,
                name,
            ],
        )
    };
    fetch(&base)?;
    let local = git(dir, &["show-ref", "--verify", "--quiet", &topic])?;
    let local = match local.status.code() {
        Some(0) => Some(git_text(dir, &["rev-parse", "--verify", &topic])?),
        Some(1) => None,
        _ => anyhow::bail!("could not inspect the local release topic"),
    };
    let head = match (local.as_deref(), remote) {
        (Some(local), Some(remote)) if local != remote => {
            anyhow::bail!("release topic differs on origin; both revisions were preserved")
        }
        (_, Some(remote)) => {
            fetch(&topic)?;
            remote
        }
        (Some(local), None) => local,
        (None, None) if pin_at(dir, base_sha)? == tag => {
            return Ok(
                json!({"status":"base_pinned","base":source.default_branch,"base_sha":base_sha}),
            )
        }
        (None, None) => anyhow::bail!("prepare the signed change on {branch}"),
    };
    ensure!(
        pin_at(dir, head)? == tag,
        "topic manifest does not pin the selected release"
    );
    git_text(dir, &["merge-base", base_sha, head]).context("topic and base must share history")?;
    let outgoing = git_text(dir, &["rev-list", &format!("{base_sha}..{head}")])?;
    ensure!(!outgoing.is_empty(), "topic has no commits to review");
    for commit in outgoing.lines() {
        git_text(dir, &["verify-commit", commit])
            .context("verify the outgoing release signature")?;
    }
    if remote.is_none() {
        // During a dual run, origin can have multiple push URLs. A draft
        // belongs only to the verified primary and leaves its mirror alone.
        git_text(
            dir,
            &[
                "-c",
                "push.followTags=false",
                "push",
                &format!("--force-with-lease={topic}:"),
                "--",
                origin,
                &format!("{head}:{topic}"),
            ],
        )?;
    }
    let (review, draft) = forge
        .draft(ReviewRequest {
            head_branch: branch.clone(),
            base_branch: source.default_branch.clone(),
            title: format!("Update Navigator to {tag}"),
            body: String::new(),
        })
        .await?;
    ensure!(
        review.head_sha == head,
        "review head differs from the signed topic"
    );
    Ok(
        json!({"status":"review","url":review.url,"base":source.default_branch,"base_sha":base_sha,
        "branch":branch,"head":head,"draft":draft}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn both_forges_resume_a_signed_topic_after_an_interruption() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("sample");
        let origin = temp.path().join("origin.git");
        let key = temp.path().join("key");
        let key_path = key.to_str().unwrap();
        ensure!(std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f", key_path])
            .status()?
            .success());
        let signers = temp.path().join("signers");
        std::fs::write(
            &signers,
            format!(
                "test@example.com {}",
                std::fs::read_to_string(key.with_extension("pub"))?
            ),
        )?;
        git_text(
            temp.path(),
            &["init", "--bare", "-b", "main", origin.to_str().unwrap()],
        )?;
        git_text(temp.path(), &["init", "-b", "main", dir.to_str().unwrap()])?;
        for (name, value) in [
            ("gpg.format", "ssh"),
            ("user.signingkey", key_path),
            ("gpg.ssh.allowedSignersFile", signers.to_str().unwrap()),
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "true"),
        ] {
            git_text(&dir, &["config", name, value])?;
        }
        git_text(&dir, &["remote", "add", "origin", origin.to_str().unwrap()])?;
        std::fs::write(
            dir.join("navigator.yaml"),
            "version: '1.0.0'\nproject: sample\n",
        )?;
        git_text(&dir, &["add", "navigator.yaml"])?;
        git_text(&dir, &["commit", "-m", "Base"])?;
        git_text(&dir, &["push", "origin", "main"])?;
        git_text(&dir, &["switch", "-c", "release/2.0.0"])?;
        std::fs::write(
            dir.join("navigator.yaml"),
            "version: '2.0.0'\nproject: sample\n",
        )?;
        git_text(&dir, &["commit", "-am", "Release"])?;
        let mirror = temp.path().join("mirror.git");
        git_text(temp.path(), &["init", "--bare", mirror.to_str().unwrap()])?;
        for url in [&origin, &mirror] {
            git_text(
                &dir,
                &[
                    "config",
                    "--add",
                    "remote.origin.pushurl",
                    url.to_str().unwrap(),
                ],
            )?;
        }
        let head = git_text(&dir, &["rev-parse", "HEAD"])?;
        git_text(&dir, &["switch", "main"])?;
        std::fs::write(dir.join("unrelated"), "keep staged")?;
        git_text(&dir, &["add", "unrelated"])?;
        let before = git_text(&dir, &["diff", "--cached"])?;

        for github in [true, false] {
            let server = MockServer::start().await;
            let (forge, prefix): (Box<dyn ReleaseForge>, _) = if github {
                (
                    Box::new(GitHubForge::new(
                        &server.uri(),
                        "example",
                        "sample",
                        "test",
                    )?),
                    "",
                )
            } else {
                (
                    Box::new(ForgejoForge::new(
                        &server.uri(),
                        "example/sample",
                        "test".into(),
                    )?),
                    "/api/v1",
                )
            };
            let repo = format!("{prefix}/repos/example/sample");
            let pulls = format!("{repo}/pulls");
            Mock::given(method("GET"))
                .and(path(&repo))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"archived":false,"default_branch":"main"})),
                )
                .mount(&server)
                .await;
            let opened = Arc::new(Mutex::new(Vec::<Value>::new()));
            let existing = opened.clone();
            Mock::given(method("GET"))
                .and(path(&pulls))
                .respond_with(move |_: &wiremock::Request| {
                    ResponseTemplate::new(200).set_body_json(&*existing.lock().unwrap())
                })
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path(&pulls))
                .respond_with(ResponseTemplate::new(503))
                .up_to_n_times(1)
                .expect(1)
                .mount(&server)
                .await;
            let sha = head.clone();
            Mock::given(method("POST")).and(path(&pulls)).respond_with(move |request: &wiremock::Request| {
                let body: Value = request.body_json().unwrap();
                let draft = if github { body["draft"] == true } else { body["title"].as_str().unwrap().starts_with("WIP: ") };
                let pull = json!({"number":1, "html_url":"https://forge.example.test/example/sample/pulls/1",
                    "head":{"sha":sha, "ref":body["head"],"repo":{"full_name":"example/sample"}},
                    "base":{"sha":"base","ref":body["base"]}, "draft":draft,"merged":false,"mergeable":false});
                opened.lock().unwrap().push(pull.clone());
                ResponseTemplate::new(201).set_body_json(pull)
            }).expect(1).mount(&server).await;
            assert!(
                review(&dir, "2.0.0", origin.to_str().unwrap(), forge.as_ref())
                    .await
                    .is_err()
            );
            let first = review(&dir, "2.0.0", origin.to_str().unwrap(), forge.as_ref()).await?;
            let second = review(&dir, "2.0.0", origin.to_str().unwrap(), forge.as_ref()).await?;
            assert_eq!(first, second);
            assert_eq!(second["draft"], true);
            assert_eq!(second["head"], head);
            assert_eq!(git_text(&dir, &["diff", "--cached"])?, before);
            assert_eq!(git_text(&dir, &["branch", "--show-current"])?, "main");
            assert!(git_text(&mirror, &["for-each-ref", "refs/heads"])?.is_empty());
            git_text(
                &dir,
                &[
                    "push",
                    "--delete",
                    origin.to_str().unwrap(),
                    "release/2.0.0",
                ],
            )?;
        }
        Ok(())
    }
}
