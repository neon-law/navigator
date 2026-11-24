//! Publish prepared signed release topics from immediate Project checkouts.
use super::{
    forge::{ArmOutcome, Forge, ReviewRef, ReviewRequest},
    forge_forgejo::ForgejoForge,
    forge_github::GitHubForge,
    lifecycle::{git, git_text, github_origin, github_token},
    push, workspace, workspace_root, workspace_skills,
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
    full_name: String,
    private: bool,
    permissions: Option<Permissions>,
}

#[derive(Deserialize)]
struct Permissions {
    push: bool,
}

impl Source {
    pub(super) fn verify(&self, expected: &str) -> Result<()> {
        ensure!(
            self.full_name.eq_ignore_ascii_case(expected),
            "forge repository identity differs"
        );
        Ok(())
    }

    fn writable(&self) -> Result<()> {
        ensure!(self.private, "Project source must be private");
        ensure!(
            !self.archived && self.permissions.as_ref().is_some_and(|p| p.push),
            "private repository write access is unconfirmed"
        );
        Ok(())
    }
}

#[async_trait]
pub(super) trait ReleaseForge: Forge {
    async fn source(&self) -> Result<Source>;
    async fn draft(&self, request: ReviewRequest) -> Result<(ReviewRef, bool)>;
    async fn checks(&self, review: &ReviewRef, base: &str) -> Result<bool>;
}

/// The primary owns reviews until the Project's recorded authority changes.
struct Targets {
    primary: String,
    reference: String,
    destinations: Vec<String>,
}

pub(crate) async fn run(
    dir: Option<&Path>,
    tag: &str,
    forgejo_first: bool,
    land: bool,
) -> Result<bool> {
    crate::release::parse(tag)?;
    let root = workspace_root::selected(dir)?;
    ensure!(root.is_dir(), "workspace is not a directory");
    let mut rows = BTreeMap::new();
    let mut complete = true;
    for checkout in workspace_skills::project_directories(&root)? {
        let outcome = match review_checkout(&checkout, tag, forgejo_first, land).await {
            Ok(row) => {
                complete &= row.get("error").is_none();
                row
            }
            Err(err) => {
                complete = false;
                json!({"status":"unresolved","error":format!("{err:#}")})
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

async fn backend(dir: &Path, remote: &str) -> Result<Box<dyn ReleaseForge>> {
    if let Some((owner, name)) = github_origin(dir, remote).await {
        model::forgejo_governance::repository_target(&format!("{owner}/{name}"))?;
        Ok(Box::new(GitHubForge::new(
            "https://api.github.com",
            &owner,
            &name,
            &github_token()?,
        )?))
    } else {
        Ok(Box::new(ForgejoForge::from_remote(remote)?))
    }
}

async fn review_checkout(dir: &Path, tag: &str, forgejo_first: bool, land: bool) -> Result<Value> {
    ensure!(
        dir.join(".git").exists()
            && Path::new(&git_text(dir, &["rev-parse", "--show-toplevel"])?)
                == dir.canonicalize()?,
        "the Project must have its own checkout"
    );
    let origin = git_text(dir, &["remote", "get-url", "origin"])?;
    let inventory = workspace::checkout_remotes(dir).await?;
    let primary = inventory
        .origin
        .context("Project has no recorded primary")?;
    let github = github_origin(dir, &primary).await.is_some();
    let dual = forgejo_first || !github || inventory.forgejo.as_deref() == Some(&origin);
    let reference = if dual && github {
        ensure!(
            inventory.forgejo_status == "verified",
            "cutover gap: Forgejo reference is unverified"
        );
        inventory
            .forgejo
            .context("cutover gap: missing Forgejo reference URL")?
    } else {
        primary.clone()
    };
    ensure!(
        origin == primary || origin == reference,
        "checkout differs from the recorded Project repositories"
    );
    let mut destinations = vec![reference.clone()];
    if reference != primary {
        destinations.push(primary.clone());
    }
    if dual {
        for url in git_text(dir, &["remote", "get-url", "--push", "--all", "origin"])?.lines() {
            if !destinations.iter().any(|other| other == url) {
                destinations.push(url.into());
            }
        }
    }
    for url in &destinations {
        // A URL-based push can still be redirected by Git's pushInsteadOf.
        // Verify the effective endpoints before querying their provider identities.
        let remote = format!("navigator-release-{}", uuid::Uuid::now_v7());
        let config = format!("remote.{remote}.url={url}");
        for option in [vec!["--all"], vec!["--push", "--all"]] {
            let mut args = vec!["-c", &config, "remote", "get-url"];
            args.extend(option);
            args.push(&remote);
            ensure!(
                git_text(dir, &args)? == *url,
                "release destination is rewritten by Git"
            );
        }
    }
    let forge = backend(dir, &primary).await?;
    let source = forge.source().await?;
    if source.archived {
        return Ok(json!({"status":"archived"}));
    }
    source.writable()?;
    for url in destinations.iter().filter(|url| **url != primary) {
        let secondary = backend(dir, url).await?.source().await?;
        secondary.writable()?;
        ensure!(
            secondary.default_branch == source.default_branch,
            "mirror default branch differs"
        );
    }
    let targets = Targets {
        primary,
        reference,
        destinations,
    };
    let mut row = review(dir, tag, &targets, forge.as_ref(), &source, land)
        .await
        .unwrap_or_else(|err| json!({"status":"unresolved","error":format!("{err:#}")}));
    row["cutover_gap"] = json!(targets.primary != targets.reference);
    row["review_forge"] = json!(if github { "github" } else { "forgejo" });
    row["reference_forge"] = json!(if dual { "forgejo" } else { "github" });
    Ok(row)
}

fn pin_at(dir: &Path, sha: &str) -> Result<String> {
    let yaml = git_text(dir, &["show", &format!("{sha}:navigator.yaml")])?;
    rules::project_manifest::pinned_version(&yaml)
        .map_err(anyhow::Error::msg)?
        .context("Project manifest must name a release")
}

async fn remote_ref(dir: &Path, url: &str, name: &str) -> Result<Option<String>> {
    let output = workspace::git(dir, &["ls-remote", "--refs", "--", url, name]).await?;
    if output.trim().is_empty() {
        return Ok(None);
    }
    let (sha, found) = output.trim().split_once('\t').context("malformed ref")?;
    ensure!(
        found == name && matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid advertised ref"
    );
    Ok(Some(sha.to_owned()))
}

async fn fetch(dir: &Path, url: &str, name: &str) -> Result<()> {
    workspace::git(
        dir,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--refmap=",
            "--",
            url,
            name,
        ],
    )
    .await?;
    Ok(())
}

async fn proposal(dir: &Path, targets: &Targets, topic: &str) -> Result<Option<String>> {
    let local = git(dir, &["show-ref", "--verify", "--quiet", topic])?;
    let mut head = match local.status.code() {
        Some(0) => Some(git_text(dir, &["rev-parse", "--verify", topic])?),
        Some(1) => None,
        _ => anyhow::bail!("could not inspect the local release topic"),
    };
    for url in &targets.destinations {
        if let Some(remote) = remote_ref(dir, url, topic).await? {
            if let Some(head) = &head {
                ensure!(
                    head == &remote,
                    "divergent release topic; remote refs were preserved"
                );
            } else {
                fetch(dir, url, topic).await?;
                head = Some(remote);
            }
        }
    }
    Ok(head)
}

async fn verify_bases(dir: &Path, targets: &Targets, base: &str, base_sha: &str) -> Result<()> {
    for url in &targets.destinations {
        ensure!(
            remote_ref(dir, url, base).await?.as_deref() == Some(base_sha),
            "reference and mirror base revisions differ"
        );
    }
    Ok(())
}

async fn publish(
    dir: &Path,
    targets: &Targets,
    topic: &str,
    head: &str,
    base: &str,
    base_sha: &str,
) -> Result<()> {
    verify_bases(dir, targets, base, base_sha).await?;
    for url in &targets.destinations {
        if remote_ref(dir, url, topic).await?.is_none() {
            workspace::git(
                dir,
                &[
                    "-c",
                    "http.followRedirects=false",
                    "push",
                    "--no-follow-tags",
                    &format!("--force-with-lease={topic}:"),
                    "--",
                    url,
                    &format!("{head}:{topic}"),
                ],
            )
            .await?;
        }
    }
    for url in &targets.destinations {
        ensure!(
            remote_ref(dir, url, topic).await?.as_deref() == Some(head),
            "published release topic differs from the signed proposal"
        );
    }
    Ok(())
}

async fn checks(forge: &dyn ReleaseForge, review: &ReviewRef, base: &str) -> Result<()> {
    tokio::time::timeout(push::CHECK_TIMEOUT, async {
        while !forge.checks(review, base).await? {
            tokio::time::sleep(push::CHECK_POLL).await;
        }
        Ok(())
    })
    .await
    .context("required checks remain unconfirmed")?
}

async fn landed(
    dir: &Path,
    targets: &Targets,
    forge: &dyn ReleaseForge,
    review: &ReviewRef,
    base: &str,
) -> Result<Value> {
    let merge = if let Some(merge) = forge.merge_commit(review).await? {
        merge
    } else {
        match forge.arm_merge(review).await? {
            ArmOutcome::MergedNow { merge_commit } => merge_commit,
            _ => push::watch_check(forge, review, tokio::time::sleep).await?,
        }
    };
    ensure!(
        forge.merge_commit(review).await?.as_deref() == Some(&merge),
        "authoritative merge is unconfirmed"
    );
    let deployment = push::watch_deployment(
        forge,
        &merge,
        std::time::Instant::now() + push::CHECK_TIMEOUT,
        tokio::time::sleep,
    )
    .await?;
    ensure!(
        deployment.commit == merge && deployment.conclusion.as_deref() == Some("success"),
        "deployment {} for {merge} did not succeed",
        deployment.url
    );
    let main = format!("refs/heads/{base}");
    for url in &targets.destinations {
        ensure!(
            remote_ref(dir, url, &main).await?.as_deref() == Some(&merge),
            "main parity unconfirmed for {merge}"
        );
    }
    Ok(json!({"merge":merge, "deployment":deployment,"main":merge}))
}

async fn review(
    dir: &Path,
    tag: &str,
    targets: &Targets,
    forge: &dyn ReleaseForge,
    source: &Source,
    land: bool,
) -> Result<Value> {
    let base = format!("refs/heads/{}", source.default_branch);
    let Some(base_sha) = remote_ref(dir, &targets.reference, &base).await? else {
        for url in &targets.destinations {
            let heads = workspace::git(dir, &["ls-remote", "--heads", "--", url]).await?;
            ensure!(
                heads.trim().is_empty(),
                "reference default branch is missing while a destination has branches"
            );
        }
        return Ok(json!({"status":"empty"}));
    };
    let branch = format!("release/{tag}");
    let topic = format!("refs/heads/{branch}");
    ensure!(topic != base, "release topic is the default branch");
    fetch(dir, &targets.reference, &base).await?;
    let Some(head) = proposal(dir, targets, &topic).await? else {
        verify_bases(dir, targets, &base, &base_sha).await?;
        ensure!(
            !land && pin_at(dir, &base_sha)? == tag,
            "prepare the signed change on {branch}"
        );
        return Ok(
            json!({"status":"base_pinned","base":source.default_branch,"base_sha":base_sha}),
        );
    };
    ensure!(
        pin_at(dir, &head)? == tag,
        "topic manifest does not pin the selected release"
    );
    git_text(dir, &["verify-commit", &head]).context("verify the signed release topic")?;
    let merged = if land {
        forge.find_merged_review(&branch, &head).await?
    } else {
        None
    };
    let (review, draft) = if let Some(merged) = merged {
        (merged, false)
    } else {
        git_text(dir, &["merge-base", &base_sha, &head])
            .context("topic and base must share history")?;
        let outgoing = git_text(dir, &["rev-list", &format!("{base_sha}..{head}")])?;
        ensure!(!outgoing.is_empty(), "topic has no commits to review");
        for commit in outgoing.lines() {
            git_text(dir, &["verify-commit", commit])
                .context("verify the outgoing release signature")?;
        }
        publish(dir, targets, &topic, &head, &base, &base_sha).await?;
        forge
            .draft(ReviewRequest {
                head_branch: branch.clone(),
                base_branch: source.default_branch.clone(),
                title: format!("Update Navigator to {tag}"),
                body: String::new(),
            })
            .await?
    };
    ensure!(
        review.head_sha == head,
        "review head differs from the signed topic"
    );
    let mut row = json!({"status":"review","url":review.url,"base":source.default_branch,"base_sha":base_sha,"branch":branch,"head":head,"draft":draft});
    if let Err(err) = checks(forge, &review, &source.default_branch).await {
        row["error"] = json!(format!("required checks for {head}: {err:#}"));
    } else {
        row["checks"] = json!("passed");
        if land {
            if draft {
                row["error"] = json!("mark the draft ready through the authoritative review");
            } else {
                match landed(dir, targets, forge, &review, &source.default_branch).await {
                    Ok(delivery) => row["delivery"] = delivery,
                    Err(err) => row["error"] = json!(format!("{err:#}")),
                }
            }
        }
    }
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn signed_topic(root: &Path, dir: &Path, origin: &Path, mirror: &Path) -> Result<String> {
        let key = root.join("key");
        let key_path = key.to_str().unwrap();
        ensure!(std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f", key_path])
            .status()?
            .success());
        let signers = root.join("signers");
        std::fs::write(
            &signers,
            format!(
                "test@example.com {}",
                std::fs::read_to_string(key.with_extension("pub"))?
            ),
        )?;
        git_text(
            root,
            &["init", "--bare", "-b", "main", origin.to_str().unwrap()],
        )?;
        git_text(root, &["init", "-b", "main", dir.to_str().unwrap()])?;
        for (name, value) in [
            ("gpg.format", "ssh"),
            ("user.signingkey", key_path),
            ("gpg.ssh.allowedSignersFile", signers.to_str().unwrap()),
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "true"),
        ] {
            git_text(dir, &["config", name, value])?;
        }
        git_text(dir, &["remote", "add", "origin", origin.to_str().unwrap()])?;
        std::fs::write(
            dir.join("navigator.yaml"),
            "version: '1.0.0'\nproject: sample\n",
        )?;
        git_text(dir, &["add", "navigator.yaml"])?;
        git_text(dir, &["commit", "-m", "Base"])?;
        git_text(dir, &["push", "origin", "main"])?;
        git_text(dir, &["switch", "-c", "release/2.0.0"])?;
        std::fs::write(
            dir.join("navigator.yaml"),
            "version: '2.0.0'\nproject: sample\n",
        )?;
        git_text(dir, &["commit", "-am", "Release"])?;
        git_text(root, &["init", "--bare", mirror.to_str().unwrap()])?;
        for url in [origin, mirror] {
            git_text(
                dir,
                &[
                    "config",
                    "--add",
                    "remote.origin.pushurl",
                    url.to_str().unwrap(),
                ],
            )?;
        }
        let head = git_text(dir, &["rev-parse", "HEAD"])?;
        git_text(dir, &["switch", "main"])?;
        std::fs::write(dir.join("unrelated"), "keep staged")?;
        git_text(dir, &["add", "unrelated"])?;
        Ok(head)
    }

    async fn get_json(server: &MockServer, route: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn mock_review(server: &MockServer, prefix: &str, github: bool, head: &str) {
        let repo = format!("{prefix}/repos/example/sample");
        let pulls = format!("{repo}/pulls");
        get_json(
            server,
            &repo,
            json!({
                "archived":false, "default_branch":"main", "full_name":"example/sample",
                "private":true, "permissions":{"push":true}
            }),
        )
        .await;
        let opened = Arc::new(Mutex::new(Vec::<Value>::new()));
        let existing = opened.clone();
        Mock::given(method("GET"))
            .and(path(&pulls))
            .respond_with(move |_: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_json(&*existing.lock().unwrap())
            })
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path(&pulls))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .expect(1)
            .mount(server)
            .await;
        let pull = json!({
            "number":1,"html_url":"https://forge.example.test/pull/1",
            "head":{"sha":head,"ref":"release/2.0.0","repo":{"full_name":"example/sample"}},
            "base":{"sha":"base","ref":"main"},"draft":true,"merged":false,"mergeable":false
        });
        get_json(server, &format!("{pulls}/1"), pull.clone()).await;
        if github {
            get_json(server,&format!("{repo}/rules/branches/main"),json!([{
                "type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci"}]}
            }])).await;
            get_json(
                server,
                &format!("{repo}/commits/{head}/check-runs"),
                json!({"check_runs":[{
                    "id":1,"name":"ci","head_sha":head,"status":"completed","conclusion":"success"
                }]}),
            )
            .await;
        } else {
            get_json(server, &format!("{repo}/branch_protections/main"),json!({
                "enable_status_check":true,"status_check_contexts":[model::forgejo_governance::REQUIRED_CHECK_CONTEXT]
            })).await;
            get_json(server,&format!("{repo}/commits/{head}/status"),json!({
                "sha":head,"statuses":[{"id":1,"context":model::forgejo_governance::REQUIRED_CHECK_CONTEXT,"status":"success"}]
            })).await;
        }
        Mock::given(method("POST"))
            .and(path(&pulls))
            .respond_with(move |request: &wiremock::Request| {
                let body: Value = request.body_json().unwrap();
                let mut response = pull.clone();
                response["head"]["ref"] = body["head"].clone();
                response["base"]["ref"] = body["base"].clone();
                response["draft"] = json!(if github {
                    body["draft"] == true
                } else {
                    body["title"].as_str().unwrap().starts_with("WIP: ")
                });
                opened.lock().unwrap().push(response.clone());
                ResponseTemplate::new(201).set_body_json(response)
            })
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_mirror_requires_affirmative_identity_privacy_and_write_access() -> Result<()> {
        for (name, private, push) in [
            ("other/sample", true, Some(true)),
            ("example/sample", false, Some(true)),
            ("example/sample", true, None),
            ("example/sample", true, Some(false)),
        ] {
            let server = MockServer::start().await;
            let forge = ForgejoForge::new(&server.uri(), "example/sample", "test".into())?;
            get_json(
                &server,
                "/api/v1/repos/example/sample",
                json!({
                    "archived":false,"default_branch":"main","full_name":name,"private":private,
                    "permissions":push.map(|allowed| json!({"push":allowed}))
                }),
            )
            .await;
            assert!(forge
                .source()
                .await
                .and_then(|source| source.writable())
                .is_err());
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_landing_requires_deployment_and_both_main_revisions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("sample");
        let origin = temp.path().join("origin.git");
        let mirror = temp.path().join("mirror.git");
        let head = signed_topic(temp.path(), &dir, &origin, &mirror)?;
        let targets = Targets {
            primary: origin.to_str().unwrap().into(),
            reference: mirror.to_str().unwrap().into(),
            destinations: vec![
                origin.to_str().unwrap().into(),
                mirror.to_str().unwrap().into(),
            ],
        };
        let merge = git_text(&dir, &["rev-parse", "main"])?;
        let review = ReviewRef {
            number: 1,
            head_sha: head.clone(),
            url: "https://example.test/pull/1".into(),
        };
        let server = MockServer::start().await;
        let forge = ForgejoForge::new(&server.uri(), "example/sample", "test".into())?;
        let repo = "/api/v1/repos/example/sample";
        get_json(&server, &format!("{repo}/pulls/1"),json!({
            "number":1,"html_url":review.url,"head":{"sha":head,"ref":"release/2.0.0"},
            "base":{"sha":merge,"ref":"main"},"draft":false,"merged":true,"merge_commit_sha":merge,"mergeable":false
        })).await;
        let run = |status| {
            json!({"workflow_runs":[{"id":1,"index_in_repo":1,
            "commit_sha":merge,"workflow_id":"cd.yml","event":"push","html_url":"https://example.test/runs/1","status":status}]})
        };
        Mock::given(method("GET"))
            .and(path(format!("{repo}/actions/runs")))
            .respond_with(ResponseTemplate::new(200).set_body_json(run("failure")))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        get_json(&server, &format!("{repo}/actions/runs"), run("success")).await;
        get_json(
            &server,
            &format!("{repo}/actions/tasks"),
            json!({"workflow_runs":[]}),
        )
        .await;
        assert!(landed(&dir, &targets, &forge, &review, "main")
            .await
            .unwrap_err()
            .to_string()
            .contains("did not succeed"));
        assert!(landed(&dir, &targets, &forge, &review, "main")
            .await
            .unwrap_err()
            .to_string()
            .contains("parity"));
        git_text(&dir, &["push", mirror.to_str().unwrap(), "main"])?;
        let delivery = landed(&dir, &targets, &forge, &review, "main").await?;
        assert_eq!(delivery["main"], merge);
        assert_eq!(delivery["deployment"]["commit"], merge);
        Ok(())
    }

    #[tokio::test]
    async fn a_divergent_mirror_preserves_all_topic_refs() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("sample");
        let origin = temp.path().join("origin.git");
        let mirror = temp.path().join("mirror.git");
        signed_topic(temp.path(), &dir, &origin, &mirror)?;
        let topic = "refs/heads/release/2.0.0";
        git_text(
            &dir,
            &["push", mirror.to_str().unwrap(), &format!("main:{topic}")],
        )?;
        let targets = Targets {
            primary: origin.to_str().unwrap().into(),
            reference: mirror.to_str().unwrap().into(),
            destinations: vec![
                origin.to_str().unwrap().into(),
                mirror.to_str().unwrap().into(),
            ],
        };
        assert!(proposal(&dir, &targets, topic)
            .await
            .unwrap_err()
            .to_string()
            .contains("divergent"));
        assert_eq!(remote_ref(&dir, &targets.primary, topic).await?, None);
        assert_eq!(
            remote_ref(&dir, &targets.reference, topic).await?,
            Some(git_text(&dir, &["rev-parse", "main"])?)
        );
        Ok(())
    }

    #[tokio::test]
    async fn stale_or_inaccessible_checks_cannot_confirm_a_release() -> Result<()> {
        let server = MockServer::start().await;
        let forge = ForgejoForge::new(&server.uri(), "example/sample", "test".into())?;
        let review = ReviewRef {
            number: 1,
            head_sha: "head".into(),
            url: "https://example.test/pull/1".into(),
        };
        let repo = "/api/v1/repos/example/sample";
        Mock::given(method("GET")).and(path(format!("{repo}/pulls/1"))).respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "number":1,"html_url":review.url,"head":{"sha":"head","ref":"topic"},"base":{"sha":"main","ref":"main"},"draft":true,"merged":false,"mergeable":false
        }))).mount(&server).await;
        Mock::given(method("GET")).and(path(format!("{repo}/commits/head/status"))).respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sha":"stale","statuses":[{"context":model::forgejo_governance::REQUIRED_CHECK_CONTEXT,"status":"success"}]
        }))).up_to_n_times(1).expect(1).mount(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("{repo}/commits/head/status")))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        assert!(checks(&forge, &review, "main")
            .await
            .unwrap_err()
            .to_string()
            .contains("another commit"));
        assert!(checks(&forge, &review, "main").await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn both_forges_resume_a_signed_topic_after_an_interruption() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("sample");
        let origin = temp.path().join("origin.git");
        let mirror = temp.path().join("mirror.git");
        let head = signed_topic(temp.path(), &dir, &origin, &mirror)?;
        let before = git_text(&dir, &["diff", "--cached"])?;
        let targets = Targets {
            primary: origin.to_str().unwrap().into(),
            reference: mirror.to_str().unwrap().into(),
            destinations: vec![
                mirror.to_str().unwrap().into(),
                origin.to_str().unwrap().into(),
            ],
        };
        git_text(&dir, &["push", mirror.to_str().unwrap(), "main"])?;

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
            mock_review(&server, prefix, github, &head).await;
            let source = forge.source().await?;
            source.writable()?;
            // Recover after just the first of the topic pushes reached its destination.
            git_text(&dir, &["push", mirror.to_str().unwrap(), "release/2.0.0"])?;
            assert!(
                review(&dir, "2.0.0", &targets, forge.as_ref(), &source, false)
                    .await
                    .is_err()
            );
            let first = review(&dir, "2.0.0", &targets, forge.as_ref(), &source, false).await?;
            let second = review(&dir, "2.0.0", &targets, forge.as_ref(), &source, false).await?;
            assert_eq!(first, second);
            assert_eq!(second["draft"], true);
            assert_eq!(second["head"], head);
            assert_eq!(git_text(&dir, &["diff", "--cached"])?, before);
            assert_eq!(git_text(&dir, &["branch", "--show-current"])?, "main");
            assert_eq!(
                git_text(&mirror, &["rev-parse", "refs/heads/release/2.0.0"])?,
                head
            );
            assert_eq!(second["checks"], "passed");
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
